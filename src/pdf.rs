//! Native PDF operations: merge, split, images → PDF.

use std::path::{Path, PathBuf};

use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Document, Object, ObjectId, Stream};

fn err(e: impl std::fmt::Display) -> String {
    format!("PDF error: {e}")
}

pub fn merge(files: &[PathBuf], out: &Path) -> Result<(), String> {
    let mut max_id = 1;
    let mut pages: Vec<(ObjectId, Object)> = Vec::new();
    let mut objects = std::collections::BTreeMap::new();
    for f in files {
        let mut doc = Document::load(f).map_err(|e| format!("Couldn't open {}: {e}", crate::util::name(f)))?;
        doc.renumber_objects_with(max_id);
        max_id = doc.max_id + 1;
        for (_, id) in doc.get_pages() {
            if let Ok(o) = doc.get_object(id) {
                pages.push((id, o.clone()));
            }
        }
        objects.extend(doc.objects);
    }
    let mut document = Document::with_version("1.5");
    let mut catalog: Option<(ObjectId, Object)> = None;
    let mut pages_root: Option<(ObjectId, Object)> = None;
    for (id, obj) in &objects {
        match obj.type_name().unwrap_or("") {
            "Catalog" => {
                if catalog.is_none() {
                    catalog = Some((*id, obj.clone()));
                }
            }
            "Pages" => {
                if let Ok(d) = obj.as_dict() {
                    let mut d = d.clone();
                    if let Some((_, ref old)) = pages_root {
                        if let Ok(od) = old.as_dict() {
                            d.extend(od);
                        }
                    }
                    pages_root = Some((pages_root.as_ref().map(|p| p.0).unwrap_or(*id), Object::Dictionary(d)));
                }
            }
            "Page" | "Outlines" | "Outline" => {}
            _ => {
                document.objects.insert(*id, obj.clone());
            }
        }
    }
    let (pages_id, pages_obj) = pages_root.ok_or("No pages found")?;
    let (catalog_id, catalog_obj) = catalog.ok_or("No catalog found")?;
    for (id, obj) in &pages {
        if let Ok(d) = obj.as_dict() {
            let mut d = d.clone();
            d.set("Parent", pages_id);
            document.objects.insert(*id, Object::Dictionary(d));
        }
    }
    if let Ok(d) = pages_obj.as_dict() {
        let mut d = d.clone();
        d.set("Count", pages.len() as u32);
        d.set("Kids", pages.iter().map(|(id, _)| Object::Reference(*id)).collect::<Vec<_>>());
        document.objects.insert(pages_id, Object::Dictionary(d));
    }
    if let Ok(d) = catalog_obj.as_dict() {
        let mut d = d.clone();
        d.set("Pages", pages_id);
        d.remove(b"Outlines");
        document.objects.insert(catalog_id, Object::Dictionary(d));
    }
    document.trailer.set("Root", catalog_id);
    document.max_id = document.objects.keys().map(|k| k.0).max().unwrap_or(0);
    document.renumber_objects();
    document.compress();
    document.save(out).map_err(err)?;
    Ok(())
}

/// Split every page into its own PDF inside `dir`. Returns page count.
pub fn split(file: &Path, dir: &Path, progress: impl Fn(f32)) -> Result<usize, String> {
    let doc = Document::load(file).map_err(|e| format!("Couldn't open {}: {e}", crate::util::name(file)))?;
    let pages: Vec<u32> = doc.get_pages().keys().copied().collect();
    let n = pages.len();
    if n == 0 {
        return Err("PDF has no pages".into());
    }
    std::fs::create_dir_all(dir).map_err(err)?;
    let stem = crate::util::stem(file);
    let width = n.to_string().len().max(2);
    for (i, &p) in pages.iter().enumerate() {
        progress(i as f32 / n as f32);
        let mut d = doc.clone();
        let others: Vec<u32> = pages.iter().copied().filter(|&x| x != p).collect();
        d.delete_pages(&others);
        d.prune_objects();
        d.compress();
        d.save(dir.join(format!("{stem}_p{:0width$}.pdf", i + 1))).map_err(err)?;
    }
    Ok(n)
}

pub fn jpeg_size(data: &[u8]) -> Option<(u32, u32, u8)> {
    let mut i = 2;
    while i + 9 < data.len() {
        if data[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = data[i + 1];
        let len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        if matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF) {
            let h = u16::from_be_bytes([data[i + 5], data[i + 6]]) as u32;
            let w = u16::from_be_bytes([data[i + 7], data[i + 8]]) as u32;
            let comps = data[i + 9];
            return Some((w, h, comps));
        }
        i += 2 + len;
    }
    None
}

pub fn from_jpegs(jpgs: &[PathBuf], out: &Path) -> Result<(), String> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let mut kids: Vec<Object> = Vec::new();
    for j in jpgs {
        let data = std::fs::read(j).map_err(err)?;
        let (w, h, comps) = jpeg_size(&data).ok_or("Couldn't read image size")?;
        let cs = if comps == 1 { "DeviceGray" } else { "DeviceRGB" };
        let mut img = Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => w as i64,
                "Height" => h as i64,
                "ColorSpace" => cs,
                "BitsPerComponent" => 8,
                "Filter" => "DCTDecode",
            },
            data,
        );
        img.allows_compression = false;
        let img_id = doc.add_object(img);
        // 96 dpi → points
        let (pw, ph) = (w as f32 * 0.75, h as f32 * 0.75);
        let content = Content {
            operations: vec![
                Operation::new("q", vec![]),
                Operation::new("cm", vec![pw.into(), 0.into(), 0.into(), ph.into(), 0.into(), 0.into()]),
                Operation::new("Do", vec![Object::Name(b"Im0".to_vec())]),
                Operation::new("Q", vec![]),
            ],
        };
        let content_id = doc.add_object(Stream::new(dictionary! {}, content.encode().map_err(err)?));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), pw.into(), ph.into()],
            "Contents" => content_id,
            "Resources" => dictionary! { "XObject" => dictionary! { "Im0" => img_id } },
        });
        kids.push(page_id.into());
    }
    let count = kids.len() as i64;
    doc.objects.insert(pages_id, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => count }));
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog);
    doc.compress();
    doc.save(out).map_err(err)?;
    Ok(())
}

/// Rotate every page by `deg` (90, 180, 270).
pub fn rotate(input: &Path, out: &Path, deg: i64) -> Result<usize, String> {
    let mut doc = Document::load(input).map_err(|e| format!("Couldn't open {}: {e}", crate::util::name(input)))?;
    let pages: Vec<ObjectId> = doc.get_pages().values().copied().collect();
    for id in &pages {
        if let Ok(d) = doc.get_object_mut(*id).and_then(|o| o.as_dict_mut()) {
            let cur = d.get(b"Rotate").and_then(|r| r.as_i64()).unwrap_or(0);
            d.set("Rotate", (cur + deg).rem_euclid(360));
        }
    }
    doc.save(out).map_err(err)?;
    Ok(pages.len())
}

fn name_of(o: Option<&Object>) -> Option<Vec<u8>> {
    o.and_then(|o| o.as_name().ok()).map(|n| n.to_vec())
}

/// Shrink a PDF by re-encoding its images as smaller JPEGs (max 1600 px, quality ~75).
/// Returns (old size, new size).
pub fn compress(input: &Path, out: &Path, ff: &Path, progress: impl Fn(f32)) -> Result<(u64, u64), String> {
    let mut doc = Document::load(input).map_err(|e| format!("Couldn't open {}: {e}", crate::util::name(input)))?;
    let tmp = crate::tools::temp_dir();
    let tag = std::process::id();
    let ids: Vec<ObjectId> = doc
        .objects
        .iter()
        .filter(|(_, o)| matches!(o, Object::Stream(s) if name_of(s.dict.get(b"Subtype").ok()).as_deref() == Some(b"Image")))
        .map(|(id, _)| *id)
        .collect();
    for (k, id) in ids.iter().enumerate() {
        progress(k as f32 / ids.len().max(1) as f32);
        let Some(Object::Stream(s)) = doc.objects.get_mut(id) else { continue };
        let int = |key: &[u8]| s.dict.get(key).and_then(|v| v.as_i64()).unwrap_or(0);
        let (w, h, bpc) = (int(b"Width"), int(b"Height"), int(b"BitsPerComponent"));
        if w * h < 160 * 160 || bpc != 8 || s.dict.has(b"Decode") || s.dict.has(b"ImageMask") {
            continue;
        }
        let filter = name_of(s.dict.get(b"Filter").ok());
        let cs = name_of(s.dict.get(b"ColorSpace").ok());
        let src = tmp.join(format!("pdfc_{tag}_{}_in", id.0));
        let dst = tmp.join(format!("pdfc_{tag}_{}_out.jpg", id.0));
        let mut args: Vec<String> = Vec::new();
        let old_comps;
        match filter.as_deref() {
            Some(b"DCTDecode") => {
                let Some((_, _, c)) = jpeg_size(&s.content) else { continue };
                if c == 4 || std::fs::write(&src, &s.content).is_err() {
                    continue;
                }
                old_comps = c;
            }
            Some(b"FlateDecode") if !s.dict.has(b"DecodeParms") && matches!(cs.as_deref(), Some(b"DeviceRGB") | Some(b"DeviceGray")) => {
                let comps = if cs.as_deref() == Some(b"DeviceRGB") { 3 } else { 1 };
                let Ok(raw) = s.decompressed_content() else { continue };
                if raw.len() != (w * h * comps) as usize || std::fs::write(&src, &raw).is_err() {
                    continue;
                }
                let fmt = if comps == 3 { "rgb24" } else { "gray" };
                args.extend(["-f", "rawvideo", "-pix_fmt", fmt, "-s", &format!("{w}x{h}")].map(String::from));
                old_comps = comps as u8;
            }
            _ => continue,
        }
        args.extend(["-i".to_string(), src.to_string_lossy().to_string()]);
        args.extend(["-vf", "scale=w='min(1600,iw)':h=-2:flags=lanczos", "-q:v", "6", "-frames:v", "1", "-y"].map(String::from));
        let mut c = crate::tools::cmd(ff);
        c.args(["-hide_banner", "-loglevel", "error"]).args(&args).arg(&dst);
        let ok = crate::tools::run(c).is_ok();
        let _ = std::fs::remove_file(&src);
        let new = if ok { std::fs::read(&dst).ok() } else { None };
        let _ = std::fs::remove_file(&dst);
        let Some(new) = new else { continue };
        let Some((nw, nh, nc)) = jpeg_size(&new) else { continue };
        if new.len() >= s.content.len() {
            continue;
        }
        s.set_content(new);
        s.dict.set("Filter", "DCTDecode");
        s.dict.remove(b"DecodeParms");
        s.dict.set("Width", nw as i64);
        s.dict.set("Height", nh as i64);
        if nc != old_comps {
            s.dict.set("ColorSpace", if nc == 1 { "DeviceGray" } else { "DeviceRGB" });
        }
        s.allows_compression = false;
    }
    doc.prune_objects();
    doc.compress();
    doc.save(out).map_err(err)?;
    let before = std::fs::metadata(input).map(|m| m.len()).unwrap_or(0);
    let after = std::fs::metadata(out).map(|m| m.len()).unwrap_or(0);
    Ok((before, after))
}

/// Render each page to PNG with Windows' built-in PDF engine (Windows.Data.Pdf).
pub fn to_images(input: &Path, dir: &Path, progress: impl Fn(f32)) -> Result<usize, String> {
    use windows::core::{Interface, HSTRING};
    use windows::Data::Pdf::{PdfDocument, PdfPageRenderOptions};
    use windows::Graphics::Imaging::BitmapEncoder;
    use windows::Storage::{CreationCollisionOption, FileAccessMode, StorageFile, StorageFolder};
    let e = |e: windows::core::Error| format!("Couldn't render PDF: {}", e.message());
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
    }
    let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(input)).and_then(|a| a.get()).map_err(e)?;
    let pdf = PdfDocument::LoadFromFileAsync(&file).and_then(|a| a.get()).map_err(|_| "Couldn't open this PDF (is it password protected?)".to_string())?;
    std::fs::create_dir_all(dir).map_err(err)?;
    let folder = StorageFolder::GetFolderFromPathAsync(&HSTRING::from(dir)).and_then(|a| a.get()).map_err(e)?;
    let n = pdf.PageCount().map_err(e)?;
    let stem = crate::util::stem(input);
    let width = n.to_string().len().max(2);
    for i in 0..n {
        progress(i as f32 / n as f32);
        let page = pdf.GetPage(i).map_err(e)?;
        let size = page.Size().map_err(e)?;
        let name = format!("{stem}_p{:0width$}.png", i + 1);
        let out = folder.CreateFileAsync(&HSTRING::from(name), CreationCollisionOption::ReplaceExisting).and_then(|a| a.get()).map_err(e)?;
        let stream = out.OpenAsync(FileAccessMode::ReadWrite).and_then(|a| a.get()).map_err(e)?;
        let opts = PdfPageRenderOptions::new().map_err(e)?;
        // ~180 dpi, capped so huge posters don't explode
        opts.SetDestinationWidth((size.Width * 2.5).clamp(200.0, 4000.0) as u32).map_err(e)?;
        opts.SetBitmapEncoderId(BitmapEncoder::PngEncoderId().map_err(e)?).map_err(e)?;
        page.RenderWithOptionsToStreamAsync(&stream, &opts).and_then(|a| a.get()).map_err(e)?;
        stream.FlushAsync().and_then(|a| a.get()).map_err(e)?;
        if let Ok(c) = stream.cast::<windows::Foundation::IClosable>() {
            let _ = c.Close();
        }
    }
    Ok(n as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let dir = std::env::temp_dir().join("orbit_pdf_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // needs a sample JPEG: set ORBIT_TEST_JPG to its path, then run cargo test
        let Some(src) = std::env::var_os("ORBIT_TEST_JPG").map(std::path::PathBuf::from) else { return };
        let a = dir.join("a.pdf");
        let b = dir.join("b.pdf");
        from_jpegs(&[src.clone()], &a).unwrap();
        from_jpegs(&[src.clone(), src.clone()], &b).unwrap();
        let m = dir.join("m.pdf");
        merge(&[a, b], &m).unwrap();
        assert_eq!(Document::load(&m).unwrap().get_pages().len(), 3);
        let n = split(&m, &dir.join("pages"), |_| {}).unwrap();
        assert_eq!(n, 3);
        for e in std::fs::read_dir(dir.join("pages")).unwrap() {
            assert_eq!(Document::load(e.unwrap().path()).unwrap().get_pages().len(), 1);
        }
    }
}

#[cfg(test)]
mod more_tests {
    use super::*;

    #[test]
    fn rotate_render_compress() {
        let Some(src) = std::env::var_os("ORBIT_TEST_BIGJPG").map(std::path::PathBuf::from) else { return };
        let dir = std::env::temp_dir().join("orbit_pdf_test2");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.pdf");
        from_jpegs(&[src.clone(), src.clone()], &a).unwrap();
        let r = dir.join("r.pdf");
        assert_eq!(rotate(&a, &r, 90).unwrap(), 2);
        let doc = Document::load(&r).unwrap();
        for id in doc.get_pages().values() {
            assert_eq!(doc.get_object(*id).unwrap().as_dict().unwrap().get(b"Rotate").unwrap().as_i64().unwrap(), 90);
        }
        let ff = crate::tools::ffmpeg_path().expect("ffmpeg");
        let c = dir.join("c.pdf");
        let (before, after) = compress(&a, &c, &ff, |_| {}).unwrap();
        println!("compress {before} -> {after}");
        assert!(after < before);
        assert_eq!(Document::load(&c).unwrap().get_pages().len(), 2);
        let n = to_images(&r, &dir.join("img"), |_| {}).unwrap();
        assert_eq!(n, 2);
        let files: Vec<_> = std::fs::read_dir(dir.join("img")).unwrap().flatten().collect();
        assert_eq!(files.len(), 2);
        let png = std::fs::read(files[0].path()).unwrap();
        let (w, h) = (u32::from_be_bytes(png[16..20].try_into().unwrap()), u32::from_be_bytes(png[20..24].try_into().unwrap()));
        println!("page image {w}x{h}");
        assert!(h > w, "rotated page should be portrait");
    }
}
