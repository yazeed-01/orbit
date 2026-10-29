//! Share files to a phone (or receive from one) over the local Wi-Fi with a QR code.

use std::cell::RefCell;
use std::io::Read;
use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::gfx::{hex, mix, rgba, Align, Surface};
use crate::hud::{ToastKind, ACCENT, GREEN, RED};
use crate::jobs::post_ui;
use crate::ui::{self, ease_out_cubic, now};
use crate::util;

const CLASS: PCWSTR = w!("OrbitShare");
const W: f32 = 360.0;
const H: f32 = 566.0;
const EXPIRY: f64 = 10.0 * 60.0;
const TIMER: usize = 1;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Send,
    Receive,
}

#[derive(Clone, Copy, PartialEq)]
enum Btn {
    None,
    Close,
    Link,
    Internet,
}

/// Largest single upload accepted from a phone.
const MAX_UPLOAD: u64 = 8 * 1024 * 1024 * 1024;
/// Wrong-link requests allowed per device before it is blocked for this share.
const MAX_BAD_REQUESTS: u32 = 20;

struct ServerState {
    stop: AtomicBool,
    downloads: AtomicU32,
    tunnel: std::sync::Mutex<Option<std::process::Child>>,
    bad: std::sync::Mutex<std::collections::HashMap<String, u32>>,
}

impl ServerState {
    fn new() -> Arc<Self> {
        Arc::new(ServerState {
            stop: AtomicBool::new(false),
            downloads: AtomicU32::new(0),
            tunnel: std::sync::Mutex::new(None),
            bad: std::sync::Mutex::new(std::collections::HashMap::new()),
        })
    }

    fn shutdown(&self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(mut c) = self.tunnel.lock().unwrap().take() {
            let _ = c.kill();
        }
    }
}


struct Win {
    hwnd: HWND,
    surf: Surface,
    scale: f32,
    mode: Mode,
    t0: f64,
    ready_at: Option<f64>,
    url: Option<String>,
    internet: Option<String>,
    going_online: bool,
    qr: Option<(Vec<bool>, usize)>,
    info: String,
    status: String,
    error: Option<String>,
    hover: Btn,
    copied_at: f64,
    closing: Option<f64>,
    server: Option<Arc<ServerState>>,
}

thread_local! {
    static WIN: RefCell<Option<Win>> = const { RefCell::new(None) };
}

pub fn register() {
    ui::register_class(CLASS, Some(proc_));
}

fn with<R>(f: impl FnOnce(&mut Win) -> R) -> Option<R> {
    WIN.with(|w| w.try_borrow_mut().ok()?.as_mut().map(f))
}

pub fn share(paths: Vec<PathBuf>) {
    if paths.is_empty() {
        return;
    }
    let info = if paths.len() == 1 { util::name(&paths[0]) } else { format!("{} items", paths.len()) };
    open(Mode::Send, info);
    let state = ServerState::new();
    with(|w| w.server = Some(state.clone()));
    std::thread::spawn(move || {
        let res = prepare_files(&paths, &state).and_then(|files| {
            let total: u64 = files.iter().map(|f| f.2).sum();
            let summary = format!("{} · {}", util::plural(files.len(), "file"), util::human_size(total));
            start_server(state.clone(), Mode::Send, files, Some(crate::config::downloads_dir())).map(|url| (url, summary))
        });
        post_ui(move || ready(res));
    });
}

pub fn receive(folder: PathBuf) {
    open(Mode::Receive, format!("Into {}", util::name(&folder)));
    let state = ServerState::new();
    with(|w| w.server = Some(state.clone()));
    std::thread::spawn(move || {
        let summary = format!("Saves into {}", util::name(&folder));
        let res = start_server(state, Mode::Receive, vec![], Some(folder)).map(|url| (url, summary));
        post_ui(move || ready(res));
    });
}

fn open(mode: Mode, info: String) {
    close_now();
    let p = ui::cursor();
    let mon = ui::monitor_at(p);
    let s = mon.scale;
    let (w, h) = ((W * s) as i32, (H * s) as i32);
    let x = (mon.work.left + mon.work.right) / 2 - w / 2;
    let y = (mon.work.top + mon.work.bottom) / 2 - h / 2;
    let Ok(surf) = Surface::new(w, h) else { return };
    let hwnd = ui::create_layered(CLASS, true, false);
    WIN.with(|wn| {
        *wn.borrow_mut() = Some(Win {
            hwnd,
            surf,
            scale: s,
            mode,
            t0: now(),
            ready_at: None,
            url: None,
            internet: None,
            going_online: false,
            qr: None,
            info,
            status: "Preparing…".into(),
            error: None,
            hover: Btn::None,
            copied_at: -10.0,
            closing: None,
            server: None,
        })
    });
    with(|w| w.paint(Some((x, y))));
    unsafe {
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, w, h, SWP_SHOWWINDOW);
        ui::force_foreground(hwnd);
        SetTimer(hwnd, TIMER, 33, None);
    }
}

fn ready(res: Result<(String, String), String>) {
    with(|w| match res {
        Ok((url, summary)) => {
            w.qr = make_qr(&url);
            w.url = Some(url);
            w.info = summary;
            w.ready_at = Some(now());
            w.status = match w.mode {
                Mode::Send => "Waiting for your phone…".into(),
                Mode::Receive => "Waiting for files…".into(),
            };
        }
        Err(e) => {
            w.error = Some(e);
            w.status = "Couldn't start sharing".into();
        }
    });
}

fn make_qr(url: &str) -> Option<(Vec<bool>, usize)> {
    qrcode::QrCode::new(url.as_bytes()).ok().map(|q| {
        let width = q.width();
        (q.to_colors().into_iter().map(|c| c == qrcode::Color::Dark).collect(), width)
    })
}

/// Start a Cloudflare quick tunnel so the share also works from other networks.
fn go_online() {
    let Some((local, state, anchor)) = with(|w| {
        if w.going_online || w.internet.is_some() {
            return None;
        }
        let mut r = windows::Win32::Foundation::RECT::default();
        unsafe {
            let _ = GetWindowRect(w.hwnd, &mut r);
        }
        let out = Some((w.url.clone()?, w.server.clone()?, POINT { x: (r.left + r.right) / 2, y: r.bottom - 60 }));
        w.going_online = true;
        w.status = "Creating internet link…".into();
        out
    })
    .flatten() else { return };
    let Some((port, tok)) = local
        .trim_start_matches("http://")
        .split_once('/')
        .and_then(|(host, tok)| Some((host.rsplit_once(':')?.1.to_string(), tok.to_string())))
    else {
        return;
    };
    crate::jobs::spawn("Internet link", anchor, move |job| {
        let fail = |msg: String| {
            post_ui(|| {
                with(|w| {
                    w.going_online = false;
                    w.status = "Internet link failed".into();
                });
            });
            Err(msg)
        };
        let exe = match crate::tools::cloudflared(job) {
            Ok(e) => e,
            Err(e) => return fail(e),
        };
        job.caption("Connecting");
        job.progress(-1.0);
        let mut child = match crate::tools::cmd(exe)
            .args(["tunnel", "--no-autoupdate", "--url", &format!("http://127.0.0.1:{port}")])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => return fail(format!("Couldn't start the tunnel: {e}")),
        };
        let stderr = child.stderr.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            use std::io::BufRead;
            // keep draining the log for the tunnel's whole life
            for line in std::io::BufReader::new(stderr).lines().map_while(Result::ok) {
                if let Some(i) = line.find("https://") {
                    let u: String = line[i..].chars().take_while(|c| !c.is_whitespace() && *c != '|').collect();
                    if u.contains(".trycloudflare.com") {
                        let _ = tx.send(u);
                    }
                }
            }
        });
        let Ok(public) = rx.recv_timeout(Duration::from_secs(40)) else {
            let _ = child.kill();
            return fail("Couldn't create an internet link. Check your connection and try again.".into());
        };
        if state.stop.load(Ordering::Relaxed) {
            let _ = child.kill();
            return Ok(String::new());
        }
        *state.tunnel.lock().unwrap() = Some(child);
        let url = format!("{}/{tok}", public.trim_end_matches('/'));
        post_ui(move || {
            with(|w| {
                w.qr = make_qr(&url);
                w.internet = Some(url);
                w.going_online = false;
                w.status = "Internet link ready".into();
            });
        });
        Ok("Internet link ready — works from any network".into())
    });
}

fn set_status(s: String) {
    with(|w| w.status = s);
}

fn close_now() {
    let old = WIN.with(|w| w.try_borrow_mut().ok().and_then(|mut b| b.take()));
    if let Some(w) = old {
        if let Some(s) = &w.server {
            s.shutdown();
        }
        unsafe {
            let _ = DestroyWindow(w.hwnd);
        }
    }
}

// ------------------------------------------------------------------ server

/// (display name, path on disk, size)
type FileEntry = (String, PathBuf, u64);

fn prepare_files(paths: &[PathBuf], state: &ServerState) -> Result<Vec<FileEntry>, String> {
    let mut out = Vec::new();
    for p in paths {
        if state.stop.load(Ordering::Relaxed) {
            return Err("Cancelled".into());
        }
        if p.is_dir() {
            let zip = util::unique(crate::tools::temp_dir().join(format!("{}.zip", util::name(p))));
            let mut c = crate::tools::cmd("tar.exe");
            c.arg("-a").arg("-cf").arg(&zip).arg("-C").arg(p.parent().unwrap_or(Path::new("."))).arg(util::name(p));
            crate::tools::run(c).map_err(|e| format!("Couldn't zip {}: {e}", util::name(p)))?;
            let size = std::fs::metadata(&zip).map(|m| m.len()).unwrap_or(0);
            out.push((format!("{}.zip", util::name(p)), zip, size));
        } else {
            let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
            out.push((util::name(p), p.clone(), size));
        }
    }
    Ok(out)
}

fn local_ip() -> Option<String> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("192.168.0.1:80").or_else(|_| sock.connect("8.8.8.8:80")).ok()?;
    let ip = sock.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then(|| ip.to_string())
}

/// 128-bit link secret from the OS cryptographic RNG.
fn token() -> String {
    use windows::Win32::Security::Cryptography::{BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG};
    let mut buf = [0u8; 16];
    let ok = unsafe { BCryptGenRandom(None, &mut buf, BCRYPT_USE_SYSTEM_PREFERRED_RNG) }.is_ok();
    assert!(ok, "system RNG unavailable");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// The device behind a request; through the internet tunnel everything arrives from
/// localhost, so use the address Cloudflare reports instead.
fn client_of(req: &tiny_http::Request) -> String {
    let remote = req.remote_addr().map(|a| a.ip());
    if remote.is_some_and(|ip| ip.is_loopback()) {
        if let Some(h) = req.headers().iter().find(|h| h.field.equiv("Cf-Connecting-Ip")) {
            return h.value.as_str().to_string();
        }
    }
    remote.map(|ip| ip.to_string()).unwrap_or_default()
}

fn start_server(state: Arc<ServerState>, mode: Mode, files: Vec<FileEntry>, folder: Option<PathBuf>) -> Result<String, String> {
    let ip = local_ip().ok_or("Not connected to a network. Connect to Wi-Fi and try again.")?;
    let server = (49152..49200)
        .find_map(|port| tiny_http::Server::http(("0.0.0.0", port)).ok())
        .or_else(|| tiny_http::Server::http("0.0.0.0:0").ok())
        .ok_or("Couldn't open a network port")?;
    let port = server.server_addr().to_ip().map(|a| a.port()).unwrap_or(0);
    let tok = token();
    let url = format!("http://{ip}:{port}/{tok}");
    let files = Arc::new(files);
    let started = std::time::Instant::now();
    std::thread::spawn(move || {
        while !state.stop.load(Ordering::Relaxed) && started.elapsed().as_secs_f64() < EXPIRY {
            let Ok(Some(req)) = server.recv_timeout(Duration::from_millis(400)) else { continue };
            let (files, folder, state, tok) = (files.clone(), folder.clone(), state.clone(), tok.clone());
            std::thread::spawn(move || handle(req, &tok, mode, &files, folder.as_deref(), &state));
        }
        if !state.stop.load(Ordering::Relaxed) {
            state.shutdown();
            post_ui(|| {
                close_now();
                crate::hud::toast("Share link expired", ToastKind::Info);
            });
        }
    });
    Ok(url)
}

fn header(k: &str, v: &str) -> tiny_http::Header {
    tiny_http::Header::from_bytes(k.as_bytes(), v.as_bytes()).unwrap()
}

fn pct_encode(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Invisible / direction-changing characters that can disguise a file name
/// (e.g. "photo{U+202E}gpj.exe" displaying as "photoexe.jpg").
fn is_format_char(c: char) -> bool {
    matches!(c as u32, 0x00AD | 0x061C | 0x180E | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x206F | 0xFEFF | 0xFFF9..=0xFFFB)
}

fn sanitize(name: &str) -> String {
    let n: String = name
        .chars()
        .filter(|c| !is_format_char(*c))
        .map(|c| if "\\/:*?\"<>|".contains(c) || c.is_control() { '_' } else { c })
        .collect();
    let n = n.trim().trim_start_matches('.').trim_end_matches(['.', ' ']).to_string();
    let n: String = n.chars().take(180).collect();
    // Windows reserved device names (CON, NUL, COM1…) can't be normal files
    let stem = n.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT")) && stem.len() == 4 && stem.as_bytes()[3].is_ascii_digit());
    if n.is_empty() {
        "file".into()
    } else if reserved {
        format!("_{n}")
    } else {
        n
    }
}

/// Mark a received file as coming from another device ("Mark of the Web"), so
/// Windows SmartScreen / Office warn before running or opening it.
fn mark_of_the_web(path: &Path) {
    let mut ads = path.as_os_str().to_owned();
    ads.push(":Zone.Identifier");
    let _ = std::fs::write(ads, "[ZoneTransfer]\r\nZoneId=3\r\n");
}

fn handle(req: tiny_http::Request, tok: &str, mode: Mode, files: &[FileEntry], folder: Option<&Path>, state: &ServerState) {
    let url = req.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((&url, ""));
    let base = format!("/{tok}");
    let client = client_of(&req);
    // anyone guessing links gets cut off after a few misses
    if state.bad.lock().unwrap().get(&client).copied().unwrap_or(0) >= MAX_BAD_REQUESTS {
        let _ = req.respond(tiny_http::Response::from_string("Too many requests").with_status_code(429));
        return;
    }
    if path != base && !path.starts_with(&format!("{base}/")) {
        *state.bad.lock().unwrap().entry(client).or_insert(0) += 1;
        let _ = req.respond(tiny_http::Response::from_string("Not found").with_status_code(404));
        return;
    }
    let html = |body: String| tiny_http::Response::from_string(body).with_header(header("Content-Type", "text/html; charset=utf-8")).with_header(header("Cache-Control", "no-store"));
    if path == base || path == format!("{base}/") {
        let page = match mode {
            Mode::Send => send_page(files),
            Mode::Receive => receive_page(folder.map(util::name).unwrap_or_default()),
        };
        let _ = req.respond(html(page));
        post_ui(|| set_status("Phone connected".into()));
        return;
    }
    if mode == Mode::Send {
        if let Some(idx) = path.strip_prefix(&format!("{base}/f/")).and_then(|i| i.parse::<usize>().ok()) {
            if let Some((name, p, _)) = files.get(idx) {
                if let Ok(f) = std::fs::File::open(p) {
                    let disp = format!("attachment; filename=\"{}\"; filename*=UTF-8''{}", name.replace(|c: char| !c.is_ascii() || c == '"', "_"), pct_encode(name));
                    let resp = tiny_http::Response::from_file(f).with_header(header("Content-Type", "application/octet-stream")).with_header(header("Content-Disposition", &disp));
                    if req.respond(resp).is_ok() {
                        let n = state.downloads.fetch_add(1, Ordering::Relaxed) + 1;
                        let nm = name.clone();
                        post_ui(move || set_status(format!("Sent {nm} · {}", util::plural(n as usize, "download"))));
                    }
                    return;
                }
            }
        }
    }
    if path == format!("{base}/up") {
        if let Some(folder) = folder {
            let name = query.split('&').find_map(|kv| kv.strip_prefix("name=")).map(pct_decode).unwrap_or_else(|| "file".into());
            let target = util::unique(folder.join(sanitize(&name)));
            let mut req = req;
            if req.body_length().is_some_and(|n| n as u64 > MAX_UPLOAD) {
                let _ = req.respond(tiny_http::Response::from_string("File too large").with_status_code(413));
                return;
            }
            let written = std::fs::File::create(&target).and_then(|mut f| std::io::copy(&mut req.as_reader().take(MAX_UPLOAD + 1), &mut f));
            let ok = matches!(written, Ok(n) if n <= MAX_UPLOAD);
            if ok {
                mark_of_the_web(&target);
                let n = state.downloads.fetch_add(1, Ordering::Relaxed) + 1;
                let nm = util::name(&target);
                post_ui(move || {
                    set_status(format!("Received {nm} · {}", util::plural(n as usize, "file")));
                    crate::hud::toast(&format!("Received {nm}"), ToastKind::Ok);
                });
                let _ = req.respond(tiny_http::Response::from_string("ok"));
            } else {
                let _ = std::fs::remove_file(&target);
                let _ = req.respond(tiny_http::Response::from_string("error").with_status_code(500));
            }
            return;
        }
    }
    let _ = req.respond(tiny_http::Response::from_string("Not found").with_status_code(404));
}

const PAGE_CSS: &str = r#"
*{box-sizing:border-box;margin:0;padding:0}
body{font-family:-apple-system,system-ui,Segoe UI,Roboto,sans-serif;background:#0f0f14;color:#f2f2f7;min-height:100vh;padding:28px 18px 40px}
.wrap{max-width:520px;margin:0 auto}
.logo{display:flex;align-items:center;gap:12px;margin-bottom:26px}
.dot{width:40px;height:40px;border-radius:50%;background:conic-gradient(#7c5cff,#3b82f6,#22c55e,#f59e0b,#ec4899,#7c5cff);display:grid;place-items:center}
.dot:after{content:"";width:14px;height:14px;border-radius:50%;background:#0f0f14}
h1{font-size:22px;font-weight:700;letter-spacing:-.3px}
.sub{color:#8e8e99;font-size:14px;margin-top:2px}
.card{background:#1a1a22;border:1px solid #ffffff12;border-radius:18px;padding:14px 14px 14px 16px;display:flex;align-items:center;gap:12px;margin-bottom:10px;animation:in .35s ease both}
@keyframes in{from{opacity:0;transform:translateY(8px)}}
.ic{width:42px;height:42px;border-radius:12px;background:#7c5cff22;color:#a594ff;display:grid;place-items:center;font-weight:700;font-size:11px;flex-shrink:0;text-transform:uppercase}
.meta{flex:1;min-width:0}
.name{font-weight:600;font-size:15px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.size{color:#8e8e99;font-size:13px;margin-top:2px}
.btn{background:#7c5cff;color:#fff;border:none;border-radius:12px;padding:10px 14px;font-weight:600;font-size:14px;text-decoration:none;display:inline-block}
.btn.big{display:block;text-align:center;width:100%;padding:16px;font-size:16px;border-radius:16px;margin-top:16px}
.btn.green{background:#22c55e}
.bar{height:4px;border-radius:2px;background:#ffffff14;margin-top:8px;overflow:hidden}
.bar i{display:block;height:100%;width:0;background:#7c5cff;transition:width .2s}
.ok{color:#22c55e;font-weight:700}
.foot{color:#5c5c66;font-size:12px;text-align:center;margin-top:28px}
"#;

const UPLOAD_JS: &str = r#"<script>
var q=[],busy=false;
document.getElementById('f').onchange=function(e){for(var i=0;i<e.target.files.length;i++)q.push(add(e.target.files[i]));next();e.target.value=''};
function add(file){var c=document.createElement('div');c.className='card';c.innerHTML='<div class="ic">'+(file.name.split('.').pop()||'').slice(0,4).replace(/[<>&]/g,'')+'</div><div class="meta"><div class="name"></div><div class="size">Waiting…</div><div class="bar"><i></i></div></div>';c.querySelector('.name').textContent=file.name;document.getElementById('list').prepend(c);return{file:file,el:c}}
function next(){if(busy||!q.length)return;busy=true;var t=q.shift();var x=new XMLHttpRequest();x.open('POST',location.pathname.replace(/\/$/,'')+'/up?name='+encodeURIComponent(t.file.name));
x.upload.onprogress=function(e){if(e.lengthComputable){t.el.querySelector('i').style.width=(e.loaded/e.total*100)+'%';t.el.querySelector('.size').textContent=Math.round(e.loaded/e.total*100)+'%'}};
x.onload=function(){t.el.querySelector('.size').innerHTML=x.status==200?'<span class="ok">✓ Sent</span>':'Failed';t.el.querySelector('i').style.width='100%';busy=false;next()};
x.onerror=function(){t.el.querySelector('.size').textContent='Failed';busy=false;next()};x.send(t.file)}
</script>"#;

fn send_page(files: &[FileEntry]) -> String {
    let mut cards = String::new();
    for (i, (name, _, size)) in files.iter().enumerate() {
        let ext = Path::new(name).extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
        cards.push_str(&format!(
            r#"<div class="card" style="animation-delay:{d}ms"><div class="ic">{ext}</div><div class="meta"><div class="name">{n}</div><div class="size">{s}</div></div><a class="btn dl" href="f/{i}" download="{n}">Get</a></div>"#,
            d = i * 40,
            ext = html_escape(&ext.chars().take(4).collect::<String>()),
            n = html_escape(name),
            s = util::human_size(*size),
        ));
    }
    let all = if files.len() > 1 {
        r##"<a class="btn big green" href="#" onclick="var a=document.querySelectorAll('.dl');a.forEach(function(x,i){setTimeout(function(){x.click()},i*700)});return false">Download all</a>"##
    } else {
        ""
    };
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Orbit Share</title><style>{PAGE_CSS}</style></head><body><div class="wrap"><div class="logo"><div class="dot"></div><div><h1>Orbit Share</h1><div class="sub">From your PC · {count}</div></div></div>{cards}{all}<div class="sub" style="margin-top:34px">Send files back to this PC (saved in Downloads)</div><label class="btn big" style="background:#2a2a36">Send to PC<input id="f" type="file" multiple style="display:none"></label><div id="list" style="margin-top:14px"></div><div class="foot">Works on any phone or PC that has the link.</div></div>{UPLOAD_JS}</body></html>"#,
        count = util::plural(files.len(), "file"),
    )
}

fn receive_page(folder: String) -> String {
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Send to PC</title><style>{PAGE_CSS}</style></head><body><div class="wrap">
<div class="logo"><div class="dot"></div><div><h1>Send to PC</h1><div class="sub">Into “{f}”</div></div></div>
<label class="btn big">Choose files<input id="f" type="file" multiple style="display:none"></label>
<div id="list" style="margin-top:18px"></div>
<div class="foot">Files go straight to your PC.</div></div>{UPLOAD_JS}</body></html>"#,
        f = html_escape(&folder)
    )
}

// ------------------------------------------------------------------ window

impl Win {
    fn paint(&mut self, pos: Option<(i32, i32)>) {
        let t = now();
        let mut alpha = ease_out_cubic(((t - self.t0) / 0.22) as f32);
        if let Some(tc) = self.closing {
            alpha *= 1.0 - ((t - tc) / 0.15) as f32;
        }
        let remaining = (EXPIRY - self.ready_at.map(|r| t - r).unwrap_or(0.0)).max(0.0);
        let (mode, hover, copied) = (self.mode, self.hover, t - self.copied_at < 1.6);
        let ready_k = self.ready_at.map(|r| ease_out_cubic(((t - r) / 0.4) as f32)).unwrap_or(0.0);
        let online = self.internet.is_some();
        let going = self.going_online;
        let url = self.internet.clone().or(self.url.clone());
        let info = self.info.clone();
        let status = self.status.clone();
        let error = self.error.clone();
        let qr = self.qr.clone();
        let s = self.scale;
        self.surf.draw(s, |c| {
            let (x, y, w, h) = (18.0, 18.0, W - 36.0, H - 36.0);
            c.shadow_rrect(x, y, w, h, 22.0, 16.0, 0.5);
            c.fill_rrect(x, y, w, h, 22.0, rgba(20, 20, 26, 0.98));
            c.stroke_rrect(x, y, w, h, 22.0, 1.0, rgba(255, 255, 255, 0.08));
            // header
            let col = if mode == Mode::Send { GREEN } else { ACCENT };
            c.fill_circle(x + 34.0, y + 36.0, 17.0, hex(col, 0.18));
            c.icon(if mode == Mode::Send { '\u{E72D}' } else { '\u{E896}' }, x + 34.0, y + 36.0, 15.0, hex(col, 1.0));
            let title = if mode == Mode::Send { "Share" } else { "Receive files" };
            c.text(title, x + 60.0, y + 18.0, w - 110.0, 20.0, 15.5, 650, rgba(255, 255, 255, 0.96), Align::Left);
            c.text(&info, x + 60.0, y + 38.0, w - 110.0, 18.0, 11.5, 500, rgba(255, 255, 255, 0.5), Align::Left);
            let (cx, cy) = (x + w - 30.0, y + 36.0);
            c.fill_circle(cx, cy, 14.0, if hover == Btn::Close { hex(RED, 0.85) } else { rgba(255, 255, 255, 0.06) });
            c.icon('\u{E711}', cx, cy, 10.0, rgba(255, 255, 255, 0.85));

            // QR area
            let qs = 236.0;
            let (qx, qy) = (x + (w - qs) / 2.0, y + 78.0);
            c.fill_rrect(qx, qy, qs, qs, 18.0, mix(rgba(255, 255, 255, 0.05), rgba(255, 255, 255, 1.0), ready_k));
            if let Some(err) = &error {
                c.icon('\u{E783}', qx + qs / 2.0, qy + 80.0, 30.0, hex(RED, 1.0));
                c.text_wrap(err, qx + 16.0, qy + 110.0, qs - 32.0, 90.0, 12.5, 500, rgba(255, 255, 255, 0.85), Align::Center);
            } else if let Some((mods, n)) = &qr {
                // pixel-snapped modules for a crisp code
                let inner_px = ((qs - 26.0) * s).floor();
                let m = (inner_px / *n as f32).floor().max(1.0);
                let size_px = m * *n as f32;
                let ox = ((qx * s) + (qs * s - size_px) / 2.0).round();
                let oy = ((qy * s) + (qs * s - size_px) / 2.0).round();
                let dark = rgba(14, 14, 20, ready_k);
                for yy in 0..*n {
                    for xx in 0..*n {
                        if mods[yy * n + xx] {
                            c.fill_rect_px(ox + xx as f32 * m, oy + yy as f32 * m, m, m, dark);
                        }
                    }
                }
            } else {
                let rot = (t * 300.0 % 360.0) as f32;
                c.arc(qx + qs / 2.0, qy + qs / 2.0, 22.0, rot, rot + 100.0, 3.5, hex(ACCENT, 1.0));
                c.text("Preparing…", qx, qy + qs / 2.0 + 30.0, qs, 20.0, 12.0, 500, rgba(255, 255, 255, 0.6), Align::Center);
            }

            // instructions
            let ty = qy + qs + 18.0;
            c.text("Scan with a phone camera", x, ty, w, 20.0, 14.0, 650, rgba(255, 255, 255, 0.95), Align::Center);
            let sub = if online { "or open the link on any device, anywhere" } else { "or open the link on any PC / phone on this Wi-Fi" };
            c.text(sub, x, ty + 22.0, w, 18.0, 11.5, 500, rgba(255, 255, 255, 0.5), Align::Center);

            // link pill
            if let Some(u) = &url {
                let (lx, ly, lw, lh) = (x + 22.0, ty + 54.0, w - 44.0, 34.0);
                let hov = hover == Btn::Link;
                c.fill_rrect(lx, ly, lw, lh, 12.0, if hov { rgba(255, 255, 255, 0.10) } else { rgba(255, 255, 255, 0.05) });
                c.icon(if copied { '\u{E73E}' } else { '\u{E8C8}' }, lx + 18.0, ly + lh / 2.0, 12.0, if copied { hex(GREEN, 1.0) } else { rgba(255, 255, 255, 0.6) });
                let label = if copied { "Link copied".to_string() } else { u.trim_start_matches("http://").trim_start_matches("https://").to_string() };
                c.text(&label, lx + 34.0, ly, lw - 44.0, lh, 12.0, 500, rgba(255, 255, 255, 0.8), Align::Left);
            }

            // internet link button
            if url.is_some() {
                let (bx, by, bw, bh) = (x + 22.0, ty + 96.0, w - 44.0, 36.0);
                let hov = hover == Btn::Internet && !online && !going;
                let fill = if online { hex(GREEN, 0.16) } else if hov { hex(ACCENT, 0.9) } else { hex(ACCENT, 0.22) };
                c.fill_rrect(bx, by, bw, bh, 12.0, fill);
                let (glyph, label) = if online {
                    ('\u{E73E}', "Internet link on · works from any network")
                } else if going {
                    ('\u{E895}', "Creating internet link…")
                } else {
                    ('\u{E774}', "Make internet link (other networks)")
                };
                let tc = if online { hex(GREEN, 1.0) } else { rgba(255, 255, 255, 0.95) };
                c.icon(glyph, bx + 20.0, by + bh / 2.0, 13.0, tc);
                c.text(label, bx + 36.0, by, bw - 44.0, bh, 12.0, 600, tc, Align::Left);
            }

            // status
            let sy = y + h - 34.0;
            let pulse = 0.5 + 0.5 * ((t * 3.0).sin() as f32);
            let scol = if error.is_some() { RED } else if url.is_some() { GREEN } else { ACCENT };
            c.fill_circle(x + 30.0, sy + 9.0, 4.0 + 3.0 * pulse, hex(scol, 0.18));
            c.fill_circle(x + 30.0, sy + 9.0, 3.5, hex(scol, 1.0));
            c.text(&status, x + 42.0, sy, w - 130.0, 18.0, 11.5, 500, rgba(255, 255, 255, 0.75), Align::Left);
            if url.is_some() {
                let m = (remaining / 60.0) as i32;
                let sec = (remaining % 60.0) as i32;
                c.text(&format!("{m}:{sec:02}"), x + w - 90.0, sy, 70.0, 18.0, 11.5, 600, rgba(255, 255, 255, 0.4), Align::Right);
            }
        });
        self.surf.present(self.hwnd, pos, alpha);
    }

    fn hit(&self, px: i32, py: i32) -> Btn {
        let (x, y) = (px as f32 / self.scale, py as f32 / self.scale);
        let (cx, cy) = (W - 18.0 - 30.0, 18.0 + 36.0);
        if (x - cx).powi(2) + (y - cy).powi(2) < 16.0f32.powi(2) {
            return Btn::Close;
        }
        let ly = 18.0 + 78.0 + 236.0 + 18.0 + 54.0;
        if self.url.is_some() && x > 40.0 && x < W - 40.0 && y > ly && y < ly + 34.0 {
            return Btn::Link;
        }
        let by = ly + 42.0;
        if self.url.is_some() && x > 40.0 && x < W - 40.0 && y > by && y < by + 36.0 {
            return Btn::Internet;
        }
        Btn::None
    }

    fn close(&mut self) {
        if self.closing.is_none() {
            self.closing = Some(now());
            if let Some(s) = &self.server {
                s.shutdown();
            }
        }
    }
}

unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_NCHITTEST => {
            let mut p = POINT { x: ui::x_of(lp), y: ui::y_of(lp) };
            let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut p);
            let caption = with(|w| w.hit(p.x, p.y) == Btn::None && (p.y as f32 / w.scale) < 76.0).unwrap_or(false);
            return LRESULT(if caption { HTCAPTION as isize } else { HTCLIENT as isize });
        }
        WM_MOUSEMOVE => {
            ui::track_leave(hwnd);
            with(|w| w.hover = w.hit(ui::x_of(lp), ui::y_of(lp)));
            return LRESULT(0);
        }
        windows::Win32::UI::Controls::WM_MOUSELEAVE => {
            with(|w| w.hover = Btn::None);
            return LRESULT(0);
        }
        WM_LBUTTONUP => {
            let b = with(|w| w.hit(ui::x_of(lp), ui::y_of(lp))).unwrap_or(Btn::None);
            match b {
                Btn::Close => {
                    with(|w| w.close());
                }
                Btn::Link => {
                    if let Some(u) = with(|w| {
                        w.copied_at = now();
                        w.internet.clone().or(w.url.clone())
                    })
                    .flatten()
                    {
                        util::set_clipboard_text(&u);
                    }
                }
                Btn::Internet => go_online(),
                Btn::None => {}
            }
            return LRESULT(0);
        }
        WM_KEYDOWN => {
            if wp.0 == VK_ESCAPE.0 as usize {
                with(|w| w.close());
            }
            return LRESULT(0);
        }
        WM_TIMER => {
            let done = with(|w| {
                if let Some(tc) = w.closing {
                    if now() - tc > 0.15 {
                        return true;
                    }
                }
                w.paint(None);
                false
            })
            .unwrap_or(false);
            if done {
                close_now();
            }
            return LRESULT(0);
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

/// README screenshot of the share window with a sample link.
pub fn snapshot(scale: f32, out: &std::path::Path) -> std::result::Result<(), String> {
    let surf = Surface::new((W * scale) as i32, (H * scale) as i32).map_err(|e| e.message())?;
    // obviously fake data: no real address, and the QR only holds a label
    let url = "http://192.168.x.x:49152/demo-link".to_string();
    let mut w = Win {
        hwnd: HWND::default(),
        surf,
        scale,
        mode: Mode::Send,
        t0: now() - 5.0,
        ready_at: Some(now() - 18.0),
        qr: make_qr("Orbit demo"),
        url: Some(url),
        internet: None,
        going_online: false,
        info: "3 files · 48.2 MB".into(),
        status: "Sent holiday.mp4 · 1 download".into(),
        error: None,
        hover: Btn::None,
        copied_at: -10.0,
        closing: None,
        server: None,
    };
    w.paint(None);
    w.surf.save_png(out)
}
