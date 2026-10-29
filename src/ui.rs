//! Shared window helpers: classes, layered popups, timing, easing, monitors.

use std::sync::OnceLock;
use std::time::Instant;

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub fn hinstance() -> HINSTANCE {
    unsafe { GetModuleHandleW(None).map(|m| m.into()).unwrap_or_default() }
}

pub fn register_class(name: PCWSTR, proc: WNDPROC) {
    unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: proc,
            hInstance: hinstance(),
            lpszClassName: name,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&wc);
    }
}

/// Create a borderless, topmost, per-pixel-alpha popup that never shows in the taskbar.
pub fn create_layered(class: PCWSTR, activatable: bool, click_through: bool) -> HWND {
    unsafe {
        let mut ex = WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW;
        if !activatable {
            ex |= WS_EX_NOACTIVATE;
        }
        if click_through {
            ex |= WS_EX_TRANSPARENT;
        }
        CreateWindowExW(ex, class, w!(""), WS_POPUP, 0, 0, 10, 10, None, None, hinstance(), None).unwrap_or_default()
    }
}

static START: OnceLock<Instant> = OnceLock::new();

/// Seconds since program start.
pub fn now() -> f64 {
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

pub fn ease_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

pub fn ease_out_back(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let c1 = 1.70158;
    let c3 = c1 + 1.0;
    1.0 + c3 * (t - 1.0).powi(3) + c1 * (t - 1.0).powi(2)
}

pub struct Monitor {
    pub work: RECT,
    pub scale: f32,
}

pub fn monitor_at(p: POINT) -> Monitor {
    unsafe {
        let hm = MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(hm, &mut mi);
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(hm, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        Monitor { work: mi.rcWork, scale: dx as f32 / 96.0 }
    }
}

pub fn cursor() -> POINT {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    p
}

/// Bring one of our windows to the foreground even though another process owns focus.
pub fn force_foreground(hwnd: HWND) {
    unsafe {
        let fg = GetForegroundWindow();
        let fg_thread = GetWindowThreadProcessId(fg, None);
        let me = GetCurrentThreadId();
        if fg_thread != 0 && fg_thread != me {
            let _ = AttachThreadInput(fg_thread, me, TRUE);
            let _ = SetForegroundWindow(hwnd);
            let _ = SetFocus(hwnd);
            let _ = AttachThreadInput(fg_thread, me, FALSE);
        } else {
            let _ = SetForegroundWindow(hwnd);
            let _ = SetFocus(hwnd);
        }
    }
}

pub fn show_topmost(hwnd: HWND, activate: bool) {
    unsafe {
        let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW | if activate { SET_WINDOW_POS_FLAGS(0) } else { SWP_NOACTIVATE };
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, flags);
    }
}

pub fn x_of(lp: LPARAM) -> i32 {
    (lp.0 & 0xFFFF) as u16 as i16 as i32
}

pub fn y_of(lp: LPARAM) -> i32 {
    ((lp.0 >> 16) & 0xFFFF) as u16 as i16 as i32
}

pub fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Track mouse leave so hover states can reset.
pub fn track_leave(hwnd: HWND) {
    unsafe {
        let mut t = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
        let _ = TrackMouseEvent(&mut t);
    }
}

/// Hand unused pages back to Windows once nothing is on screen (keeps idle memory tiny).
pub fn trim_memory() {
    unsafe {
        let _ = windows::Win32::System::Threading::SetProcessWorkingSetSize(windows::Win32::System::Threading::GetCurrentProcess(), usize::MAX, usize::MAX);
    }
}

/// Opt-in troubleshooting log: set ORBIT_LOG=1 and read %TEMP%\orbit.log.
pub fn log(s: &str) {
    static ON: OnceLock<bool> = OnceLock::new();
    if *ON.get_or_init(|| std::env::var_os("ORBIT_LOG").is_some()) {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(std::env::temp_dir().join("orbit.log")) {
            let _ = writeln!(f, "{:9.3} {s}", now());
        }
    }
}
