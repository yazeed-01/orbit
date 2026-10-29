#![windows_subsystem = "windows"]

//! Orbit — a radial right-click menu for Windows Explorer.

mod actions;
mod config;
mod exif;
mod explorer;
mod fileops;
mod gfx;
mod hud;
mod input;
mod jobs;
mod menu;
mod pdf;
mod radial;
mod share;
mod tools;
mod ui;
mod util;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use config::{Config, HOTKEYS};
use hud::ToastKind;

pub const WM_APP_RCLICK: u32 = WM_APP + 1;
pub const WM_APP_EVENT: u32 = WM_APP + 2;
pub const WM_APP_TRAY: u32 = WM_APP + 3;
pub const WM_APP_ACTION: u32 = WM_APP + 4;
pub const WM_APP_SECOND: u32 = WM_APP + 5;

const MAIN_CLASS: PCWSTR = w!("OrbitMain");
const HOTKEY_ID: i32 = 1;
const TIMER_HOOK_WATCHDOG: usize = 90;

const CMD_TOGGLE: usize = 1;
const CMD_AUTOSTART: usize = 2;
const CMD_TOOLS: usize = 3;
const CMD_HELP: usize = 4;
const CMD_EXIT: usize = 5;
const CMD_RECEIVE: usize = 6;
const CMD_HOTKEY_BASE: usize = 100;

static MAIN: AtomicIsize = AtomicIsize::new(0);
static HOOK_THREAD: AtomicU32 = AtomicU32::new(0);
static ENABLED: AtomicBool = AtomicBool::new(false);
static SWALLOW_UP: AtomicBool = AtomicBool::new(false);
/// Set by ORBIT_TEST_INPUT so automated tests can drive the hook with synthetic clicks.
static ACCEPT_INJECTED: AtomicBool = AtomicBool::new(false);
/// Marks clicks Orbit replays itself so the hook lets them through.
const REPLAY_TAG: usize = 0x0B17_0B17;

thread_local! {
    static CONFIG: RefCell<Config> = RefCell::new(Config::load());
    static TASKBAR_CREATED: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

pub fn main_hwnd() -> HWND {
    HWND(MAIN.load(Ordering::Relaxed) as _)
}

fn hotkey_label() -> &'static str {
    CONFIG.with(|c| HOTKEYS[c.borrow().hotkey].label)
}

fn main() {
    unsafe {
        if probe_from_args() {
            return;
        }
        if std::env::args().any(|a| a == "--screenshots") {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            if gfx::init().is_ok() {
                screenshots_from_args();
            }
            return;
        }
        let _mutex = CreateMutexW(None, true, w!("Local\\Orbit.RadialMenu.SingleInstance"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            if let Ok(h) = FindWindowW(MAIN_CLASS, None) {
                let _ = PostMessageW(h, WM_APP_SECOND, WPARAM(0), LPARAM(0));
            }
            return;
        }
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        ui::now();
        ACCEPT_INJECTED.store(std::env::var_os("ORBIT_TEST_INPUT").is_some(), Ordering::Relaxed);
        if gfx::init().is_err() {
            MessageBoxW(None, w!("Orbit couldn't start graphics (Direct2D)."), w!("Orbit"), MB_ICONERROR);
            return;
        }
        ui::register_class(MAIN_CLASS, Some(main_proc));
        hud::register();
        radial::register();
        share::register();
        input::register();

        let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), MAIN_CLASS, w!("Orbit"), WS_OVERLAPPED, 0, 0, 0, 0, None, None, ui::hinstance(), None).unwrap();
        MAIN.store(hwnd.0 as isize, Ordering::Relaxed);
        TASKBAR_CREATED.with(|t| t.set(RegisterWindowMessageW(w!("TaskbarCreated"))));

        let (first_run, enabled) = CONFIG.with(|c| (c.borrow().first_run, c.borrow().enabled));
        tray_add();
        let hk_ok = register_hotkey();
        if first_run {
            config::set_autostart(true);
            CONFIG.with(|c| c.borrow().save());
        } else if config::autostart() {
            config::set_autostart(true); // keep the entry pointing at this exe if it was moved
        }
        set_enabled(enabled, false);
        let background = std::env::args().any(|a| a == "--background");
        if !hk_ok {
            hud::toast(&format!("{} is used by another app — pick another shortcut from the tray icon", hotkey_label()), ToastKind::Error);
        } else if !background {
            let msg = if enabled {
                format!("Orbit is on — right-click files in Explorer · {} to turn off", hotkey_label())
            } else {
                format!("Orbit is running — press {} to turn on the radial menu", hotkey_label())
            };
            hud::toast(&msg, ToastKind::Info);
        }

        demo_from_args();

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        set_hook(false);
        tray_remove();
    }
}

fn register_hotkey() -> bool {
    unsafe {
        let _ = UnregisterHotKey(main_hwnd(), HOTKEY_ID);
        let i = CONFIG.with(|c| c.borrow().hotkey);
        let hk = &HOTKEYS[i];
        RegisterHotKey(main_hwnd(), HOTKEY_ID, hk.mods | MOD_NOREPEAT, hk.vk).is_ok()
    }
}

/// The mouse hook lives on its own thread that does nothing but pump messages, so it
/// always answers instantly (Windows silently drops low-level hooks that respond slowly,
/// and the UI thread can be busy talking to Explorer).
fn set_hook(on: bool) {
    unsafe {
        let tid = HOOK_THREAD.swap(0, Ordering::SeqCst);
        if tid != 0 {
            let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        if !on {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let hook = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), ui::hinstance(), 0);
            ui::log(&format!("hook thread started, ok={}", hook.is_ok()));
            let _ = tx.send(windows::Win32::System::Threading::GetCurrentThreadId());
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                DispatchMessageW(&msg);
            }
            if let Ok(h) = hook {
                let _ = UnhookWindowsHookEx(h);
            }
        });
        if let Ok(id) = rx.recv() {
            HOOK_THREAD.store(id, Ordering::SeqCst);
        }
    }
}

fn set_enabled(on: bool, announce: bool) {
    ENABLED.store(on, Ordering::Relaxed);
    set_hook(on);
    unsafe {
        if on {
            SetTimer(main_hwnd(), TIMER_HOOK_WATCHDOG, 30_000, None);
        } else {
            let _ = KillTimer(main_hwnd(), TIMER_HOOK_WATCHDOG);
        }
    }
    CONFIG.with(|c| {
        let mut c = c.borrow_mut();
        c.enabled = on;
        c.save();
    });
    tray_update();
    if announce {
        if on {
            hud::toast("Radial menu ON — right-click in Explorer  ·  Shift+right-click = classic menu", ToastKind::Ok);
        } else {
            hud::toast("Radial menu OFF — normal right-click restored", ToastKind::Info);
        }
    }
}

unsafe extern "system" fn mouse_hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && ENABLED.load(Ordering::Relaxed) {
        let ms = &*(lp.0 as *const MSLLHOOKSTRUCT);
        let injected = ms.flags & LLMHF_INJECTED != 0;
        if ms.dwExtraInfo != REPLAY_TAG && (!injected || ACCEPT_INJECTED.load(Ordering::Relaxed)) {
            match wp.0 as u32 {
                WM_RBUTTONDOWN => {
                    ui::log(&format!("hook: rbutton down at {},{} injected={injected}", ms.pt.x, ms.pt.y));
                    let shift = GetAsyncKeyState(VK_SHIFT.0 as i32) < 0;
                    if !shift {
                        if let Some(dv) = explorer::defview_at(ms.pt) {
                            SWALLOW_UP.store(true, Ordering::Relaxed);
                            let packed = ((ms.pt.y as u16 as isize) << 16) | (ms.pt.x as u16 as isize);
                            let _ = PostMessageW(main_hwnd(), WM_APP_RCLICK, WPARAM(dv.0 as usize), LPARAM(packed));
                            return LRESULT(1);
                        }
                    }
                }
                WM_RBUTTONUP => {
                    if SWALLOW_UP.swap(false, Ordering::Relaxed) {
                        return LRESULT(1);
                    }
                }
                _ => {}
            }
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

/// Re-send the right click so Explorer shows its own menu (used for virtual folders).
fn replay_right_click() {
    unsafe {
        let mk = |flags| INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 { mi: MOUSEINPUT { dx: 0, dy: 0, mouseData: 0, dwFlags: flags, time: 0, dwExtraInfo: REPLAY_TAG } },
        };
        let inputs = [mk(MOUSEEVENTF_RIGHTDOWN), mk(MOUSEEVENTF_RIGHTUP)];
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

/// `orbit.exe --screenshots DIR` renders README images of the UI (no windows are shown).
fn screenshots_from_args() -> bool {
    let args: Vec<String> = std::env::args().collect();
    let Some(i) = args.iter().position(|a| a == "--screenshots") else { return false };
    let dir = std::path::PathBuf::from(args.get(i + 1).cloned().unwrap_or_else(|| "screenshots".into()));
    let _ = std::fs::create_dir_all(&dir);
    let s = 1.5;
    let ctx = |items: &[&str], folder: &str| {
        Rc::new(explorer::Ctx {
            folder: folder.into(),
            items: items.iter().map(std::path::PathBuf::from).collect(),
            background: items.is_empty(),
            pt: POINT::default(),
            view: None,
            defview: HWND::default(),
        })
    };
    let menu = |name: &str, c: Rc<explorer::Ctx>, open: &[usize], page: usize, hover: Option<usize>| {
        let (title, items) = menu::build(&c);
        if let Err(e) = radial::snapshot(c, title, items, open, page, hover, s, &dir.join(name)) {
            eprintln!("{name}: {e}");
        }
    };
    menu("menu-image.png", ctx(&["C:\\Photos\\sunset.jpg"], "C:\\Photos"), &[], 0, Some(0));
    menu("menu-image-convert.png", ctx(&["C:\\Photos\\sunset.jpg"], "C:\\Photos"), &[0], 0, Some(1));
    menu("menu-image-resize.png", ctx(&["C:\\Photos\\sunset.jpg"], "C:\\Photos"), &[1], 0, Some(3));
    menu("menu-video.png", ctx(&["C:\\Videos\\trip.mp4"], "C:\\Videos"), &[], 0, Some(3));
    menu("menu-video-edit.png", ctx(&["C:\\Videos\\trip.mp4", "C:\\Videos\\trip-2.mp4"], "C:\\Videos"), &[3], 0, Some(1));
    menu("menu-pdf.png", ctx(&["C:\\Docs\\report.pdf"], "C:\\Docs"), &[], 0, Some(1));
    menu("menu-folder.png", ctx(&["C:\\Projects\\website"], "C:\\Projects"), &[], 0, Some(2));
    menu("menu-empty-space.png", ctx(&[], "C:\\Projects\\website"), &[], 0, Some(1));
    menu("menu-new-file.png", ctx(&[], "C:\\Projects\\website"), &[1], 0, Some(2));
    let _ = hud::snapshot_ring(0.62, None, "To MP4 · 2/3", s, &dir.join("progress.png"));
    let _ = hud::snapshot_ring(1.0, Some(true), "Done", s, &dir.join("done.png"));
    let _ = hud::snapshot_toast("Saved sunset.webp", ToastKind::Ok, s, &dir.join("toast.png"));
    let _ = hud::snapshot_toast(
        "sunset.jpg · 4.1 MB · 4032×3024 (12.2 MP)\nCanon EOS R6 · 2025-07-14 19:42:10\nf/2.8 · 1/250s · ISO 100 · 35 mm\nGPS 48.85837, 2.29448\nCopied, including a map link",
        ToastKind::Info,
        s,
        &dir.join("image-info.png"),
    );
    let _ = share::snapshot(s, &dir.join("share.png"));
    true
}

/// `orbit.exe --probe X Y` writes what Orbit detects at a screen point to %TEMP%\orbit_probe.txt.
fn probe_from_args() -> bool {
    let args: Vec<String> = std::env::args().collect();
    let Some(i) = args.iter().position(|a| a == "--probe") else { return false };
    let num = |k: usize| args.get(i + k).and_then(|v| v.parse::<i32>().ok()).unwrap_or(0);
    let pt = POINT { x: num(1), y: num(2) };
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    // `--probe-hwnd TOP ON_ITEM` skips point hit-testing (useful when something overlays the screen)
    let direct = args.iter().any(|a| a == "--hwnd");
    let dv = if direct { explorer::find_defview(HWND(num(1) as isize as _)) } else { explorer::defview_at(pt) };
    let report = match dv {
        None => "no explorer view".to_string(),
        Some(dv) => match if direct { explorer::context_for(pt, dv, num(2) != 0) } else { explorer::context_at(pt, dv) } {
            None => "view found, no file-system context".to_string(),
            Some(c) => {
                let (title, items) = menu::build(&c);
                format!(
                    "folder={}\nbackground={}\nitems={:?}\ntitle={title}\nmenu={:?}",
                    c.folder.display(),
                    c.background,
                    c.items,
                    items.iter().map(|i| i.label.clone()).collect::<Vec<_>>()
                )
            }
        },
    };
    let _ = std::fs::write(std::env::temp_dir().join("orbit_probe.txt"), report);
    true
}

/// `orbit.exe --demo <paths…>` opens the menu for those paths at the cursor
/// (a folder alone with `--bg` opens its empty-space menu). Used for previews/testing.
fn demo_from_args() {
    let args: Vec<String> = std::env::args().collect();
    let Some(i) = args.iter().position(|a| a == "--demo") else { return };
    let bg = args.iter().any(|a| a == "--bg");
    let paths: Vec<std::path::PathBuf> = args[i + 1..].iter().filter(|a| !a.starts_with("--")).map(std::path::PathBuf::from).collect();
    let Some(first) = paths.first() else { return };
    let folder = if bg { first.clone() } else { first.parent().map(|p| p.to_path_buf()).unwrap_or_default() };
    let ctx = explorer::Ctx { folder, items: if bg { vec![] } else { paths }, background: bg, pt: ui::cursor(), view: None, defview: HWND::default() };
    let (title, items) = menu::build(&ctx);
    radial::open(Rc::new(ctx), title, items);
}

fn on_right_click(defview: HWND, pt: POINT) {
    let ctx = explorer::context_at(pt, defview);
    ui::log(&format!("right-click at {},{} -> {}", pt.x, pt.y, ctx.as_ref().map(|c| format!("{} items, bg={}", c.items.len(), c.background)).unwrap_or("no context".into())));
    match ctx {
        Some(ctx) => {
            let (title, items) = menu::build(&ctx);
            radial::open(Rc::new(ctx), title, items);
        }
        None => replay_right_click(),
    }
}

// ------------------------------------------------------------------ tray

fn tray_icon(on: bool) -> HICON {
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16) * 2;
    let colors: [u32; 6] = [0x7C5CFF, 0x3B82F6, 0x22C55E, 0xF59E0B, 0xEC4899, 0xB46EFF];
    gfx::make_icon(size, |c| {
        let s = size as f32;
        let cx = s / 2.0;
        let ro = s * 0.48;
        let ri = s * 0.22;
        for (i, col) in colors.iter().enumerate() {
            let a0 = -90.0 - 30.0 + i as f32 * 60.0;
            let color = if on { gfx::hex(*col, 1.0) } else { gfx::rgba(150, 150, 160, 1.0) };
            c.wedge(cx, cx, ri, ro, a0, a0 + 60.0, s * 0.07, color);
        }
        c.fill_circle(cx, cx, s * 0.12, if on { gfx::rgba(255, 255, 255, 1.0) } else { gfx::rgba(150, 150, 160, 1.0) });
    })
    .unwrap_or_default()
}

fn tray_data() -> NOTIFYICONDATAW {
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: main_hwnd(),
        uID: 1,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_APP_TRAY,
        hIcon: tray_icon(ENABLED.load(Ordering::Relaxed)),
        ..Default::default()
    };
    let tip = format!("Orbit — {} ({})", if ENABLED.load(Ordering::Relaxed) { "ON" } else { "OFF" }, hotkey_label());
    for (i, ch) in tip.encode_utf16().take(127).enumerate() {
        nid.szTip[i] = ch;
    }
    nid
}

fn tray_add() {
    unsafe {
        let nid = tray_data();
        let _ = Shell_NotifyIconW(NIM_ADD, &nid);
        let _ = DestroyIcon(nid.hIcon);
    }
}

fn tray_update() {
    unsafe {
        let nid = tray_data();
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        let _ = DestroyIcon(nid.hIcon);
    }
}

fn tray_remove() {
    unsafe {
        let nid = NOTIFYICONDATAW { cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32, hWnd: main_hwnd(), uID: 1, ..Default::default() };
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

fn tray_menu() {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let on = ENABLED.load(Ordering::Relaxed);
        let item = |m: HMENU, flags: MENU_ITEM_FLAGS, id: usize, text: &str| {
            let w = ui::wide(text);
            let _ = AppendMenuW(m, flags, id, PCWSTR(w.as_ptr()));
        };
        item(menu, MF_STRING | MF_DISABLED, 0, "Orbit — radial menu");
        item(menu, MF_SEPARATOR, 0, "");
        item(menu, MF_STRING | if on { MF_CHECKED } else { MF_UNCHECKED }, CMD_TOGGLE, &format!("Enabled\t{}", hotkey_label()));
        if let Ok(sub) = CreatePopupMenu() {
            let cur = CONFIG.with(|c| c.borrow().hotkey);
            for (i, hk) in HOTKEYS.iter().enumerate() {
                item(sub, MF_STRING | if i == cur { MF_CHECKED } else { MF_UNCHECKED }, CMD_HOTKEY_BASE + i, hk.label);
            }
            item(menu, MF_POPUP, sub.0 as usize, "Shortcut");
        }
        item(menu, MF_STRING | if config::autostart() { MF_CHECKED } else { MF_UNCHECKED }, CMD_AUTOSTART, "Start with Windows");
        item(menu, MF_STRING, CMD_RECEIVE, "Receive files from phone…");
        item(menu, MF_STRING, CMD_TOOLS, "Open tools folder");
        item(menu, MF_STRING, CMD_HELP, "How to use");
        item(menu, MF_SEPARATOR, 0, "");
        item(menu, MF_STRING, CMD_EXIT, "Exit");
        let p = ui::cursor();
        let _ = SetForegroundWindow(main_hwnd());
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, p.x, p.y, 0, main_hwnd(), None).0 as usize;
        let _ = DestroyMenu(menu);
        match cmd {
            CMD_TOGGLE => set_enabled(!on, true),
            CMD_AUTOSTART => {
                let now_on = !config::autostart();
                config::set_autostart(now_on);
                hud::toast(if now_on { "Orbit will start with Windows" } else { "Orbit won't start with Windows" }, ToastKind::Info);
            }
            CMD_TOOLS => {
                let _ = std::process::Command::new("explorer.exe").arg(tools::tools_dir()).spawn();
            }
            CMD_HELP => hud::toast(
                &format!(
                    "Press {} to turn Orbit on, then right-click files, folders or empty space in Explorer. Scroll to see more options, right-click or Esc to go back. Shift+right-click opens the classic menu.",
                    hotkey_label()
                ),
                ToastKind::Info,
            ),
            CMD_RECEIVE => share::receive(config::downloads_dir()),
            CMD_EXIT => PostQuitMessage(0),
            c if (CMD_HOTKEY_BASE..CMD_HOTKEY_BASE + HOTKEYS.len()).contains(&c) => {
                let old = CONFIG.with(|cf| cf.borrow().hotkey);
                CONFIG.with(|cf| cf.borrow_mut().hotkey = c - CMD_HOTKEY_BASE);
                if register_hotkey() {
                    CONFIG.with(|cf| cf.borrow().save());
                    tray_update();
                    hud::toast(&format!("Shortcut set to {}", hotkey_label()), ToastKind::Ok);
                } else {
                    let taken = hotkey_label();
                    CONFIG.with(|cf| cf.borrow_mut().hotkey = old);
                    register_hotkey();
                    hud::toast(&format!("{taken} is used by another app"), ToastKind::Error);
                }
            }
            _ => {}
        }
    }
}

unsafe extern "system" fn main_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_HOTKEY => {
            if wp.0 as i32 == HOTKEY_ID {
                set_enabled(!ENABLED.load(Ordering::Relaxed), true);
            }
            return LRESULT(0);
        }
        WM_APP_RCLICK => {
            let pt = POINT { x: ui::x_of(lp), y: ui::y_of(lp) };
            on_right_click(HWND(wp.0 as _), pt);
            return LRESULT(0);
        }
        WM_APP_EVENT => {
            jobs::drain();
            return LRESULT(0);
        }
        WM_APP_ACTION => {
            if let Some((action, ctx)) = radial::take_pending() {
                actions::run(action, ctx);
            }
            return LRESULT(0);
        }
        WM_APP_SECOND => {
            let on = ENABLED.load(Ordering::Relaxed);
            hud::toast(&format!("Orbit is already running ({}) · {} to toggle", if on { "ON" } else { "OFF" }, hotkey_label()), ToastKind::Info);
            return LRESULT(0);
        }
        WM_APP_TRAY => {
            match lp.0 as u32 {
                WM_LBUTTONUP => set_enabled(!ENABLED.load(Ordering::Relaxed), true),
                WM_RBUTTONUP | WM_CONTEXTMENU => tray_menu(),
                _ => {}
            }
            return LRESULT(0);
        }
        WM_TIMER => {
            match wp.0 {
                hud::TIMER_HUD => hud::tick(),
                actions::TIMER_RENAME => actions::do_rename(),
                // re-install the hook periodically in case Windows dropped it
                TIMER_HOOK_WATCHDOG if ENABLED.load(Ordering::Relaxed) && !SWALLOW_UP.load(Ordering::Relaxed) => set_hook(true),
                _ => {}
            }
            return LRESULT(0);
        }
        _ => {
            if msg != 0 && msg == TASKBAR_CREATED.with(|t| t.get()) {
                tray_add();
                return LRESULT(0);
            }
        }
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}
