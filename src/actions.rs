//! Everything the menu can do.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::explorer::{self, Ctx};
use crate::hud::ToastKind;
use crate::input;
use crate::jobs::{self, Job, JobResult};
use crate::menu::{kind_of, FileKind};
use crate::tools;
use crate::util::{self, ext, name, sibling, stem, unique};

#[derive(Clone)]
pub enum Action {
    Share,
    Receive,
    Archive(&'static str),
    Extract(bool),
    CopyPath,
    Hash,
    Properties,
    BatchRename,
    Terminal,
    VsCode,
    FolderSize,
    Flatten,
    OrganizeType,
    OrganizeDate,
    ExtractAll,
    NewFolder,
    NewFile(&'static str),
    Paste,
    Copy,
    Cut,
    Rename,
    Delete,
    ToggleHidden,
    Img(Img),
    Media(Media),
    Doc(Doc),
    PdfMerge,
    PdfSplit,
    ImagesToPdf,
    FormatJson,
    CopyBase64,
    CountLines,
    ImageInfo,
    MoveTo(Option<PathBuf>),
    CopyTo(Option<PathBuf>),
    PdfToImages,
    PdfCompress,
    PdfRotate(i64),
}

#[derive(Clone)]
pub enum Img {
    Convert(&'static str),
    Height(u32),
    Percent(u32),
    Width,
    Custom,
    Compress,
    Rotate(&'static str, &'static str),
    Strip,
    Favicon,
    Combine(&'static str),
}

#[derive(Clone)]
pub enum Media {
    VConvert(&'static str),
    ExtractAudio(&'static str),
    VResize(u32),
    VCompress,
    Trim,
    Frame,
    Mute,
    ToGif,
    AConvert(&'static str),
    Normalize,
    Speed(u32),
    Volume(u32),
    Merge,
    VRotate(&'static str),
}

#[derive(Clone)]
pub enum Doc {
    Office(&'static str),
    PdfToWord,
    SlidesImages,
}

pub const TIMER_RENAME: usize = 78;

thread_local! {
    static RENAME: RefCell<Option<(IShellViewHolder, PathBuf, isize)>> = const { RefCell::new(None) };
}
struct IShellViewHolder(IShellView);

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

fn targets(ctx: &Ctx) -> Vec<PathBuf> {
    if ctx.background { vec![ctx.folder.clone()] } else { ctx.items.clone() }
}

fn dirs_of(ctx: &Ctx) -> Vec<PathBuf> {
    if ctx.background { vec![ctx.folder.clone()] } else { ctx.items.iter().filter(|p| p.is_dir()).cloned().collect() }
}

fn files_of(ctx: &Ctx) -> Vec<PathBuf> {
    ctx.items.iter().filter(|p| p.is_file()).cloned().collect()
}

fn summarize(made: &[PathBuf], errs: &[String]) -> JobResult {
    match (made.len(), errs.len()) {
        (0, 0) => Ok(String::new()),
        (0, _) => Err(errs[0].clone()),
        (1, 0) => Ok(format!("Saved {}", name(&made[0]))),
        (n, 0) => Ok(format!("Saved {n} files")),
        (n, e) => Ok(format!("Saved {n} files · {e} failed: {}", errs[0])),
    }
}

pub fn run(action: Action, ctx: Rc<Ctx>) {
    let at = ctx.pt;
    match action {
        Action::Share => crate::share::share(targets(&ctx)),
        Action::Receive => crate::share::receive(ctx.folder.clone()),
        Action::Archive(fmt) => archive(&ctx, fmt),
        Action::Extract(to_folder) => {
            let items = files_of(&ctx);
            jobs::spawn("Extracting", at, move |job| {
                let mut made = Vec::new();
                for (i, a) in items.iter().enumerate() {
                    job.progress(if items.len() > 1 { i as f32 / items.len() as f32 } else { -1.0 });
                    let parent = a.parent().unwrap_or(Path::new(".")).to_path_buf();
                    let dest = if to_folder { unique(parent.join(stem_no_tar(a))) } else { parent };
                    extract(job, a, &dest)?;
                    made.push(dest);
                }
                Ok(if to_folder && made.len() == 1 { format!("Extracted to {}", name(&made[0])) } else { "Extracted".into() })
            });
        }
        Action::CopyPath => {
            let text = targets(&ctx).iter().map(|p| p.to_string_lossy().to_string()).collect::<Vec<_>>().join("\r\n");
            util::set_clipboard_text(&text);
            crate::hud::toast(if ctx.items.len() > 1 { "Paths copied" } else { "Path copied" }, ToastKind::Ok);
        }
        Action::Hash => {
            let files = files_of(&ctx);
            jobs::spawn("Hashing", at, move |job| {
                let mut out = Vec::new();
                for (i, f) in files.iter().enumerate() {
                    job.progress(i as f32 / files.len() as f32);
                    let (sha, md5) = hash_file(f).map_err(|e| format!("Couldn't read {}: {e}", name(f)))?;
                    out.push(format!("{}\r\nSHA-256: {sha}\r\nMD5: {md5}", name(f)));
                }
                let text = out.join("\r\n\r\n");
                let short = text.lines().nth(1).unwrap_or("").chars().take(30).collect::<String>();
                jobs::post_ui(move || {
                    util::set_clipboard_text(&text);
                });
                Ok(format!("Copied · {short}…"))
            });
        }
        Action::Properties => {
            let p = targets(&ctx).into_iter().next().unwrap_or(ctx.folder.clone());
            let w = util_wide(&p);
            unsafe {
                let _ = SHObjectProperties(crate::main_hwnd(), SHOP_FILEPATH, PCWSTR(w.as_ptr()), PCWSTR::null());
            }
        }
        Action::BatchRename => batch_rename(&ctx),
        Action::Terminal => {
            let dir = if ctx.background { ctx.folder.clone() } else { ctx.items.iter().find(|p| p.is_dir()).cloned().unwrap_or(ctx.folder.clone()) };
            open_terminal(&dir);
        }
        Action::VsCode => {
            let t = targets(&ctx);
            if let Err(e) = open_vscode(&t) {
                crate::hud::toast(&e, ToastKind::Error);
            }
        }
        Action::FolderSize => {
            let dirs = dirs_of(&ctx);
            jobs::spawn("Measuring", at, move |job| {
                job.progress(-1.0);
                let (mut b, mut f, mut d) = (0, 0, 0);
                for dir in &dirs {
                    let (bb, ff, dd) = util::dir_stats(dir);
                    b += bb;
                    f += ff;
                    d += dd;
                }
                Ok(format!("{} · {} · {}", util::human_size(b), util::plural(f as usize, "file"), util::plural(d as usize, "folder")))
            });
        }
        Action::Flatten => folder_job(&ctx, "Flattening", flatten),
        Action::OrganizeType => folder_job(&ctx, "Organizing", organize_type),
        Action::OrganizeDate => folder_job(&ctx, "Organizing", organize_date),
        Action::ExtractAll => folder_job(&ctx, "Extracting", extract_all),
        Action::NewFolder => {
            let p = unique(ctx.folder.join("New folder"));
            match std::fs::create_dir(&p) {
                Ok(_) => schedule_rename(&ctx, p),
                Err(e) => crate::hud::toast(&format!("Couldn't create folder: {e}"), ToastKind::Error),
            }
        }
        Action::NewFile(e) => new_file(&ctx, e),
        Action::Paste => paste(&ctx),
        Action::Copy => copy_to_clipboard(&ctx, false),
        Action::Cut => copy_to_clipboard(&ctx, true),
        Action::Rename => rename(&ctx),
        Action::Delete => {
            let permanent = unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState(0x10) } < 0;
            crate::fileops::run(crate::fileops::Op::Delete(permanent), ctx.items.clone(), None, String::new());
        }
        Action::ToggleHidden => toggle_hidden(&ctx),
        Action::Img(op) => image_op(&ctx, op),
        Action::Media(op) => media_op(&ctx, op),
        Action::Doc(op) => doc_op(&ctx, op),
        Action::PdfMerge => {
            let mut files = files_of(&ctx);
            files.sort_by_key(|p| natural_key(&name(p)));
            let out = unique(ctx.folder.join("Merged.pdf"));
            jobs::spawn("Merging", at, move |job| {
                job.progress(-1.0);
                crate::pdf::merge(&files, &out)?;
                Ok(format!("Saved {}", name(&out)))
            });
        }
        Action::PdfSplit => {
            let files = files_of(&ctx);
            jobs::spawn("Splitting", at, move |job| {
                let mut total = 0;
                for f in &files {
                    let dir = unique(f.with_file_name(format!("{}_pages", stem(f))));
                    total += crate::pdf::split(f, &dir, |k| job.progress(k))?;
                }
                Ok(format!("Split into {total} pages"))
            });
        }
        Action::ImagesToPdf => {
            let mut files = files_of(&ctx);
            files.sort_by_key(|p| natural_key(&name(p)));
            let out = if files.len() == 1 { sibling(&files[0], "", "pdf") } else { unique(ctx.folder.join("Images.pdf")) };
            jobs::spawn("Making PDF", at, move |job| {
                let ff = tools::ffmpeg(job)?;
                job.caption("Making PDF");
                let tmp = tools::temp_dir();
                let mut jpgs = Vec::new();
                for (i, f) in files.iter().enumerate() {
                    let j = tmp.join(format!("pdfimg_{}_{i}.jpg", job.id));
                    let mut post = flatten_alpha_args();
                    post.extend(s(&["-frames:v", "1", "-q:v", "2"]));
                    tools::ffmpeg_run(job, &ff, &[], f, &post, &j, i, files.len())?;
                    jpgs.push(j);
                }
                let r = crate::pdf::from_jpegs(&jpgs, &out);
                jpgs.iter().for_each(|j| {
                    let _ = std::fs::remove_file(j);
                });
                r?;
                Ok(format!("Saved {}", name(&out)))
            });
        }
        Action::FormatJson => {
            let mut done = 0;
            for f in files_of(&ctx).iter().filter(|p| ext(p) == "json") {
                let res = std::fs::read_to_string(f)
                    .map_err(|e| e.to_string())
                    .and_then(|t| serde_json::from_str::<serde_json::Value>(t.trim_start_matches('\u{feff}')).map_err(|e| e.to_string()))
                    .and_then(|v| serde_json::to_string_pretty(&v).map_err(|e| e.to_string()))
                    .and_then(|t| std::fs::write(f, t + "\n").map_err(|e| e.to_string()));
                match res {
                    Ok(_) => done += 1,
                    Err(e) => {
                        crate::hud::toast(&format!("{}: invalid JSON — {e}", name(f)), ToastKind::Error);
                        return;
                    }
                }
            }
            crate::hud::toast(&format!("Formatted {}", util::plural(done, "file")), ToastKind::Ok);
        }
        Action::CopyBase64 => {
            let Some(f) = files_of(&ctx).into_iter().next() else { return };
            match std::fs::read(&f) {
                Ok(data) if data.len() <= 8 * 1024 * 1024 => {
                    use base64::Engine;
                    let b = base64::engine::general_purpose::STANDARD.encode(&data);
                    let text = if kind_of(&f) == FileKind::Image {
                        let mime = match ext(&f).as_str() {
                            "jpg" | "jpeg" | "jfif" => "image/jpeg".to_string(),
                            "svg" => "image/svg+xml".to_string(),
                            "ico" => "image/x-icon".to_string(),
                            e => format!("image/{e}"),
                        };
                        format!("data:{mime};base64,{b}")
                    } else {
                        b
                    };
                    util::set_clipboard_text(&text);
                    crate::hud::toast(&format!("Base64 copied ({})", util::human_size(text.len() as u64)), ToastKind::Ok);
                }
                Ok(_) => crate::hud::toast("File is too large for Base64 (max 8 MB)", ToastKind::Error),
                Err(e) => crate::hud::toast(&e.to_string(), ToastKind::Error),
            }
        }
        Action::ImageInfo => image_info(files_of(&ctx)),
        Action::MoveTo(dest) => send_to(&ctx, dest, true),
        Action::CopyTo(dest) => send_to(&ctx, dest, false),
        Action::PdfToImages => {
            let files = files_of(&ctx);
            jobs::spawn("PDF → images", at, move |job| {
                let mut total = 0;
                for f in &files {
                    let dir = unique(f.with_file_name(format!("{}_images", stem(f))));
                    total += crate::pdf::to_images(f, &dir, |k| job.progress(k))?;
                }
                Ok(format!("Saved {}", util::plural(total, "page image")))
            });
        }
        Action::PdfCompress => {
            let files = files_of(&ctx);
            jobs::spawn("Compressing PDF", at, move |job| {
                let ff = tools::ffmpeg(job)?;
                job.caption("Compressing PDF");
                let mut msgs = Vec::new();
                for f in &files {
                    let out = sibling(f, "_compressed", "pdf");
                    let (before, after) = crate::pdf::compress(f, &out, &ff, |k| job.progress(k))?;
                    if after >= before {
                        let _ = std::fs::remove_file(&out);
                        msgs.push(format!("{} is already as small as it gets", name(f)));
                    } else {
                        let pct = 100 - after * 100 / before.max(1);
                        msgs.push(format!("{} → {} (−{pct}%)", util::human_size(before), util::human_size(after)));
                    }
                }
                Ok(msgs.join(" · "))
            });
        }
        Action::PdfRotate(deg) => {
            let files = files_of(&ctx);
            jobs::spawn("Rotating pages", at, move |job| {
                job.progress(-1.0);
                let mut made = Vec::new();
                for f in &files {
                    let out = sibling(f, "_rotated", "pdf");
                    crate::pdf::rotate(f, &out, deg)?;
                    made.push(out);
                }
                summarize(&made, &[])
            });
        }
        Action::CountLines => {
            let files = files_of(&ctx);
            let mut lines = 0usize;
            let mut words = 0usize;
            for f in &files {
                if let Ok(data) = std::fs::read(f) {
                    let t = String::from_utf8_lossy(&data);
                    lines += t.lines().count();
                    words += t.split_whitespace().count();
                }
            }
            crate::hud::toast(&format!("{} lines · {} words · {}", fmt_num(lines), fmt_num(words), util::plural(files.len(), "file")), ToastKind::Info);
        }
    }
}

fn fmt_num(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn util_wide(p: &Path) -> Vec<u16> {
    crate::ui::wide(&p.to_string_lossy())
}

fn natural_key(s: &str) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    let mut txt = String::new();
    let mut num = String::new();
    for c in s.to_lowercase().chars() {
        if c.is_ascii_digit() {
            num.push(c);
        } else {
            if !num.is_empty() {
                out.push((std::mem::take(&mut txt), num.parse().unwrap_or(0)));
                num.clear();
            }
            txt.push(c);
        }
    }
    out.push((txt, num.parse().unwrap_or(0)));
    out
}

// ------------------------------------------------------------------ files & folders

fn schedule_rename(ctx: &Ctx, p: PathBuf) {
    if let Some(v) = ctx.view.clone() {
        RENAME.with(|r| *r.borrow_mut() = Some((IShellViewHolder(v), p, ctx.defview.0 as isize)));
        unsafe { SetTimer(crate::main_hwnd(), TIMER_RENAME, 300, None) };
    }
}

/// Called from the main window timer once Explorer has noticed the new item.
pub fn do_rename() {
    unsafe {
        let _ = KillTimer(crate::main_hwnd(), TIMER_RENAME);
    }
    if let Some((v, p, defview)) = RENAME.with(|r| r.borrow_mut().take()) {
        // the radial menu had focus; give it back to Explorer so inline rename sticks
        crate::ui::force_foreground(unsafe { GetAncestor(HWND(defview as _), GA_ROOT) });
        explorer::select_and_rename(&v.0, &p);
    }
}

fn template(e: &str) -> &'static str {
    match e {
        "html" => "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n  <meta charset=\"UTF-8\">\n  <meta name=\"viewport\" content=\"width=device-width, initial-scale=1.0\">\n  <title>Document</title>\n</head>\n<body>\n\n</body>\n</html>\n",
        "json" => "{\n  \n}\n",
        "py" => "def main():\n    pass\n\n\nif __name__ == \"__main__\":\n    main()\n",
        "md" => "# Title\n\n",
        "bat" => "@echo off\n\n",
        "ps1" => "# PowerShell script\n\n",
        "rs" => "fn main() {\n    \n}\n",
        "cs" => "Console.WriteLine(\"Hello\");\n",
        "java" => "public class Main {\n    public static void main(String[] args) {\n        \n    }\n}\n",
        "gitignore" => "node_modules/\ndist/\nbuild/\ntarget/\n.env\n*.log\n.DS_Store\n",
        "yml" => "",
        _ => "",
    }
}

fn new_file(ctx: &Ctx, e: &str) {
    let fname = match e {
        "gitignore" => ".gitignore".to_string(),
        "env" => ".env".to_string(),
        "java" => "Main.java".to_string(),
        _ => format!("New file.{e}"),
    };
    let p = unique(ctx.folder.join(fname));
    match std::fs::write(&p, template(e)) {
        Ok(_) => schedule_rename(ctx, p),
        Err(err) => crate::hud::toast(&format!("Couldn't create file: {err}"), ToastKind::Error),
    }
}

fn copy_to_clipboard(ctx: &Ctx, cut: bool) {
    let items = targets(ctx);
    if util::set_clipboard_files(&items, cut) {
        let what = if items.len() == 1 { name(&items[0]) } else { util::plural(items.len(), "item") };
        crate::hud::toast(&format!("{} {what} — paste anywhere", if cut { "Cut" } else { "Copied" }), ToastKind::Ok);
    }
}

/// Paste files copied in Orbit or Explorer; otherwise turn clipboard text / images into files.
fn paste(ctx: &Ctx) {
    if let Some((files, cut)) = util::get_clipboard_files() {
        let op = if cut { crate::fileops::Op::Move } else { crate::fileops::Op::Copy };
        let msg = format!("{} {}", if cut { "Moved" } else { "Pasted" }, util::plural(files.len(), "item"));
        crate::config::push_recent(&ctx.folder);
        crate::fileops::run(op, files, Some(ctx.folder.clone()), msg);
        return;
    }
    paste_file(ctx);
}

fn rename(ctx: &Ctx) {
    match ctx.items.as_slice() {
        [one] if ctx.view.is_some() => schedule_rename(ctx, one.clone()),
        [one] => {
            let Some(new) = input::prompt("Rename", "New name", &name(one)) else { return };
            let new = new.trim();
            if new.is_empty() || new == name(one) {
                return;
            }
            if let Err(e) = std::fs::rename(one, one.with_file_name(new)) {
                crate::hud::toast(&format!("Couldn't rename: {e}"), ToastKind::Error);
            }
        }
        [] => {}
        _ => batch_rename(ctx),
    }
}

fn paste_file(ctx: &Ctx) {
    match util::get_clipboard() {
        Some(util::Clip::Text(t)) => {
            let tt = t.trim_start();
            let e = if tt.starts_with('{') || tt.starts_with('[') && serde_json::from_str::<serde_json::Value>(tt).is_ok() {
                "json"
            } else if tt.to_lowercase().starts_with("<!doctype html") || tt.to_lowercase().starts_with("<html") {
                "html"
            } else {
                "txt"
            };
            let p = unique(ctx.folder.join(format!("Pasted text.{e}")));
            match std::fs::write(&p, t) {
                Ok(_) => schedule_rename(ctx, p),
                Err(e) => crate::hud::toast(&e.to_string(), ToastKind::Error),
            }
        }
        Some(util::Clip::Image(px, w, h)) => {
            let p = unique(ctx.folder.join("Pasted image.png"));
            match util::write_png(&p, &px, w, h) {
                Ok(_) => schedule_rename(ctx, p),
                Err(e) => crate::hud::toast(&e, ToastKind::Error),
            }
        }
        None => crate::hud::toast("Clipboard has no text or image", ToastKind::Info),
    }
}

fn toggle_hidden(ctx: &Ctx) {
    use winreg::enums::*;
    let key = winreg::RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Advanced", KEY_READ | KEY_WRITE);
    let Ok(key) = key else { return };
    let cur: u32 = key.get_value("Hidden").unwrap_or(2);
    let show = cur != 1;
    let _ = key.set_value("Hidden", &(if show { 1u32 } else { 2u32 }));
    unsafe {
        SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None);
    }
    explorer::refresh(ctx.defview);
    crate::hud::toast(if show { "Hidden files shown" } else { "Hidden files hidden" }, ToastKind::Info);
}

fn batch_rename(ctx: &Ctx) {
    let mut items = ctx.items.clone();
    if items.is_empty() {
        return;
    }
    items.sort_by_key(|p| natural_key(&name(p)));
    let base = stem(&items[0]).trim_end_matches(|c: char| c.is_ascii_digit() || c == '_' || c == '-' || c == ' ').to_string();
    let default = format!("{}_#", if base.is_empty() { "file" } else { &base });
    let Some(pattern) = input::prompt("Batch rename", "Use # for the number, e.g. holiday_#  →  holiday_01.jpg", &default) else { return };
    let pattern = pattern.trim().to_string();
    if pattern.is_empty() {
        return;
    }
    let pattern = if pattern.contains('#') { pattern } else { format!("{pattern}_#") };
    let width = items.len().to_string().len().max(2);
    // two-phase rename so names can be swapped without collisions
    let mut temps = Vec::new();
    for (i, p) in items.iter().enumerate() {
        let tmp = p.with_file_name(format!(".orbit_tmp_{i}_{}", name(p)));
        if std::fs::rename(p, &tmp).is_ok() {
            temps.push((tmp, p.clone(), i));
        }
    }
    let mut done = 0;
    for (tmp, orig, i) in temps {
        let num = format!("{:0width$}", i + 1);
        let new_stem = pattern.replacen('#', &num, 1).replace('#', "");
        let e = if orig.is_dir() { String::new() } else { orig.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default() };
        let target = unique(orig.with_file_name(format!("{new_stem}{e}")));
        if std::fs::rename(&tmp, &target).is_ok() {
            done += 1;
        } else {
            let _ = std::fs::rename(&tmp, &orig);
        }
    }
    crate::hud::toast(&format!("Renamed {}", util::plural(done, "item")), ToastKind::Ok);
}

fn open_terminal(dir: &Path) {
    let wt = std::env::var_os("LOCALAPPDATA").map(|l| PathBuf::from(l).join("Microsoft\\WindowsApps\\wt.exe"));
    if let Some(wt) = wt.filter(|p| p.exists()) {
        if std::process::Command::new(wt).arg("-d").arg(dir).spawn().is_ok() {
            return;
        }
    }
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("powershell.exe").arg("-NoExit").current_dir(dir).creation_flags(0x10).spawn();
}

fn open_vscode(paths: &[PathBuf]) -> Result<(), String> {
    let mut cands = Vec::new();
    if let Some(l) = std::env::var_os("LOCALAPPDATA") {
        cands.push(PathBuf::from(l).join("Programs\\Microsoft VS Code\\Code.exe"));
    }
    for v in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(p) = std::env::var_os(v) {
            cands.push(PathBuf::from(p).join("Microsoft VS Code\\Code.exe"));
        }
    }
    if let Some(code) = cands.into_iter().find(|p| p.exists()) {
        std::process::Command::new(code).args(paths).spawn().map_err(|e| e.to_string())?;
        return Ok(());
    }
    if let Some(cmd) = tools::which("code.cmd") {
        let mut c = tools::cmd(cmd);
        c.args(paths);
        c.spawn().map_err(|e| e.to_string())?;
        return Ok(());
    }
    Err("VS Code isn't installed".into())
}

fn folder_job(ctx: &Ctx, caption: &str, f: fn(&Job, &Path) -> Result<usize, String>) {
    let dirs = dirs_of(ctx);
    let defview = ctx.defview.0 as isize;
    jobs::spawn(caption, ctx.pt, move |job| {
        job.progress(-1.0);
        let mut n = 0;
        for d in &dirs {
            n += f(job, d)?;
        }
        jobs::post_ui(move || explorer::refresh(HWND(defview as _)));
        Ok(format!("Done · {}", util::plural(n, "item")))
    });
}

fn move_into(file: &Path, dir: &Path) -> bool {
    let _ = std::fs::create_dir_all(dir);
    let target = unique(dir.join(name(file)));
    std::fs::rename(file, target).is_ok()
}

fn top_files(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect())
        .unwrap_or_default()
}

fn flatten(_: &Job, dir: &Path) -> Result<usize, String> {
    let mut moved = 0;
    let mut stack: Vec<PathBuf> = std::fs::read_dir(dir).map_err(|e| e.to_string())?.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    let mut subdirs = Vec::new();
    while let Some(d) = stack.pop() {
        subdirs.push(d.clone());
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if move_into(&p, dir) {
                moved += 1;
            }
        }
    }
    // remove now-empty folders, deepest first
    subdirs.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for d in subdirs {
        let _ = std::fs::remove_dir(d);
    }
    Ok(moved)
}

fn organize_type(_: &Job, dir: &Path) -> Result<usize, String> {
    let mut n = 0;
    for f in top_files(dir) {
        let folder = match kind_of(&f) {
            FileKind::Image => "Images",
            FileKind::Video => "Videos",
            FileKind::Audio => "Audio",
            FileKind::Pdf | FileKind::Word | FileKind::Slides | FileKind::Sheet => "Documents",
            FileKind::Archive => "Archives",
            FileKind::Text => "Code & Text",
            FileKind::Other => match ext(&f).as_str() {
                "exe" | "msi" | "msix" | "appx" => "Installers",
                "lnk" | "url" | "ini" | "db" => continue,
                _ => "Other",
            },
        };
        if move_into(&f, &dir.join(folder)) {
            n += 1;
        }
    }
    Ok(n)
}

fn organize_date(_: &Job, dir: &Path) -> Result<usize, String> {
    let mut n = 0;
    for f in top_files(dir) {
        let Ok(m) = std::fs::metadata(&f).and_then(|m| m.modified()) else { continue };
        let secs = m.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
        let (y, mo) = ymd(secs);
        if move_into(&f, &dir.join(format!("{y}-{mo:02}"))) {
            n += 1;
        }
    }
    Ok(n)
}

/// Civil-from-days (Howard Hinnant), UTC.
fn ymd(secs: i64) -> (i64, u32) {
    let z = secs.div_euclid(86400) + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y, m)
}

fn extract_all(job: &Job, dir: &Path) -> Result<usize, String> {
    let mut n = 0;
    for f in top_files(dir).into_iter().filter(|f| kind_of(f) == FileKind::Archive) {
        let dest = unique(dir.join(stem_no_tar(&f)));
        extract(job, &f, &dest)?;
        n += 1;
    }
    Ok(n)
}

// ------------------------------------------------------------------ archives

fn stem_no_tar(p: &Path) -> String {
    let s = stem(p);
    s.strip_suffix(".tar").map(str::to_string).unwrap_or(s)
}

fn extract(job: &Job, archive: &Path, dest: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let e = ext(archive);
    if e == "rar" {
        if let Some(unrar) = tools::unrar() {
            let mut c = tools::cmd(unrar);
            c.args(["x", "-o+", "-idq", "-y"]).arg(archive).arg(format!("{}\\", dest.display()));
            return tools::run(c).map(|_| ());
        }
    }
    if e == "7z" {
        let sz = tools::sevenzip(job)?;
        let mut c = tools::cmd(sz);
        c.args(["x", "-y"]).arg(format!("-o{}", dest.display())).arg(archive);
        return tools::run(c).map(|_| ());
    }
    let mut c = tools::cmd("tar.exe");
    c.arg("-xf").arg(archive).arg("-C").arg(dest);
    tools::run(c).map(|_| ()).map_err(|e| format!("Couldn't extract {}: {e}", name(archive)))
}

fn archive(ctx: &Ctx, fmt: &'static str) {
    let items = targets(ctx);
    if items.is_empty() {
        return;
    }
    let folder = ctx.folder.clone();
    let base = if items.len() == 1 {
        if items[0].is_dir() { name(&items[0]) } else { stem(&items[0]) }
    } else {
        name(&folder)
    };
    let has_rar = tools::winrar().is_some();
    let fell_back = fmt == "rar" && !has_rar;
    let fmt = if fell_back { "zip" } else { fmt };
    let out = unique(folder.join(format!("{base}.{fmt}")));
    jobs::spawn(format!("Compressing {}", fmt.to_uppercase()), ctx.pt, move |job| {
        job.progress(-1.0);
        let names: Vec<String> = items.iter().map(|p| name(p)).collect();
        match fmt {
            "rar" => {
                let mut c = tools::cmd(tools::winrar().unwrap());
                c.args(["a", "-r", "-ep1", "-idq", "-y", "-m5"]).arg(&out).args(&items);
                tools::run(c)?;
            }
            "7z" => {
                let sz = tools::sevenzip(job)?;
                job.caption("Compressing 7Z");
                let mut c = tools::cmd(sz);
                c.args(["a", "-t7z", "-mx=7", "-y"]).arg(&out).args(&items);
                tools::run(c)?;
            }
            _ => {
                let mut c = tools::cmd("tar.exe");
                c.arg("-a").arg("-cf").arg(&out).arg("-C").arg(&folder).args(&names);
                if tools::run(c).is_err() {
                    let _ = std::fs::remove_file(&out);
                    // fallback for unusual file names
                    // paths go in as separate arguments, never into the script text
                    let script = "param([string]$dest)\nCompress-Archive -LiteralPath $args -DestinationPath $dest -CompressionLevel Optimal";
                    let mut args = vec![out.as_os_str()];
                    args.extend(items.iter().map(|p| p.as_os_str()));
                    tools::powershell(script, &args)?;
                }
            }
        }
        if !out.exists() {
            return Err("Compression failed".into());
        }
        let size = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
        Ok(format!("{}Saved {} · {}", if fell_back { "WinRAR not found — " } else { "" }, name(&out), util::human_size(size)))
    });
}

fn hash_file(p: &Path) -> std::io::Result<(String, String)> {
    use md5::Md5;
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut f = std::fs::File::open(p)?;
    let mut sha = Sha256::new();
    let mut md = Md5::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        sha.update(&buf[..n]);
        md.update(&buf[..n]);
    }
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    Ok((hex(&sha.finalize()), hex(&md.finalize())))
}

// ------------------------------------------------------------------ media (FFmpeg)

type Plan = Box<dyn Fn(&Path) -> Result<(Vec<String>, Vec<String>, PathBuf), String> + Send>;

fn ff_batch(ctx: &Ctx, caption: String, items: Vec<PathBuf>, plan: Plan) {
    if items.is_empty() {
        return;
    }
    jobs::spawn(caption.clone(), ctx.pt, move |job| {
        let ff = tools::ffmpeg(job)?;
        job.caption(caption.clone());
        let n = items.len();
        let (mut made, mut errs) = (Vec::new(), Vec::new());
        for (i, p) in items.iter().enumerate() {
            if n > 1 {
                job.caption(format!("{caption} · {}/{n}", i + 1));
            }
            let (pre, post, out) = match plan(p) {
                Ok(x) => x,
                Err(e) => {
                    errs.push(e);
                    continue;
                }
            };
            match tools::ffmpeg_run(job, &ff, &pre, p, &post, &out, i, n) {
                Ok(_) => made.push(out),
                Err(e) => errs.push(e),
            }
        }
        summarize(&made, &errs)
    });
}

fn has_alpha_ext(p: &Path) -> bool {
    matches!(ext(p).as_str(), "png" | "webp" | "gif" | "ico" | "tif" | "tiff" | "avif" | "tga" | "psd")
}

/// Composite transparent pixels onto white (for formats without alpha).
fn flatten_alpha_args() -> Vec<String> {
    s(&["-filter_complex", "[0:v]format=rgba,split[a][b];[a]drawbox=c=white@1:replace=1:t=fill[bg];[bg][b]overlay=format=auto,format=rgb24[o]", "-map", "[o]"])
}

fn image_out_args(input: &Path, out_ext: &str, vf: Option<String>) -> Vec<String> {
    let mut a = Vec::new();
    let needs_flatten = matches!(out_ext, "jpg" | "jpeg" | "bmp") && has_alpha_ext(input);
    if needs_flatten {
        let mut fc = "[0:v]format=rgba,split[a][b];[a]drawbox=c=white@1:replace=1:t=fill[bg];[bg][b]overlay=format=auto,format=rgb24".to_string();
        if let Some(vf) = &vf {
            fc.push(',');
            fc.push_str(vf);
        }
        fc.push_str("[o]");
        a.extend(s(&["-filter_complex", &fc, "-map", "[o]"]));
    } else if let Some(vf) = vf {
        a.extend(["-vf".to_string(), vf]);
    }
    match out_ext {
        "jpg" | "jpeg" => a.extend(s(&["-q:v", "2", "-frames:v", "1"])),
        "png" | "bmp" | "tiff" => a.extend(s(&["-frames:v", "1"])),
        "ico" => {}
        "webp" => a.extend(s(&["-quality", "90", "-loop", "0"])),
        "avif" => a.extend(s(&["-crf", "28", "-still-picture", "1", "-frames:v", "1"])),
        "gif" => {}
        _ => {}
    }
    if out_ext == "tiff" {
        a.extend(s(&["-f", "image2"]));
    }
    a
}

fn image_op(ctx: &Ctx, op: Img) {
    let items = files_of(ctx);
    let short_side = |h: u32| format!("scale=w='if(gte(iw,ih),-1,{h})':h='if(gte(iw,ih),{h},-1)'");
    let (caption, plan): (String, Plan) = match op {
        Img::Convert(e) => (
            format!("To {}", e.to_uppercase()),
            Box::new(move |p: &Path| {
                let vf = (e == "ico").then(|| "scale=w='min(256,iw)':h='min(256,ih)':force_original_aspect_ratio=decrease".to_string());
                Ok((vec![], image_out_args(p, e, vf), sibling(p, "", e)))
            }),
        ),
        Img::Height(h) => {
            let label = match h {
                720 => "720p",
                1080 => "1080p",
                1440 => "2K",
                2160 => "4K",
                _ => "resized",
            };
            let vf = short_side(h);
            (
                format!("Resize {label}"),
                Box::new(move |p: &Path| {
                    let e = ext(p);
                    Ok((vec![], image_out_args(p, &e, Some(format!("{vf}:flags=lanczos"))), sibling(p, &format!("_{label}"), &e)))
                }),
            )
        }
        Img::Percent(pc) => (
            format!("Resize {pc}%"),
            Box::new(move |p: &Path| {
                let e = ext(p);
                let vf = format!("scale=w='max(1,trunc(iw*{pc}/100))':h=-1:flags=lanczos");
                Ok((vec![], image_out_args(p, &e, Some(vf)), sibling(p, &format!("_{pc}pct"), &e)))
            }),
        ),
        Img::Width => {
            let Some(v) = input::prompt("Resize to width", "Width in pixels — height keeps the aspect ratio", "1200") else { return };
            let Ok(w) = v.trim().trim_end_matches("px").parse::<u32>() else {
                crate::hud::toast("Enter a number, e.g. 1200", ToastKind::Error);
                return;
            };
            (
                format!("Width {w}px"),
                Box::new(move |p: &Path| {
                    let e = ext(p);
                    Ok((vec![], image_out_args(p, &e, Some(format!("scale={w}:-1:flags=lanczos"))), sibling(p, &format!("_{w}w"), &e)))
                }),
            )
        }
        Img::Custom => {
            let Some(v) = input::prompt("Custom size", "Width × Height in pixels, e.g. 1920x1080  (use 0 to keep ratio)", "1920x1080") else { return };
            let Some((w, h)) = parse_size(&v) else {
                crate::hud::toast("Use the format 1920x1080", ToastKind::Error);
                return;
            };
            let (ws, hs) = (if w == 0 { "-1".to_string() } else { w.to_string() }, if h == 0 { "-1".to_string() } else { h.to_string() });
            (
                format!("Resize {}x{}", w, h),
                Box::new(move |p: &Path| {
                    let e = ext(p);
                    Ok((vec![], image_out_args(p, &e, Some(format!("scale={ws}:{hs}:flags=lanczos"))), sibling(p, &format!("_{w}x{h}"), &e)))
                }),
            )
        }
        Img::Compress => (
            "Compressing".into(),
            Box::new(|p: &Path| {
                let e = ext(p);
                let (args, out_ext) = match e.as_str() {
                    "jpg" | "jpeg" | "jfif" => (s(&["-q:v", "7", "-map_metadata", "-1"]), "jpg"),
                    "png" => (
                        s(&["-vf", "split[a][b];[a]palettegen=max_colors=256:reserve_transparent=1:stats_mode=full[p];[b][p]paletteuse=dither=sierra2_4a:alpha_threshold=128", "-frames:v", "1"]),
                        "png",
                    ),
                    "webp" => (s(&["-quality", "72", "-loop", "0"]), "webp"),
                    "gif" => (s(&["-vf", "split[a][b];[a]palettegen=max_colors=128:stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=3", "-loop", "0"]), "gif"),
                    _ => {
                        let mut a = image_out_args(p, "jpg", None);
                        a.retain(|x| x != "2");
                        a.extend(s(&["-q:v", "6"]));
                        (a, "jpg")
                    }
                };
                Ok((vec![], args, sibling(p, "_compressed", out_ext)))
            }),
        ),
        Img::Rotate(f, sfx) => (
            "Rotating".into(),
            Box::new(move |p: &Path| {
                let e = ext(p);
                Ok((vec![], image_out_args(p, &e, Some(f.to_string())), sibling(p, sfx, &e)))
            }),
        ),
        Img::Strip => (
            "Cleaning".into(),
            Box::new(|p: &Path| {
                let e = ext(p);
                let mut a = image_out_args(p, &e, None);
                a.extend(s(&["-map_metadata", "-1"]));
                Ok((vec![], a, sibling(p, "_clean", &e)))
            }),
        ),
        Img::Combine(mode) => {
            combine(ctx, items, mode);
            return;
        }
        Img::Favicon => {
            favicon(ctx, items);
            return;
        }
    };
    ff_batch(ctx, caption, items, plan);
}

fn parse_size(v: &str) -> Option<(u32, u32)> {
    let v = v.to_lowercase().replace(['×', '*', ',', ' '], "x");
    let parts: Vec<&str> = v.split('x').filter(|p| !p.is_empty()).collect();
    if parts.len() != 2 {
        return None;
    }
    let w = parts[0].trim().parse().ok()?;
    let h = parts[1].trim().parse().ok()?;
    (w > 0 || h > 0).then_some((w, h))
}

fn favicon(ctx: &Ctx, items: Vec<PathBuf>) {
    let Some(src) = items.into_iter().next() else { return };
    jobs::spawn("Favicons", ctx.pt, move |job| {
        let ff = tools::ffmpeg(job)?;
        job.caption("Favicons");
        let dir = unique(src.with_file_name(format!("{}_favicon", stem(&src))));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let sizes = [(16, "favicon-16x16.png"), (32, "favicon-32x32.png"), (48, "favicon-48x48.png"), (180, "apple-touch-icon.png"), (192, "android-chrome-192x192.png"), (512, "android-chrome-512x512.png"), (48, "favicon.ico")];
        for (i, (sz, fname)) in sizes.iter().enumerate() {
            let vf = format!("format=rgba,scale={sz}:{sz}:force_original_aspect_ratio=decrease:flags=lanczos,pad={sz}:{sz}:(ow-iw)/2:(oh-ih)/2:color=0x00000000");
            tools::ffmpeg_run(job, &ff, &[], &src, &s(&["-vf", &vf, "-frames:v", "1"]), &dir.join(fname), i, sizes.len())?;
        }
        Ok(format!("Saved {} (7 icons)", name(&dir)))
    });
}

fn audio_codec(e: &str) -> Vec<String> {
    match e {
        "mp3" => s(&["-c:a", "libmp3lame", "-b:a", "192k"]),
        "mp3_128" => s(&["-c:a", "libmp3lame", "-b:a", "128k"]),
        "mp3_320" => s(&["-c:a", "libmp3lame", "-b:a", "320k"]),
        "wav" => s(&["-c:a", "pcm_s16le"]),
        "flac" => s(&["-c:a", "flac"]),
        "m4a" | "aac" => s(&["-c:a", "aac", "-b:a", "192k"]),
        "ogg" | "oga" => s(&["-c:a", "libvorbis", "-q:a", "6"]),
        "opus" => s(&["-c:a", "libopus", "-b:a", "128k"]),
        "wma" => s(&["-c:a", "wmav2", "-b:a", "192k"]),
        "aiff" | "aif" => s(&["-c:a", "pcm_s16be"]),
        _ => vec![],
    }
}

fn x264(crf: &str) -> Vec<String> {
    s(&["-c:v", "libx264", "-preset", "medium", "-crf", crf, "-pix_fmt", "yuv420p"])
}

/// Parse "1:25", "85", "01:02:03.5" into seconds.
fn parse_time(t: &str) -> Option<f64> {
    let t = t.trim();
    if t.is_empty() {
        return None;
    }
    let mut secs = 0.0;
    for part in t.split(':') {
        secs = secs * 60.0 + part.trim().parse::<f64>().ok()?;
    }
    Some(secs)
}

fn media_op(ctx: &Ctx, op: Media) {
    let items = files_of(ctx);
    let (caption, plan): (String, Plan) = match op {
        Media::VConvert(e) => (
            format!("To {}", e.to_uppercase()),
            Box::new(move |p: &Path| {
                let mut a = match e {
                    "mp4" => {
                        let mut v = x264("20");
                        v.extend(s(&["-c:a", "aac", "-b:a", "192k", "-movflags", "+faststart"]));
                        v
                    }
                    "mov" => {
                        let mut v = x264("20");
                        v.extend(s(&["-c:a", "aac", "-b:a", "192k"]));
                        v
                    }
                    "mkv" => s(&["-map", "0:v?", "-map", "0:a?", "-c", "copy"]),
                    "webm" => s(&["-c:v", "libvpx-vp9", "-crf", "32", "-b:v", "0", "-row-mt", "1", "-deadline", "good", "-cpu-used", "4", "-c:a", "libopus", "-b:a", "128k"]),
                    "avi" => s(&["-c:v", "mpeg4", "-vtag", "xvid", "-q:v", "3", "-c:a", "libmp3lame", "-q:a", "3"]),
                    _ => vec![],
                };
                a.extend(s(&["-map_metadata", "0"]));
                Ok((vec![], a, sibling(p, "", e)))
            }),
        ),
        Media::ExtractAudio(e) => (
            format!("To {}", e.to_uppercase()),
            Box::new(move |p: &Path| {
                if !tools::has_audio(p) {
                    return Err(format!("{} has no audio track", name(p)));
                }
                let mut a = s(&["-vn", "-sn"]);
                a.extend(audio_codec(e));
                Ok((vec![], a, sibling(p, "", e)))
            }),
        ),
        Media::AConvert(k) => {
            let e = k.split('_').next().unwrap();
            let suffix = match k {
                "mp3_128" => "_128k",
                "mp3_320" => "_320k",
                _ => "",
            };
            (
                format!("To {}", e.to_uppercase()),
                Box::new(move |p: &Path| {
                    let mut a = s(&["-vn", "-sn", "-map_metadata", "0"]);
                    a.extend(audio_codec(k));
                    let e = k.split('_').next().unwrap();
                    Ok((vec![], a, sibling(p, suffix, e)))
                }),
            )
        }
        Media::VResize(h) => (
            format!("Resize {h}p"),
            Box::new(move |p: &Path| {
                let mut a = s(&["-vf", &format!("scale=w='if(gte(iw,ih),-2,{h})':h='if(gte(iw,ih),{h},-2)':flags=lanczos")]);
                a.extend(x264("20"));
                a.extend(s(&["-c:a", "aac", "-b:a", "192k", "-movflags", "+faststart"]));
                Ok((vec![], a, sibling(p, &format!("_{h}p"), "mp4")))
            }),
        ),
        Media::VCompress => (
            "Compressing".into(),
            Box::new(|p: &Path| {
                let mut a = x264("28");
                a.extend(s(&["-c:a", "aac", "-b:a", "128k", "-movflags", "+faststart"]));
                Ok((vec![], a, sibling(p, "_compressed", "mp4")))
            }),
        ),
        Media::Trim => {
            let Some(v) = input::prompt("Trim", "Start - End, e.g.  0:10 - 1:25   (leave End empty for the rest)", "0:00 - ") else { return };
            let v = v.replace(['–', '—'], "-").replace(" to ", "-");
            let mut parts = v.splitn(2, '-');
            let start = parts.next().and_then(parse_time).unwrap_or(0.0);
            let end = parts.next().and_then(parse_time);
            if let Some(e) = end {
                if e <= start {
                    crate::hud::toast("End must be after start", ToastKind::Error);
                    return;
                }
            }
            (
                "Trimming".into(),
                Box::new(move |p: &Path| {
                    let e = ext(p);
                    let mut post = Vec::new();
                    if let Some(end) = end {
                        post.extend(["-t".to_string(), format!("{:.3}", end - start)]);
                    }
                    if kind_of(p) == FileKind::Video {
                        if matches!(e.as_str(), "mp4" | "mov" | "mkv" | "m4v") {
                            post.extend(x264("18"));
                            post.extend(s(&["-c:a", "aac", "-b:a", "192k"]));
                        } else {
                            post.extend(s(&["-c", "copy"]));
                        }
                    } else {
                        post.extend(audio_codec(&e));
                    }
                    Ok((vec!["-ss".into(), format!("{start:.3}")], post, sibling(p, "_trim", &e)))
                }),
            )
        }
        Media::Frame => (
            "Grabbing frame".into(),
            Box::new(|p: &Path| {
                let mid = tools::duration(p).map(|d| d / 2.0).unwrap_or(1.0);
                Ok((vec!["-ss".into(), format!("{mid:.3}")], s(&["-frames:v", "1"]), sibling(p, "_frame", "png")))
            }),
        ),
        Media::Mute => (
            "Muting".into(),
            Box::new(|p: &Path| {
                let e = ext(p);
                Ok((vec![], s(&["-an", "-c:v", "copy"]), sibling(p, "_muted", &e)))
            }),
        ),
        Media::ToGif => (
            "To GIF".into(),
            Box::new(|p: &Path| {
                let vf = "fps=12,scale=w='min(480,iw)':h=-1:flags=lanczos,split[a][b];[a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=4";
                Ok((vec![], s(&["-vf", vf, "-loop", "0"]), sibling(p, "", "gif")))
            }),
        ),
        Media::Speed(pct) => {
            speed(ctx, items, pct);
            return;
        }
        Media::Merge => {
            merge_media(ctx, items);
            return;
        }
        Media::Volume(pct) => {
            let pct = if pct > 0 {
                pct
            } else {
                let Some(v) = input::prompt("Volume", "Percent of the current volume, e.g. 130 = louder, 70 = quieter", "130") else { return };
                match v.trim().trim_end_matches('%').parse::<u32>() {
                    Ok(p) if p > 0 && p <= 1000 => p,
                    _ => {
                        crate::hud::toast("Enter a percent between 1 and 1000", ToastKind::Error);
                        return;
                    }
                }
            };
            (
                format!("Volume {pct}%"),
                Box::new(move |p: &Path| {
                    let e = ext(p);
                    let mut a = s(&["-af", &format!("volume={:.2}", pct as f64 / 100.0)]);
                    if kind_of(p) == FileKind::Video {
                        a.extend(s(&["-c:v", "copy"]));
                        a.extend(audio_codec("m4a"));
                    } else {
                        a.extend(audio_codec(&e));
                    }
                    Ok((vec![], a, sibling(p, &format!("_vol{pct}"), &e)))
                }),
            )
        }
        Media::VRotate(f) => (
            "Rotating".into(),
            Box::new(move |p: &Path| {
                let mut a = s(&["-vf", f]);
                a.extend(x264("20"));
                a.extend(s(&["-c:a", "aac", "-b:a", "192k", "-movflags", "+faststart"]));
                Ok((vec![], a, sibling(p, "_rotated", &video_out_ext(p))))
            }),
        ),
        Media::Normalize => (
            "Normalizing".into(),
            Box::new(|p: &Path| {
                let e = ext(p);
                let mut a = s(&["-af", "loudnorm=I=-16:TP=-1.5:LRA=11"]);
                if kind_of(p) == FileKind::Video {
                    a.extend(s(&["-c:v", "copy"]));
                    a.extend(audio_codec("m4a"));
                } else {
                    a.extend(audio_codec(&e));
                }
                Ok((vec![], a, sibling(p, "_normalized", &e)))
            }),
        ),
    };
    ff_batch(ctx, caption, items, plan);
}

// ------------------------------------------------------------------ documents

const OFFICE_PS: &str = r#"
param([string]$app,[string]$in,[string]$out,[string]$fmt)
$ErrorActionPreference = 'Stop'
switch ($app) {
  'Word' {
    $w = New-Object -ComObject Word.Application
    $w.Visible = $false; $w.DisplayAlerts = 0
    try {
      $d = $w.Documents.Open($in, $false, $true, $false)
      $d.SaveAs2($out, [int]$fmt)
      $d.Close(0)
    } finally { if ($w.Documents.Count -eq 0) { $w.Quit(0) } }
  }
  'PowerPoint' {
    $p = New-Object -ComObject PowerPoint.Application
    try {
      $pr = $p.Presentations.Open($in, -1, 0, 0)
      if ($fmt -eq 'png') { $pr.Export($out, 'PNG') } else { $pr.SaveAs($out, [int]$fmt) }
      $pr.Close()
    } finally { if ($p.Presentations.Count -eq 0) { $p.Quit() } }
  }
  'Excel' {
    $x = New-Object -ComObject Excel.Application
    $x.Visible = $false; $x.DisplayAlerts = $false
    try {
      $wb = $x.Workbooks.Open($in, 0, $true)
      if ($fmt -eq 'pdf') { $wb.ExportAsFixedFormat(0, $out) } else { $wb.SaveAs($out, [int]$fmt) }
      $wb.Close($false)
    } finally { if ($x.Workbooks.Count -eq 0) { $x.Quit() } }
  }
}
"#;

/// (Office app, Office format code, LibreOffice filter)
fn office_plan(kind: FileKind, target: &str) -> Option<(&'static str, &'static str, &'static str)> {
    let r = match (kind, target) {
        (FileKind::Word | FileKind::Text | FileKind::Pdf, "pdf") => ("Word", "17", "pdf"),
        (FileKind::Word | FileKind::Pdf, "docx") => ("Word", "16", "docx:MS Word 2007 XML"),
        (FileKind::Word, "txt") => ("Word", "7", "txt:Text (encoded):UTF8"),
        (FileKind::Word, "html") => ("Word", "10", "html"),
        (FileKind::Word, "odt") => ("Word", "23", "odt"),
        (FileKind::Word, "rtf") => ("Word", "6", "rtf"),
        (FileKind::Slides, "pdf") => ("PowerPoint", "32", "pdf"),
        (FileKind::Slides, "pptx") => ("PowerPoint", "24", "pptx"),
        (FileKind::Slides, "png") => ("PowerPoint", "png", ""),
        (FileKind::Sheet, "pdf") => ("Excel", "pdf", "pdf"),
        (FileKind::Sheet, "csv") => ("Excel", "62", "csv"),
        (FileKind::Sheet, "xlsx") => ("Excel", "51", "xlsx"),
        _ => return None,
    };
    Some(r)
}

fn convert_doc(job: &Job, input: &Path, target: &str, out: &Path) -> Result<(), String> {
    let kind = kind_of(input);
    let (app, code, lo_filter) = office_plan(kind, target).ok_or("This conversion isn't supported")?;
    // Plain-text sources go straight to LibreOffice when Word isn't around.
    let office = tools::office_has(app);
    let mut office_err = None;
    if office {
        job.caption(format!("{app} → {}", target.to_uppercase()));
        job.progress(-1.0);
        if app == "Word" && kind == FileKind::Pdf {
            disable_pdf_reflow_prompt();
        }
        let r = tools::powershell(OFFICE_PS, &[std::ffi::OsStr::new(app), input.as_os_str(), out.as_os_str(), std::ffi::OsStr::new(code)]);
        match r {
            Ok(_) if out.exists() => return Ok(()),
            Ok(_) => office_err = Some(format!("{app} couldn't convert this file")),
            Err(e) => office_err = Some(format!("{app}: {e}")),
        }
    }
    if lo_filter.is_empty() {
        return Err(office_err.unwrap_or_else(|| format!("This needs Microsoft {app}")));
    }
    let infilter = (kind == FileKind::Pdf).then_some("writer_pdf_import");
    tools::soffice_convert(job, input, lo_filter, infilter, out).map_err(|e| office_err.map(|o| format!("{o} · {e}")).unwrap_or(e))
}

fn disable_pdf_reflow_prompt() {
    use winreg::enums::*;
    for v in ["16.0", "15.0"] {
        if let Ok((k, _)) = winreg::RegKey::predef(HKEY_CURRENT_USER).create_subkey(format!("Software\\Microsoft\\Office\\{v}\\Word\\Options")) {
            let _ = k.set_value("DisableConvertPdfWarning", &1u32);
        }
    }
}

fn doc_op(ctx: &Ctx, op: Doc) {
    let items = files_of(ctx);
    let (caption, target) = match op {
        Doc::Office(t) => (format!("To {}", t.to_uppercase()), t),
        Doc::PdfToWord => ("To Word".to_string(), "docx"),
        Doc::SlidesImages => ("To images".to_string(), "png"),
    };
    jobs::spawn(caption, ctx.pt, move |job| {
        let (mut made, mut errs) = (Vec::new(), Vec::new());
        for f in &items {
            let out = if target == "png" { unique(f.with_file_name(format!("{}_slides", stem(f)))) } else { sibling(f, "", target) };
            match convert_doc(job, f, target, &out) {
                Ok(_) => made.push(out),
                Err(e) => errs.push(e),
            }
        }
        summarize(&made, &errs)
    });
}

// ------------------------------------------------------------------ added tools

/// Keep the container when it can hold H.264/AAC, otherwise use MP4.
fn video_out_ext(p: &Path) -> String {
    let e = ext(p);
    if matches!(e.as_str(), "mp4" | "mov" | "mkv" | "m4v") { e } else { "mp4".into() }
}

fn image_dims(p: &Path, data: &[u8]) -> Option<(u32, u32)> {
    match ext(p).as_str() {
        "png" if data.len() > 24 => Some((u32::from_be_bytes(data[16..20].try_into().ok()?), u32::from_be_bytes(data[20..24].try_into().ok()?))),
        "jpg" | "jpeg" | "jfif" => crate::pdf::jpeg_size(data).map(|(w, h, _)| (w, h)),
        "gif" if data.len() > 10 => Some((u16::from_le_bytes([data[6], data[7]]) as u32, u16::from_le_bytes([data[8], data[9]]) as u32)),
        "bmp" if data.len() > 26 => Some((i32::from_le_bytes(data[18..22].try_into().ok()?).unsigned_abs(), i32::from_le_bytes(data[22..26].try_into().ok()?).unsigned_abs())),
        _ => tools::video_size(p),
    }
}

/// Size, camera, settings and GPS for the selected images; details go to the clipboard.
fn image_info(files: Vec<PathBuf>) {
    std::thread::spawn(move || {
        let mut toast = Vec::new();
        let mut clip = Vec::new();
        for f in files.iter().take(4) {
            let Ok(data) = std::fs::read(f) else { continue };
            let mut lines = Vec::new();
            let mut head = format!("{} · {}", name(f), util::human_size(data.len() as u64));
            if let Some((w, h)) = image_dims(f, &data) {
                head += &format!(" · {w}×{h} ({:.1} MP)", (w as f64 * h as f64) / 1e6);
            }
            lines.push(head);
            let mut map = None;
            if let Some(x) = crate::exif::read(&data) {
                let camera = match (&x.make, &x.model) {
                    (Some(m), Some(md)) if md.to_lowercase().starts_with(&m.to_lowercase()) => Some(md.clone()),
                    (Some(m), Some(md)) => Some(format!("{m} {md}")),
                    (a, b) => a.clone().or(b.clone()),
                };
                let when = x.date.as_ref().map(|d| d.replacen(':', "-", 2));
                let cam_line = [camera, when].into_iter().flatten().collect::<Vec<_>>().join(" · ");
                if !cam_line.is_empty() {
                    lines.push(cam_line);
                }
                let mut set = Vec::new();
                if let Some(v) = x.fnumber {
                    set.push(format!("f/{v:.1}"));
                }
                if let Some(v) = &x.exposure {
                    set.push(v.clone());
                }
                if let Some(v) = x.iso {
                    set.push(format!("ISO {v}"));
                }
                if let Some(v) = x.focal {
                    set.push(format!("{v:.0} mm"));
                }
                if !set.is_empty() {
                    lines.push(set.join(" · "));
                }
                if let Some((lat, lon)) = x.gps {
                    lines.push(format!("GPS {lat:.5}, {lon:.5}"));
                    map = Some(format!("https://maps.google.com/?q={lat:.6},{lon:.6}"));
                }
            }
            if lines.len() == 1 {
                lines.push("No camera data (EXIF)".into());
            }
            toast.push(lines.join("\n"));
            clip.push(lines.join("\r\n") + &map.map(|m| format!("\r\nMap: {m}")).unwrap_or_default());
        }
        let has_map = clip.iter().any(|c| c.contains("Map: "));
        let text = clip.join("\r\n\r\n");
        jobs::post_ui(move || {
            util::set_clipboard_text(&text);
        });
        let suffix = if has_map { "\nCopied, including a map link" } else { "\nCopied to clipboard" };
        jobs::toast(toast.join("\n\n") + suffix, ToastKind::Info);
    });
}

fn pick_folder(start: &Path) -> Option<PathBuf> {
    use windows::core::{w, HSTRING};
    use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};
    unsafe {
        let d: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let opts = d.GetOptions().ok()?;
        d.SetOptions(opts | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM).ok()?;
        let _ = d.SetTitle(w!("Choose destination folder"));
        if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(start), None) {
            let _ = d.SetFolder(&item);
        }
        d.Show(None).ok()?;
        let p = d.GetResult().ok()?.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as _));
        s.map(PathBuf::from)
    }
}

fn send_to(ctx: &Ctx, dest: Option<PathBuf>, mv: bool) {
    let Some(dest) = dest.or_else(|| pick_folder(&ctx.folder)) else { return };
    let items = targets(ctx);
    if items.is_empty() {
        return;
    }
    crate::config::push_recent(&dest);
    let msg = format!("{} {} to {}", if mv { "Moved" } else { "Copied" }, util::plural(items.len(), "item"), name(&dest));
    let op = if mv { crate::fileops::Op::Move } else { crate::fileops::Op::Copy };
    crate::fileops::run(op, items, Some(dest), msg);
}

/// Several images into one picture: "grid", "h" (one row) or "v" (one column).
fn combine(ctx: &Ctx, mut items: Vec<PathBuf>, mode: &'static str) {
    if items.len() < 2 {
        crate::hud::toast("Select 2 or more images to combine", ToastKind::Info);
        return;
    }
    items.sort_by_key(|p| natural_key(&name(p)));
    let out = unique(ctx.folder.join("Collage.png"));
    jobs::spawn("Combining", ctx.pt, move |job| {
        let ff = tools::ffmpeg(job)?;
        job.caption("Combining");
        job.progress(-1.0);
        let n = items.len();
        let mut args = Vec::new();
        for p in &items {
            args.extend(["-i".to_string(), p.to_string_lossy().to_string()]);
        }
        let mut fc = String::new();
        let labels: String = (0..n).map(|i| format!("[s{i}]")).collect();
        match mode {
            "h" => {
                for i in 0..n {
                    fc += &format!("[{i}:v]scale=-2:900:flags=lanczos,format=rgba,setsar=1[s{i}];");
                }
                fc += &format!("{labels}hstack=inputs={n}[o]");
            }
            "v" => {
                for i in 0..n {
                    fc += &format!("[{i}:v]scale=1200:-2:flags=lanczos,format=rgba,setsar=1[s{i}];");
                }
                fc += &format!("{labels}vstack=inputs={n}[o]");
            }
            _ => {
                let cols = (n as f64).sqrt().ceil() as usize;
                let cell = 800;
                for i in 0..n {
                    fc += &format!(
                        "[{i}:v]scale={cell}:{cell}:force_original_aspect_ratio=decrease:flags=lanczos,format=rgba,pad={cell}:{cell}:(ow-iw)/2:(oh-ih)/2:color=white,setsar=1[s{i}];"
                    );
                }
                let layout = (0..n).map(|i| format!("{}_{}", (i % cols) * cell, (i / cols) * cell)).collect::<Vec<_>>().join("|");
                fc += &format!("{labels}xstack=inputs={n}:layout={layout}:fill=white[o]");
            }
        }
        args.extend(["-filter_complex".to_string(), fc, "-map".into(), "[o]".into(), "-frames:v".into(), "1".into()]);
        tools::ffmpeg_exec(job, &ff, &args, &out, None, 0, 1)?;
        Ok(format!("Saved {}", name(&out)))
    });
}

/// Speed up / slow down video or audio (pct 50 = half speed, 200 = double).
fn speed(ctx: &Ctx, items: Vec<PathBuf>, pct: u32) {
    let f = pct as f64 / 100.0;
    let label = format!("{f}x");
    jobs::spawn(format!("Speed {label}"), ctx.pt, move |job| {
        let ff = tools::ffmpeg(job)?;
        let (mut made, mut errs) = (Vec::new(), Vec::new());
        for (i, p) in items.iter().enumerate() {
            let video = kind_of(p) == FileKind::Video;
            let mut args = s(&["-i", &p.to_string_lossy()]);
            let out_ext = if video { video_out_ext(p) } else { ext(p) };
            if video {
                if tools::has_audio(p) {
                    args.extend(s(&["-filter_complex", &format!("[0:v]setpts=PTS/{f}[v];[0:a]atempo={f}[a]"), "-map", "[v]", "-map", "[a]"]));
                    args.extend(s(&["-c:a", "aac", "-b:a", "192k"]));
                } else {
                    args.extend(s(&["-vf", &format!("setpts=PTS/{f}"), "-an"]));
                }
                args.extend(x264("20"));
            } else {
                args.extend(s(&["-af", &format!("atempo={f}")]));
                args.extend(audio_codec(&out_ext));
            }
            let out = sibling(p, &format!("_{label}"), &out_ext);
            // output length changes with speed; scale the expected duration for the ring
            let dur = tools::duration(p).map(|d| d / f);
            match tools::ffmpeg_exec(job, &ff, &args, &out, dur, i, items.len()) {
                Ok(_) => made.push(out),
                Err(e) => errs.push(e),
            }
        }
        summarize(&made, &errs)
    });
}

/// Join selected clips (video or audio) in name order.
fn merge_media(ctx: &Ctx, mut items: Vec<PathBuf>) {
    if items.len() < 2 {
        crate::hud::toast("Select 2 or more clips to merge", ToastKind::Info);
        return;
    }
    items.sort_by_key(|p| natural_key(&name(p)));
    let video = items.iter().any(|p| kind_of(p) == FileKind::Video);
    let out_ext = if video { "mp4".to_string() } else { ext(&items[0]) };
    let out = unique(ctx.folder.join(format!("Merged.{out_ext}")));
    jobs::spawn("Merging", ctx.pt, move |job| {
        let ff = tools::ffmpeg(job)?;
        job.caption("Merging");
        let n = items.len();
        let total: f64 = items.iter().filter_map(|p| tools::duration(p)).sum();
        let mut args = Vec::new();
        for p in &items {
            args.extend(["-i".to_string(), p.to_string_lossy().to_string()]);
        }
        let mut fc = String::new();
        if video {
            // every clip is fitted into the first clip's frame size
            let (w, h) = tools::video_size(&items[0]).unwrap_or((1920, 1080));
            let (w, h) = (w / 2 * 2, h / 2 * 2);
            let with_audio = items.iter().all(|p| tools::has_audio(p));
            let mut chain = String::new();
            for i in 0..n {
                fc += &format!("[{i}:v]scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,setsar=1,fps=30,format=yuv420p[v{i}];");
                chain += &format!("[v{i}]");
                if with_audio {
                    fc += &format!("[{i}:a]aresample=48000,aformat=channel_layouts=stereo[a{i}];");
                    chain += &format!("[a{i}]");
                }
            }
            let a = if with_audio { 1 } else { 0 };
            fc += &format!("{chain}concat=n={n}:v=1:a={a}[v]{}", if with_audio { "[a]" } else { "" });
            args.extend(s(&["-filter_complex", &fc, "-map", "[v]"]));
            if with_audio {
                args.extend(s(&["-map", "[a]", "-c:a", "aac", "-b:a", "192k"]));
            }
            args.extend(x264("20"));
            args.extend(s(&["-movflags", "+faststart"]));
        } else {
            let mut chain = String::new();
            for i in 0..n {
                fc += &format!("[{i}:a]aresample=48000,aformat=channel_layouts=stereo[a{i}];");
                chain += &format!("[a{i}]");
            }
            fc += &format!("{chain}concat=n={n}:v=0:a=1[a]");
            args.extend(s(&["-filter_complex", &fc, "-map", "[a]"]));
            args.extend(audio_codec(&out_ext));
        }
        tools::ffmpeg_exec(job, &ff, &args, &out, Some(total), 0, 1)?;
        Ok(format!("Saved {}", name(&out)))
    });
}
