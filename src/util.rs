//! Path, formatting and clipboard helpers.

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::*;
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::Memory::*;
use windows::Win32::System::Ole::{CF_DIB, CF_HDROP, CF_UNICODETEXT};

pub fn ext(p: &Path) -> String {
    p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default()
}

pub fn stem(p: &Path) -> String {
    p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "file".into())
}

pub fn name(p: &Path) -> String {
    p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| p.to_string_lossy().to_string())
}

/// First free path: "x.ext", "x (2).ext", "x (3).ext", …
pub fn unique(p: PathBuf) -> PathBuf {
    if !p.exists() {
        return p;
    }
    let dir = p.parent().map(Path::to_path_buf).unwrap_or_default();
    let st = stem(&p);
    let ex = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    (2..).map(|i| dir.join(format!("{st} ({i}){ex}"))).find(|c| !c.exists()).unwrap()
}

/// Output path next to `input` named "<stem><suffix>.<ext>", never overwriting.
pub fn sibling(input: &Path, suffix: &str, ext: &str) -> PathBuf {
    let dir = input.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut out = dir.join(format!("{}{suffix}.{ext}", stem(input)));
    if out == input {
        out = dir.join(format!("{}{suffix}_converted.{ext}", stem(input)));
    }
    unique(out)
}

pub fn human_size(n: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < units.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{n} B") } else { format!("{v:.1} {}", units[u]) }
}

pub fn plural(n: usize, word: &str) -> String {
    if n == 1 { format!("1 {word}") } else { format!("{n} {word}s") }
}

/// Walk a directory tree, returning (bytes, files, folders).
pub fn dir_stats(p: &Path) -> (u64, u64, u64) {
    let mut acc = (0u64, 0u64, 0u64);
    let mut stack = vec![p.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                acc.2 += 1;
                stack.push(e.path());
            } else {
                acc.1 += 1;
                acc.0 += e.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    acc
}

pub fn set_clipboard_text(s: &str) -> bool {
    unsafe {
        if OpenClipboard(crate::main_hwnd()).is_err() {
            return false;
        }
        let _ = EmptyClipboard();
        let w: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = w.len() * 2;
        let mut ok = false;
        if let Ok(h) = GlobalAlloc(GMEM_MOVEABLE, bytes) {
            let p = GlobalLock(h) as *mut u16;
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
                let _ = GlobalUnlock(h);
                ok = SetClipboardData(CF_UNICODETEXT.0 as u32, HANDLE(h.0)).is_ok();
            }
        }
        let _ = CloseClipboard();
        ok
    }
}

pub enum Clip {
    Text(String),
    /// RGBA pixels, width, height
    Image(Vec<u8>, u32, u32),
}

pub fn get_clipboard() -> Option<Clip> {
    unsafe {
        OpenClipboard(crate::main_hwnd()).ok()?;
        let res = (|| {
            if IsClipboardFormatAvailable(CF_DIB.0 as u32).is_ok() {
                let h = GetClipboardData(CF_DIB.0 as u32).ok()?;
                let g = HGLOBAL(h.0);
                let p = GlobalLock(g) as *const u8;
                if p.is_null() {
                    return None;
                }
                let size = GlobalSize(g);
                let data = std::slice::from_raw_parts(p, size).to_vec();
                let _ = GlobalUnlock(g);
                return dib_to_rgba(&data);
            }
            if IsClipboardFormatAvailable(CF_UNICODETEXT.0 as u32).is_ok() {
                let h = GetClipboardData(CF_UNICODETEXT.0 as u32).ok()?;
                let g = HGLOBAL(h.0);
                let p = GlobalLock(g) as *const u16;
                if p.is_null() {
                    return None;
                }
                let mut n = 0;
                while *p.add(n) != 0 {
                    n += 1;
                }
                let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
                let _ = GlobalUnlock(g);
                return Some(Clip::Text(s));
            }
            None
        })();
        let _ = CloseClipboard();
        res
    }
}

fn dib_to_rgba(d: &[u8]) -> Option<Clip> {
    let rd32 = |o: usize| -> Option<u32> { Some(u32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?)) };
    let hsize = rd32(0)?;
    let w = rd32(4)? as i32;
    let h = rd32(8)? as i32;
    let bpp = u16::from_le_bytes(d.get(14..16)?.try_into().ok()?);
    let comp = rd32(16)?;
    if !(bpp == 24 || bpp == 32) || w <= 0 || h == 0 {
        return None;
    }
    let mut off = hsize as usize;
    if comp == 3 && hsize == 40 {
        off += 12; // BI_BITFIELDS masks
    }
    let (w, top_down, h) = (w as usize, h < 0, h.unsigned_abs() as usize);
    let stride = (w * bpp as usize / 8 + 3) & !3;
    let mut out = vec![0u8; w * h * 4];
    let mut any_alpha = false;
    for y in 0..h {
        let sy = if top_down { y } else { h - 1 - y };
        let row = d.get(off + sy * stride..off + sy * stride + stride)?;
        for x in 0..w {
            let px = &row[x * bpp as usize / 8..];
            let o = (y * w + x) * 4;
            out[o] = px[2];
            out[o + 1] = px[1];
            out[o + 2] = px[0];
            out[o + 3] = if bpp == 32 { px[3] } else { 255 };
            any_alpha |= bpp == 32 && px[3] != 0;
        }
    }
    if !any_alpha {
        out.chunks_mut(4).for_each(|p| p[3] = 255);
    }
    Some(Clip::Image(out, w as u32, h as u32))
}

pub fn write_png(path: &Path, rgba: &[u8], w: u32, h: u32) -> Result<(), String> {
    let f = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut wr = enc.write_header().map_err(|e| e.to_string())?;
    wr.write_image_data(rgba).map_err(|e| e.to_string())
}

fn drop_effect_format() -> u32 {
    unsafe { RegisterClipboardFormatW(windows::core::w!("Preferred DropEffect")) }
}

/// Put files on the clipboard exactly like Explorer's Copy / Cut.
pub fn set_clipboard_files(paths: &[PathBuf], cut: bool) -> bool {
    use windows::Win32::UI::Shell::DROPFILES;
    if paths.is_empty() {
        return false;
    }
    let mut list: Vec<u16> = Vec::new();
    for p in paths {
        list.extend(p.as_os_str().to_string_lossy().encode_utf16());
        list.push(0);
    }
    list.push(0);
    unsafe {
        if OpenClipboard(crate::main_hwnd()).is_err() {
            return false;
        }
        let _ = EmptyClipboard();
        let header = std::mem::size_of::<DROPFILES>();
        let mut ok = false;
        if let Ok(h) = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, header + list.len() * 2) {
            let p = GlobalLock(h) as *mut u8;
            if !p.is_null() {
                let df = p as *mut DROPFILES;
                (*df).pFiles = header as u32;
                (*df).fWide = TRUE;
                std::ptr::copy_nonoverlapping(list.as_ptr() as *const u8, p.add(header), list.len() * 2);
                let _ = GlobalUnlock(h);
                ok = SetClipboardData(CF_HDROP.0 as u32, HANDLE(h.0)).is_ok();
            }
        }
        if let Ok(h) = GlobalAlloc(GMEM_MOVEABLE, 4) {
            let p = GlobalLock(h) as *mut u32;
            if !p.is_null() {
                *p = if cut { 2 } else { 5 }; // DROPEFFECT_MOVE / DROPEFFECT_COPY|LINK (what Explorer uses)
                let _ = GlobalUnlock(h);
                let _ = SetClipboardData(drop_effect_format(), HANDLE(h.0));
            }
        }
        let _ = CloseClipboard();
        ok
    }
}

/// Files on the clipboard (from Explorer or Orbit) and whether they were cut.
pub fn get_clipboard_files() -> Option<(Vec<PathBuf>, bool)> {
    use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
    unsafe {
        OpenClipboard(crate::main_hwnd()).ok()?;
        let res = (|| {
            IsClipboardFormatAvailable(CF_HDROP.0 as u32).ok()?;
            let h = GetClipboardData(CF_HDROP.0 as u32).ok()?;
            let hdrop = HDROP(h.0);
            let n = DragQueryFileW(hdrop, u32::MAX, None);
            let mut files = Vec::new();
            for i in 0..n {
                let len = DragQueryFileW(hdrop, i, None) as usize;
                let mut buf = vec![0u16; len + 1];
                DragQueryFileW(hdrop, i, Some(&mut buf));
                files.push(PathBuf::from(String::from_utf16_lossy(&buf[..len])));
            }
            let mut cut = false;
            let fmt = drop_effect_format();
            if IsClipboardFormatAvailable(fmt).is_ok() {
                if let Ok(h) = GetClipboardData(fmt) {
                    let p = GlobalLock(HGLOBAL(h.0)) as *const u32;
                    if !p.is_null() {
                        cut = *p & 2 != 0 && *p & 1 == 0;
                        let _ = GlobalUnlock(HGLOBAL(h.0));
                    }
                }
            }
            (!files.is_empty()).then_some((files, cut))
        })();
        let _ = CloseClipboard();
        res
    }
}
