//! Background jobs and the worker → UI thread message queue.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use windows::Win32::Foundation::*;
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::hud::{self, ToastKind};

type UiFn = Box<dyn FnOnce() + Send>;

static QUEUE: Mutex<Vec<UiFn>> = Mutex::new(Vec::new());
static NEXT_ID: AtomicU32 = AtomicU32::new(1);

/// Run a closure on the UI thread (thread-safe; callable from any worker).
pub fn post_ui(f: impl FnOnce() + Send + 'static) {
    QUEUE.lock().unwrap().push(Box::new(f));
    unsafe {
        let _ = PostMessageW(crate::main_hwnd(), crate::WM_APP_EVENT, WPARAM(0), LPARAM(0));
    }
}

/// Called by the main window on WM_APP_EVENT.
pub fn drain() {
    let items = std::mem::take(&mut *QUEUE.lock().unwrap());
    for f in items {
        f();
    }
}

pub fn toast(text: impl Into<String>, kind: ToastKind) {
    let text = text.into();
    post_ui(move || hud::toast(&text, kind));
}

/// Handle given to a running job for reporting progress.
pub struct Job {
    pub id: u32,
    last: Mutex<f64>,
}

impl Job {
    /// 0.0..=1.0, or negative for "indeterminate". Throttled to ~30 updates/s.
    pub fn progress(&self, f: f32) {
        let t = crate::ui::now();
        {
            let mut last = self.last.lock().unwrap();
            if t - *last < 0.033 && f < 1.0 {
                return;
            }
            *last = t;
        }
        let id = self.id;
        post_ui(move || hud::progress(id, f));
    }

    pub fn caption(&self, s: impl Into<String>) {
        let id = self.id;
        let s = s.into();
        post_ui(move || hud::caption(id, &s));
    }
}

pub type JobResult = Result<String, String>;

/// Start a job on a worker thread with a progress ring shown at `anchor`.
pub fn spawn(caption: impl Into<String>, anchor: POINT, f: impl FnOnce(&Job) -> JobResult + Send + 'static) {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    hud::start(id, anchor, &caption.into());
    std::thread::spawn(move || {
        let job = Job { id, last: Mutex::new(0.0) };
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&job)))
            .unwrap_or_else(|_| Err("Unexpected error".into()));
        post_ui(move || match res {
            Ok(msg) => {
                hud::finish(id, true);
                if !msg.is_empty() {
                    hud::toast(&msg, ToastKind::Ok);
                }
            }
            Err(e) => {
                hud::finish(id, false);
                hud::toast(&e, ToastKind::Error);
            }
        });
    });
}
