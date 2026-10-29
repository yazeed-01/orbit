//! External tools: discovery, one-time auto download, and hidden process execution.

use std::io::{BufRead, BufReader, Read};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use crate::hud::ToastKind;
use crate::jobs::{self, Job};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const FFMPEG_URL: &str = "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl.zip";
const SEVENZR_URL: &str = "https://www.7-zip.org/a/7zr.exe";
const LIBREOFFICE_INDEX: &str = "https://download.documentfoundation.org/libreoffice/stable/";

static DOWNLOAD_LOCK: Mutex<()> = Mutex::new(());

pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let d = base.join("Orbit");
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn tools_dir() -> PathBuf {
    let d = data_dir().join("tools");
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn temp_dir() -> PathBuf {
    let d = data_dir().join("tmp");
    let _ = std::fs::create_dir_all(&d);
    d
}

/// A Command that never flashes a console window.
pub fn cmd(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut c = Command::new(program);
    c.creation_flags(CREATE_NO_WINDOW).stdin(Stdio::null());
    c
}

/// Run to completion; Ok(stdout) on success, Err(readable message) otherwise.
pub fn run(mut c: Command) -> Result<String, String> {
    let out = c.stdout(Stdio::piped()).stderr(Stdio::piped()).output().map_err(|e| format!("Couldn't start tool: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if out.status.success() {
        Ok(stdout)
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let msg = last_lines(if err.trim().is_empty() { &stdout } else { &err }, 3);
        Err(if msg.is_empty() { format!("Tool failed (exit code {})", out.status.code().unwrap_or(-1)) } else { msg })
    }
}

fn last_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// Search PATH for an executable.
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| p.is_file())
}

fn find_file(dir: &Path, name: &str, depth: u32) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut subdirs = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            subdirs.push(p);
        } else if p.file_name().is_some_and(|n| n.eq_ignore_ascii_case(name)) {
            return Some(p);
        }
    }
    if depth == 0 {
        return None;
    }
    subdirs.into_iter().find_map(|d| find_file(&d, name, depth - 1))
}

/// Download `url` to `dest`, reporting progress on the job's ring.
pub fn download(job: &Job, url: &str, dest: &Path, label: &str) -> Result<(), String> {
    jobs::toast(format!("Downloading {label} (one-time setup)…"), ToastKind::Wait);
    job.caption(format!("Getting {label}"));
    job.progress(0.0);
    let total = head_length(url);
    let part = dest.with_extension("part");
    let _ = std::fs::remove_file(&part);
    let mut child = cmd("curl.exe")
        .args(["-L", "--fail", "-s", "--retry", "2", "-o"])
        .arg(&part)
        .arg(url)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Couldn't start download: {e}"))?;
    let status = loop {
        if let Some(st) = child.try_wait().map_err(|e| e.to_string())? {
            break st;
        }
        if let Some(total) = total {
            let got = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
            job.progress((got as f64 / total as f64).min(0.99) as f32);
        } else {
            job.progress(-1.0);
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    jobs::post_ui(crate::hud::dismiss_wait);
    if !status.success() {
        let _ = std::fs::remove_file(&part);
        return Err(format!("Couldn't download {label}. Check your internet connection."));
    }
    std::fs::rename(&part, dest).map_err(|e| e.to_string())?;
    job.progress(-1.0);
    Ok(())
}

fn head_length(url: &str) -> Option<u64> {
    let out = run({
        let mut c = cmd("curl.exe");
        c.args(["-sIL", url]);
        c
    })
    .ok()?;
    out.lines()
        .filter_map(|l| {
            let l = l.to_ascii_lowercase();
            l.strip_prefix("content-length:").and_then(|v| v.trim().parse::<u64>().ok())
        })
        .filter(|&n| n > 0)
        .last()
}

fn extract_zip(zip: &Path, dest: &Path) -> Result<(), String> {
    let _ = std::fs::create_dir_all(dest);
    let mut c = cmd("tar.exe");
    c.arg("-xf").arg(zip).arg("-C").arg(dest);
    run(c).map(|_| ())
}

// ---------------------------------------------------------------- FFmpeg

pub fn ffmpeg_path() -> Option<PathBuf> {
    which("ffmpeg.exe").or_else(|| find_file(&tools_dir().join("ffmpeg"), "ffmpeg.exe", 3))
}

pub fn ffprobe_path() -> Option<PathBuf> {
    if let Some(ff) = ffmpeg_path() {
        let p = ff.with_file_name("ffprobe.exe");
        if p.is_file() {
            return Some(p);
        }
    }
    which("ffprobe.exe")
}

pub fn ffmpeg(job: &Job) -> Result<PathBuf, String> {
    if let Some(p) = ffmpeg_path() {
        return Ok(p);
    }
    let _guard = DOWNLOAD_LOCK.lock().unwrap();
    if let Some(p) = ffmpeg_path() {
        return Ok(p);
    }
    let zip = tools_dir().join("ffmpeg.zip");
    download(job, FFMPEG_URL, &zip, "FFmpeg")?;
    job.caption("Installing FFmpeg");
    let res = extract_zip(&zip, &tools_dir().join("ffmpeg"));
    let _ = std::fs::remove_file(&zip);
    res?;
    ffmpeg_path().ok_or_else(|| "FFmpeg install failed".to_string())
}

/// Media duration in seconds.
pub fn duration(input: &Path) -> Option<f64> {
    let probe = ffprobe_path()?;
    let mut c = cmd(probe);
    c.args(["-v", "error", "-show_entries", "format=duration", "-of", "default=nw=1:nk=1"]).arg(input);
    run(c).ok()?.trim().parse().ok()
}

pub fn has_audio(input: &Path) -> bool {
    let Some(probe) = ffprobe_path() else { return true };
    let mut c = cmd(probe);
    c.args(["-v", "error", "-select_streams", "a", "-show_entries", "stream=index", "-of", "csv=p=0"]).arg(input);
    run(c).map(|s| !s.trim().is_empty()).unwrap_or(true)
}

/// Run ffmpeg on one input with progress mapped into slot `i` of `n`.
pub fn ffmpeg_run(job: &Job, ff: &Path, pre: &[String], input: &Path, post: &[String], out: &Path, i: usize, n: usize) -> Result<(), String> {
    let mut args = pre.to_vec();
    args.extend(["-i".to_string(), input.to_string_lossy().to_string()]);
    args.extend_from_slice(post);
    ffmpeg_exec(job, ff, &args, out, duration(input), i, n)
}

/// Run ffmpeg with arbitrary arguments (several `-i` inputs allowed). `dur` = expected
/// output length in seconds, used for the progress ring.
pub fn ffmpeg_exec(job: &Job, ff: &Path, args: &[String], out: &Path, dur: Option<f64>, i: usize, n: usize) -> Result<(), String> {
    let dur = dur.filter(|d| *d > 0.05);
    let mut c = cmd(ff);
    c.args(["-hide_banner", "-nostdin", "-y", "-loglevel", "error", "-progress", "pipe:1", "-nostats"])
        .args(args)
        .arg(out)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| format!("Couldn't start FFmpeg: {e}"))?;
    let mut stderr = child.stderr.take().unwrap();
    let err_thread = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let base = i as f32 / n as f32;
    if dur.is_none() {
        job.progress(if n == 1 { -1.0 } else { base });
    }
    for line in BufReader::new(child.stdout.take().unwrap()).lines().map_while(Result::ok) {
        if let (Some(d), Some(v)) = (dur, line.strip_prefix("out_time_us=").or_else(|| line.strip_prefix("out_time_ms="))) {
            if let Ok(us) = v.trim().parse::<f64>() {
                let f = (us / 1e6 / d).clamp(0.0, 1.0) as f32;
                job.progress(base + f / n as f32);
            }
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    let err = err_thread.join().unwrap_or_default();
    if status.success() && out.exists() {
        Ok(())
    } else {
        let _ = std::fs::remove_file(out);
        let msg = last_lines(&err, 2);
        Err(if msg.is_empty() { "Conversion failed".into() } else { format!("Conversion failed: {msg}") })
    }
}

// ---------------------------------------------------------------- Archivers

pub fn winrar() -> Option<PathBuf> {
    for base in [std::env::var_os("ProgramFiles"), std::env::var_os("ProgramFiles(x86)")].into_iter().flatten() {
        let p = PathBuf::from(base).join("WinRAR").join("Rar.exe");
        if p.is_file() {
            return Some(p);
        }
    }
    which("Rar.exe")
}

pub fn unrar() -> Option<PathBuf> {
    winrar().map(|p| p.with_file_name("UnRAR.exe")).filter(|p| p.is_file())
}

pub fn sevenzip(job: &Job) -> Result<PathBuf, String> {
    for base in [std::env::var_os("ProgramFiles"), std::env::var_os("ProgramFiles(x86)")].into_iter().flatten() {
        let p = PathBuf::from(base).join("7-Zip").join("7z.exe");
        if p.is_file() {
            return Ok(p);
        }
    }
    let local = tools_dir().join("7zr.exe");
    if local.is_file() {
        return Ok(local);
    }
    let _guard = DOWNLOAD_LOCK.lock().unwrap();
    if !local.is_file() {
        download(job, SEVENZR_URL, &local, "7-Zip")?;
    }
    Ok(local)
}

// ---------------------------------------------------------------- Office / LibreOffice

/// Is a Microsoft Office app registered for COM automation? ("Word", "PowerPoint", "Excel")
pub fn office_has(app: &str) -> bool {
    use winreg::enums::HKEY_CLASSES_ROOT;
    winreg::RegKey::predef(HKEY_CLASSES_ROOT).open_subkey(format!("{app}.Application\\CLSID")).is_ok()
}

fn soffice_path() -> Option<PathBuf> {
    for base in [std::env::var_os("ProgramFiles"), std::env::var_os("ProgramFiles(x86)")].into_iter().flatten() {
        let p = PathBuf::from(base).join("LibreOffice").join("program").join("soffice.exe");
        if p.is_file() {
            return Some(p);
        }
    }
    find_file(&tools_dir().join("libreoffice"), "soffice.exe", 4)
}

pub fn soffice(job: &Job) -> Result<PathBuf, String> {
    if let Some(p) = soffice_path() {
        return Ok(p);
    }
    let _guard = DOWNLOAD_LOCK.lock().unwrap();
    if let Some(p) = soffice_path() {
        return Ok(p);
    }
    job.caption("Finding LibreOffice");
    let index = run({
        let mut c = cmd("curl.exe");
        c.args(["-sL", LIBREOFFICE_INDEX]);
        c
    })
    .map_err(|_| "Couldn't reach the LibreOffice download server".to_string())?;
    let version = index
        .split("href=\"")
        .skip(1)
        .filter_map(|s| s.split('/').next())
        .filter(|v| v.split('.').count() >= 3 && v.split('.').all(|p| p.parse::<u32>().is_ok()))
        .max_by_key(|v| v.split('.').map(|p| p.parse::<u32>().unwrap_or(0)).collect::<Vec<_>>())
        .ok_or("Couldn't find a LibreOffice version to download")?
        .to_string();
    let url = format!("{LIBREOFFICE_INDEX}{version}/win/x86_64/LibreOffice_{version}_Win_x86-64.msi");
    let msi = tools_dir().join("libreoffice.msi");
    download(job, &url, &msi, "LibreOffice (~350 MB)")?;
    job.caption("Installing LibreOffice");
    let target = tools_dir().join("libreoffice");
    let mut c = cmd("msiexec.exe");
    c.arg("/a").arg(&msi).arg("/qn").arg(format!("TARGETDIR={}", target.display()));
    let res = run(c);
    let _ = std::fs::remove_file(&msi);
    res.map_err(|e| format!("LibreOffice setup failed: {e}"))?;
    soffice_path().ok_or_else(|| "LibreOffice setup failed".to_string())
}

/// Convert with LibreOffice into `out` (exact path). `filter` is the --convert-to value.
pub fn soffice_convert(job: &Job, input: &Path, filter: &str, infilter: Option<&str>, out: &Path) -> Result<(), String> {
    let so = soffice(job)?;
    job.caption("Converting");
    job.progress(-1.0);
    let tmp = temp_dir().join(format!("lo{}", std::process::id() as u64 * 1000 + job.id as u64));
    let _ = std::fs::create_dir_all(&tmp);
    let profile = format!("-env:UserInstallation=file:///{}", temp_dir().join("lo_profile").to_string_lossy().replace('\\', "/"));
    let mut c = cmd(so);
    c.arg(profile).args(["--headless", "--norestore", "--nolockcheck"]);
    if let Some(f) = infilter {
        c.arg(format!("--infilter={f}"));
    }
    c.arg("--convert-to").arg(filter).arg("--outdir").arg(&tmp).arg(input);
    let res = run(c);
    let produced = std::fs::read_dir(&tmp).ok().and_then(|mut d| d.next()).and_then(|e| e.ok()).map(|e| e.path());
    let outcome = match (res, produced) {
        (_, Some(p)) => std::fs::rename(&p, out).or_else(|_| std::fs::copy(&p, out).map(|_| ())).map_err(|e| e.to_string()),
        (Err(e), None) => Err(format!("LibreOffice couldn't convert this file: {e}")),
        (Ok(_), None) => Err("LibreOffice couldn't convert this file".into()),
    };
    let _ = std::fs::remove_dir_all(&tmp);
    outcome
}

/// Run a PowerShell script (written to a temp file) with arguments.
pub fn powershell(script: &str, args: &[&std::ffi::OsStr]) -> Result<String, String> {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = temp_dir().join(format!("orbit_{}_{n}.ps1", std::process::id()));
    // UTF-8 with BOM so Windows PowerShell 5 reads non-ASCII correctly
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(script.as_bytes());
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    let mut c = cmd("powershell.exe");
    c.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"]).arg(&path).args(args);
    let r = run(c);
    let _ = std::fs::remove_file(&path);
    r
}

// ---------------------------------------------------------------- Internet links

const CLOUDFLARED_URL: &str = "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-windows-amd64.exe";

/// Cloudflare's free tunnel client, used for "Internet link" shares.
pub fn cloudflared(job: &Job) -> Result<PathBuf, String> {
    if let Some(p) = which("cloudflared.exe") {
        return Ok(p);
    }
    let local = tools_dir().join("cloudflared.exe");
    if local.is_file() {
        return Ok(local);
    }
    let _guard = DOWNLOAD_LOCK.lock().unwrap();
    if !local.is_file() {
        download(job, CLOUDFLARED_URL, &local, "Cloudflare tunnel")?;
    }
    Ok(local)
}

/// Width x height of the first video stream.
pub fn video_size(input: &Path) -> Option<(u32, u32)> {
    let probe = ffprobe_path()?;
    let mut c = cmd(probe);
    c.args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height", "-of", "csv=p=0:s=x"]).arg(input);
    let out = run(c).ok()?;
    let (w, h) = out.trim().lines().next()?.split_once('x')?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}
