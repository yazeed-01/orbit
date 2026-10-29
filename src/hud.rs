//! Heads-up display: animated progress rings over files and toast notifications.

use std::cell::RefCell;

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::gfx::{self, hex, rgba, Align, Surface};
use crate::ui::{self, ease_out_back, ease_out_cubic, now};

pub const ACCENT: u32 = 0x7C5CFF;
pub const GREEN: u32 = 0x22C55E;
pub const RED: u32 = 0xEF4444;
pub const AMBER: u32 = 0xF5A524;

#[derive(Clone, Copy, PartialEq)]
pub enum ToastKind {
    Info,
    Ok,
    Error,
    Wait,
}

#[derive(PartialEq)]
enum State {
    Running,
    Done(bool, f64),
}

struct Ring {
    id: u32,
    hwnd: HWND,
    surf: Surface,
    scale: f32,
    pos: (i32, i32),
    caption: String,
    target: f32,
    shown: f32,
    state: State,
    t0: f64,
}

struct Toast {
    hwnd: HWND,
    surf: Surface,
    scale: f32,
    text: String,
    kind: ToastKind,
    t0: f64,
    dur: f64,
    pos: (i32, i32),
    w: f32,
    h: f32,
}

#[derive(Default)]
struct Hud {
    rings: Vec<Ring>,
    toast: Option<Toast>,
    timer: bool,
    last: f64,
}

thread_local! {
    static HUD: RefCell<Hud> = RefCell::new(Hud::default());
}

const CLASS: PCWSTR = w!("OrbitHud");
const RING_W: f32 = 140.0;
const RING_H: f32 = 116.0;
const RING_CY: f32 = 42.0;
pub const TIMER_HUD: usize = 77;

pub fn register() {
    ui::register_class(CLASS, Some(proc_));
}

unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_MOUSEACTIVATE {
        return LRESULT(MA_NOACTIVATE as isize);
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

fn ensure_timer(h: &mut Hud) {
    if !h.timer {
        h.timer = true;
        unsafe { SetTimer(crate::main_hwnd(), TIMER_HUD, 14, None) };
    }
}

pub fn start(id: u32, anchor: POINT, caption: &str) {
    let mon = ui::monitor_at(anchor);
    let s = mon.scale;
    let (w, h) = ((RING_W * s) as i32, (RING_H * s) as i32);
    let Ok(surf) = Surface::new(w, h) else { return };
    HUD.with(|hud| {
        let mut hud = hud.borrow_mut();
        // avoid stacking rings on top of each other
        let mut x = anchor.x - w / 2;
        let y = (anchor.y - (RING_CY * s) as i32).clamp(mon.work.top, mon.work.bottom - h);
        while hud.rings.iter().any(|r| (r.pos.0 - x).abs() < w - 8 && (r.pos.1 - y).abs() < h) {
            x += w;
        }
        let x = x.clamp(mon.work.left, mon.work.right - w);
        let hwnd = ui::create_layered(CLASS, false, true);
        let mut ring = Ring { id, hwnd, surf, scale: s, pos: (x, y), caption: caption.to_string(), target: -1.0, shown: 0.0, state: State::Running, t0: now() };
        paint_ring(&mut ring, now());
        ui::show_topmost(ring.hwnd, false);
        hud.rings.push(ring);
        ensure_timer(&mut hud);
    });
}

pub fn progress(id: u32, f: f32) {
    HUD.with(|hud| {
        if let Some(r) = hud.borrow_mut().rings.iter_mut().find(|r| r.id == id) {
            if f >= 0.0 && r.target < 0.0 {
                r.shown = 0.0;
            }
            r.target = f.min(1.0);
        }
    });
}

pub fn caption(id: u32, s: &str) {
    HUD.with(|hud| {
        if let Some(r) = hud.borrow_mut().rings.iter_mut().find(|r| r.id == id) {
            r.caption = s.to_string();
        }
    });
}

pub fn finish(id: u32, ok: bool) {
    HUD.with(|hud| {
        if let Some(r) = hud.borrow_mut().rings.iter_mut().find(|r| r.id == id) {
            r.target = 1.0;
            r.caption = if ok { "Done".into() } else { "Failed".into() };
            r.state = State::Done(ok, now());
        }
    });
}

pub fn toast(text: &str, kind: ToastKind) {
    let mut text: String = text.trim().to_string();
    if text.chars().count() > 260 {
        text = text.chars().take(257).collect::<String>() + "…";
    }
    let p = ui::cursor();
    let mon = ui::monitor_at(p);
    let s = mon.scale;
    let max_text = 460.0;
    let (tw, th) = gfx::measure(&text, 13.5, 500, max_text, true);
    let w = (tw.min(max_text) + 72.0).max(180.0);
    let h = (th + 26.0).max(48.0);
    let pad = 24.0;
    let (pw, ph) = (((w + pad * 2.0) * s) as i32, ((h + pad * 2.0) * s) as i32);
    let Ok(surf) = Surface::new(pw, ph) else { return };
    HUD.with(|hud| {
        let mut hud = hud.borrow_mut();
        let hwnd = match hud.toast.take() {
            Some(t) => t.hwnd,
            None => ui::create_layered(CLASS, false, true),
        };
        let cx = (mon.work.left + mon.work.right) / 2;
        let pos = (cx - pw / 2, mon.work.bottom - ph - (40.0 * s) as i32);
        let dur = match kind {
            ToastKind::Error => 6.0,
            ToastKind::Wait => 3600.0,
            _ => 2.6 + text.len() as f64 / 60.0,
        };
        hud.toast = Some(Toast { hwnd, surf, scale: s, text, kind, t0: now(), dur, pos, w, h });
        ui::show_topmost(hwnd, false);
        ensure_timer(&mut hud);
    });
    tick();
}

/// Dismiss a "Wait" toast (e.g. after a download completes).
pub fn dismiss_wait() {
    HUD.with(|hud| {
        if let Some(t) = hud.borrow_mut().toast.as_mut() {
            if t.kind == ToastKind::Wait {
                t.dur = (now() - t.t0).max(0.25);
            }
        }
    });
}

/// Animation tick, driven by the main window's timer.
pub fn tick() {
    let t = now();
    HUD.with(|hud| {
        let Ok(mut hud) = hud.try_borrow_mut() else { return };
        let dt = (t - hud.last).clamp(0.0, 0.1) as f32;
        hud.last = t;
        hud.rings.retain_mut(|r| {
            let rate = if r.target >= r.shown { 10.0 } else { 30.0 };
            if r.target >= 0.0 {
                r.shown += (r.target - r.shown) * (dt * rate).min(1.0);
            }
            let alive = paint_ring(r, t);
            if !alive {
                unsafe {
                    let _ = DestroyWindow(r.hwnd);
                }
            }
            alive
        });
        if let Some(toast) = hud.toast.as_mut() {
            if !paint_toast(toast, t) {
                unsafe {
                    let _ = DestroyWindow(toast.hwnd);
                }
                hud.toast = None;
            }
        }
        if hud.rings.is_empty() && hud.toast.is_none() && hud.timer {
            hud.timer = false;
            unsafe {
                let _ = KillTimer(crate::main_hwnd(), TIMER_HUD);
            ui::trim_memory();
            }
        }
    });
}

/// Returns false when the ring has finished fading out.
fn paint_ring(r: &mut Ring, t: f64) -> bool {
    let appear = ((t - r.t0) / 0.35) as f32;
    let mut alpha = ease_out_cubic(appear * 1.4);
    let mut k_state = 0.0f32;
    let mut done = None;
    if let State::Done(ok, ts) = r.state {
        let e = (t - ts) as f32;
        k_state = (e / 0.35).min(1.0);
        done = Some(ok);
        if e > 1.5 {
            alpha *= 1.0 - ((e - 1.5) / 0.35).min(1.0);
        }
        if e > 1.85 {
            return false;
        }
    }
    let pop = ease_out_back(appear);
    let s = r.scale;
    let caption = r.caption.clone();
    let shown = r.shown;
    let indeterminate = r.target < 0.0;
    r.surf.draw(s, |c| {
        let cx = RING_W / 2.0;
        let cy = RING_CY;
        let bg_r = 30.0 * pop.max(0.01) + if done.is_some() { 3.0 * (1.0 - (k_state * 2.0 - 1.0).abs()) } else { 0.0 };
        c.shadow_circle(cx, cy, bg_r, 10.0, 0.35);
        let base = rgba(22, 22, 28, 0.96);
        let bg = match done {
            Some(true) => gfx::mix(base, hex(GREEN, 1.0), k_state),
            Some(false) => gfx::mix(base, hex(RED, 1.0), k_state),
            None => base,
        };
        c.fill_circle(cx, cy, bg_r, bg);
        c.stroke_circle(cx, cy, bg_r, 1.0, rgba(255, 255, 255, 0.08));
        let rr = 22.0 * pop.max(0.01);
        match done {
            None => {
                c.stroke_circle(cx, cy, rr, 4.0, rgba(255, 255, 255, 0.09));
                if indeterminate {
                    let rot = (t * 320.0 % 360.0) as f32;
                    let len = 70.0 + 50.0 * ((t * 2.2).sin() as f32 + 1.0) * 0.5;
                    c.arc(cx, cy, rr, rot, rot + len, 4.0, hex(ACCENT, 1.0));
                } else {
                    c.arc(cx, cy, rr, -90.0, -90.0 + 360.0 * shown.max(0.01), 4.0, hex(ACCENT, 1.0));
                    let pct = format!("{}%", (shown * 100.0).round() as i32);
                    c.text(&pct, cx - 20.0, cy - 9.0, 40.0, 18.0, 11.5, 650, rgba(255, 255, 255, 0.95), Align::Center);
                }
            }
            Some(ok) => {
                // ring completes then check / cross draws itself
                c.stroke_circle(cx, cy, rr, 4.0 * (1.0 - k_state), rgba(255, 255, 255, 0.5 * (1.0 - k_state)));
                let k = ease_out_cubic(((t - r.t0) as f32).min(k_state * 1.4));
                let white = rgba(255, 255, 255, 1.0);
                if ok {
                    let pts = [(-10.0f32, 0.5f32), (-3.5, 7.0), (10.5, -7.0)];
                    let total = 9.2 + 19.8;
                    let len = k * total;
                    let mut path = vec![(cx + pts[0].0, cy + pts[0].1)];
                    if len <= 9.2 {
                        let f = len / 9.2;
                        path.push((cx + pts[0].0 + (pts[1].0 - pts[0].0) * f, cy + pts[0].1 + (pts[1].1 - pts[0].1) * f));
                    } else {
                        path.push((cx + pts[1].0, cy + pts[1].1));
                        let f = (len - 9.2) / 19.8;
                        path.push((cx + pts[1].0 + (pts[2].0 - pts[1].0) * f, cy + pts[1].1 + (pts[2].1 - pts[1].1) * f));
                    }
                    c.polyline(&path, 3.6, white);
                } else {
                    let d = 8.0 * k;
                    c.line(cx - d, cy - d, cx + d, cy + d, 3.4, white);
                    c.line(cx + d, cy - d, cx - d, cy + d, 3.4, white);
                }
            }
        }
        if !caption.is_empty() {
            let (tw, _) = gfx::measure(&caption, 11.0, 600, RING_W - 16.0, false);
            let pw = (tw + 20.0).min(RING_W - 4.0);
            let py = cy + 40.0;
            c.fill_rrect(cx - pw / 2.0, py, pw, 22.0, 11.0, rgba(22, 22, 28, 0.92));
            c.stroke_rrect(cx - pw / 2.0, py, pw, 22.0, 11.0, 1.0, rgba(255, 255, 255, 0.07));
            c.text(&caption, cx - pw / 2.0 + 8.0, py, pw - 16.0, 22.0, 11.0, 600, rgba(255, 255, 255, 0.85), Align::Center);
        }
    });
    r.surf.present(r.hwnd, Some(r.pos), alpha);
    true
}

fn paint_toast(tw: &mut Toast, t: f64) -> bool {
    let e = (t - tw.t0) as f32;
    let din = ease_out_cubic(e / 0.25);
    let out_start = tw.dur as f32;
    let dout = if e > out_start { ((e - out_start) / 0.25).min(1.0) } else { 0.0 };
    if dout >= 1.0 {
        return false;
    }
    let alpha = din * (1.0 - dout);
    let slide = (1.0 - din) * 14.0 + dout * 8.0;
    let (w, h, s) = (tw.w, tw.h, tw.scale);
    let text = tw.text.clone();
    let kind = tw.kind;
    tw.surf.draw(s, |c| {
        let (x, y) = (24.0, 24.0);
        c.shadow_rrect(x, y, w, h, 16.0, 14.0, 0.45);
        c.fill_rrect(x, y, w, h, 16.0, rgba(24, 24, 30, 0.97));
        c.stroke_rrect(x, y, w, h, 16.0, 1.0, rgba(255, 255, 255, 0.08));
        let (col, glyph) = match kind {
            ToastKind::Info => (ACCENT, '\u{E946}'),
            ToastKind::Ok => (GREEN, '\u{E73E}'),
            ToastKind::Error => (RED, '\u{E711}'),
            ToastKind::Wait => (AMBER, '\u{E896}'),
        };
        let icx = x + 28.0;
        let icy = y + h / 2.0;
        c.fill_circle(icx, icy, 13.0, hex(col, 0.18));
        if kind == ToastKind::Wait {
            let rot = (t * 300.0 % 360.0) as f32;
            c.arc(icx, icy, 13.0, rot, rot + 100.0, 2.4, hex(col, 1.0));
        }
        c.icon(glyph, icx, icy, 12.0, hex(col, 1.0));
        c.text_wrap(&text, x + 50.0, y, w - 64.0, h, 13.5, 500, rgba(255, 255, 255, 0.92), Align::Left);
    });
    tw.surf.present(tw.hwnd, Some((tw.pos.0, tw.pos.1 + (slide * s) as i32)), alpha);
    true
}

/// README screenshots: a progress ring (`done` = None) or a finished one.
pub fn snapshot_ring(progress: f32, done: Option<bool>, caption: &str, scale: f32, out: &std::path::Path) -> std::result::Result<(), String> {
    let surf = Surface::new((RING_W * scale) as i32, (RING_H * scale) as i32).map_err(|e| e.message())?;
    let t = now();
    let mut r = Ring {
        id: 0,
        hwnd: HWND::default(),
        surf,
        scale,
        pos: (0, 0),
        caption: caption.into(),
        target: progress,
        shown: progress,
        state: match done {
            Some(ok) => State::Done(ok, t - 0.6),
            None => State::Running,
        },
        t0: t - 5.0,
    };
    paint_ring(&mut r, t);
    r.surf.save_png(out)
}

pub fn snapshot_toast(text: &str, kind: ToastKind, scale: f32, out: &std::path::Path) -> std::result::Result<(), String> {
    let (tw, th) = gfx::measure(text, 13.5, 500, 460.0, true);
    let w = (tw.min(460.0) + 72.0).max(180.0);
    let h = (th + 26.0).max(48.0);
    let surf = Surface::new(((w + 48.0) * scale) as i32, ((h + 48.0) * scale) as i32).map_err(|e| e.message())?;
    let mut t = Toast { hwnd: HWND::default(), surf, scale, text: text.into(), kind, t0: now() - 1.0, dur: 60.0, pos: (0, 0), w, h };
    paint_toast(&mut t, now());
    t.surf.save_png(out)
}
