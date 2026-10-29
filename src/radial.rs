//! The animated radial (pie) menu.

use std::cell::RefCell;
use std::f32::consts::PI;
use std::rc::Rc;

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::actions::Action;
use crate::explorer::Ctx;
use crate::gfx::{hex, mix, rgba, Align, Surface};
use crate::hud::{ACCENT, RED};
use crate::menu::{Item, Kind};
use crate::ui::{self, ease_out_back, ease_out_cubic, now};

const CLASS: PCWSTR = w!("OrbitRadial");
const R_OUT: f32 = 150.0;
const R_IN: f32 = 56.0;
const PUSH: f32 = 9.0;
const HALF: f32 = R_OUT + PUSH + 34.0;
const PAGE: usize = 10;
const TIMER_ANIM: usize = 1;

struct Level {
    title: String,
    items: Vec<Item>,
    page: usize,
}

#[derive(Clone, Copy, PartialEq)]
enum Hit {
    None,
    Center,
    Item(usize),
}

struct Radial {
    hwnd: HWND,
    surf: Surface,
    scale: f32,
    pos: (i32, i32),
    ctx: Rc<Ctx>,
    stack: Vec<Level>,
    hover: Hit,
    hover_k: Vec<f32>,
    center_k: f32,
    t_open: f64,
    t_level: f64,
    level_dir: i32,
    page_dir: i32,
    closing: Option<f64>,
    last_tick: f64,
    armed: bool,
}

thread_local! {
    static MENU: RefCell<Option<Radial>> = const { RefCell::new(None) };
    static PENDING: RefCell<Option<(Action, Rc<Ctx>)>> = const { RefCell::new(None) };
}

pub fn register() {
    ui::register_class(CLASS, Some(proc_));
}

/// Action chosen by the user, taken by the main window on WM_APP_ACTION.
pub fn take_pending() -> Option<(Action, Rc<Ctx>)> {
    PENDING.with(|p| p.borrow_mut().take())
}

pub fn open(ctx: Rc<Ctx>, title: String, items: Vec<Item>) {
    destroy_now();
    let mon = ui::monitor_at(ctx.pt);
    let s = mon.scale;
    let size = (HALF * 2.0 * s) as i32;
    let x = (ctx.pt.x - size / 2).clamp(mon.work.left - size / 5, mon.work.right - size + size / 5);
    let y = (ctx.pt.y - size / 2).clamp(mon.work.top - size / 5, mon.work.bottom - size + size / 5);
    let Ok(surf) = Surface::new(size, size) else { return };
    let hwnd = ui::create_layered(CLASS, true, false);
    let n = items.len();
    let t = now();
    MENU.with(|m| {
        *m.borrow_mut() = Some(Radial {
            hwnd,
            surf,
            scale: s,
            pos: (x, y),
            ctx,
            stack: vec![Level { title, items, page: 0 }],
            hover: Hit::None,
            hover_k: vec![0.0; n.max(PAGE)],
            center_k: 0.0,
            t_open: t,
            t_level: t,
            level_dir: 0,
            page_dir: 0,
            closing: None,
            last_tick: t,
            armed: false,
        })
    });
    with(|r| r.paint());
    unsafe {
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, size, size, SWP_SHOWWINDOW);
        ui::force_foreground(hwnd);
        SetTimer(hwnd, TIMER_ANIM, 12, None);
        ui::log(&format!("radial open, foreground={}", GetForegroundWindow() == hwnd));
    }
}

fn with<R>(f: impl FnOnce(&mut Radial) -> R) -> Option<R> {
    MENU.with(|m| {
        let mut b = m.try_borrow_mut().ok()?;
        b.as_mut().map(f)
    })
}

fn destroy_now() {
    let old = MENU.with(|m| m.try_borrow_mut().ok().and_then(|mut b| b.take()));
    if let Some(r) = old {
        unsafe {
            let _ = DestroyWindow(r.hwnd);
        }
        drop(r);
        ui::trim_memory();
    }
}

impl Radial {
    fn level(&self) -> &Level {
        self.stack.last().unwrap()
    }

    fn pages(&self) -> usize {
        self.level().items.len().div_ceil(PAGE).max(1)
    }

    /// Items per page, spread evenly (10 items -> 5 + 5 rather than 8 + 2).
    fn per_page(&self) -> usize {
        self.level().items.len().div_ceil(self.pages()).max(1)
    }

    fn page_items(&self) -> &[Item] {
        let l = self.level();
        let start = (l.page * self.per_page()).min(l.items.len());
        &l.items[start..(start + self.per_page()).min(l.items.len())]
    }

    fn hit(&self, px: i32, py: i32) -> Hit {
        let dx = px as f32 / self.scale - HALF;
        let dy = py as f32 / self.scale - HALF;
        let d = (dx * dx + dy * dy).sqrt();
        if d < R_IN - 2.0 {
            return Hit::Center;
        }
        if d > R_OUT + PUSH + 18.0 {
            return Hit::None;
        }
        let n = self.page_items().len();
        if n == 0 {
            return Hit::None;
        }
        let step = 360.0 / n as f32;
        // wedge 0 is centered at the top (-90 degrees)
        let mut a = dy.atan2(dx).to_degrees() + 90.0 + step / 2.0;
        a = a.rem_euclid(360.0);
        Hit::Item(((a / step) as usize).min(n - 1))
    }

    fn set_hover(&mut self, h: Hit) {
        if h != self.hover {
            self.hover = h;
        }
    }

    fn change_page(&mut self, d: i32) {
        let pages = self.pages() as i32;
        if pages <= 1 {
            return;
        }
        let l = self.stack.last_mut().unwrap();
        l.page = ((l.page as i32 + d).rem_euclid(pages)) as usize;
        self.page_dir = d.signum();
        self.level_dir = 0;
        self.t_level = now();
        self.hover = Hit::None;
    }

    fn back(&mut self) {
        if self.stack.len() > 1 {
            self.stack.pop();
            self.level_dir = -1;
            self.page_dir = 0;
            self.t_level = now();
            self.hover = Hit::None;
        } else {
            self.close();
        }
    }

    fn close(&mut self) {
        if self.closing.is_none() {
            self.closing = Some(now());
        }
    }

    fn activate(&mut self, h: Hit) {
        match h {
            Hit::None => self.close(),
            Hit::Center => self.back(),
            Hit::Item(i) => {
                let Some(item) = self.page_items().get(i) else { return };
                match &item.kind {
                    Kind::Sub(title, items) => {
                        let lvl = Level { title: title.clone(), items: items.clone(), page: 0 };
                        if self.hover_k.len() < lvl.items.len() {
                            self.hover_k.resize(lvl.items.len(), 0.0);
                        }
                        self.stack.push(lvl);
                        self.level_dir = 1;
                        self.page_dir = 0;
                        self.t_level = now();
                        self.hover = Hit::None;
                        self.hover_k.iter_mut().for_each(|k| *k = 0.0);
                    }
                    Kind::Run(action) => {
                        let action = action.clone();
                        self.run(action);
                    }
                }
            }
        }
    }

    fn run(&mut self, action: Action) {
        PENDING.with(|p| *p.borrow_mut() = Some((action, self.ctx.clone())));
        self.close();
        unsafe {
            let _ = PostMessageW(crate::main_hwnd(), crate::WM_APP_ACTION, WPARAM(0), LPARAM(0));
        }
    }

    /// Explorer-style keys while the menu is open: Ctrl+C/X/V, F2, Del.
    fn shortcut(&mut self, vk: VIRTUAL_KEY) -> bool {
        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
        let on_items = !self.ctx.background;
        let action = match vk {
            VK_F2 if on_items => Action::Rename,
            VK_DELETE if on_items => Action::Delete,
            VIRTUAL_KEY(0x43) if ctrl && on_items => Action::Copy,
            VIRTUAL_KEY(0x58) if ctrl && on_items => Action::Cut,
            VIRTUAL_KEY(0x56) if ctrl => Action::Paste,
            _ => return false,
        };
        self.run(action);
        true
    }

    /// Advance animation. Returns false when the menu has fully closed.
    fn tick(&mut self) -> bool {
        let t = now();
        let dt = (t - self.last_tick).clamp(0.0, 0.1) as f32;
        self.last_tick = t;
        let n = self.page_items().len();
        for i in 0..self.hover_k.len() {
            let target = if self.hover == Hit::Item(i) && i < n { 1.0 } else { 0.0 };
            self.hover_k[i] += (target - self.hover_k[i]) * (dt * 16.0).min(1.0);
        }
        let ct = if self.hover == Hit::Center { 1.0 } else { 0.0 };
        self.center_k += (ct - self.center_k) * (dt * 16.0).min(1.0);
        if let Some(tc) = self.closing {
            if t - tc > 0.16 {
                return false;
            }
        }
        self.paint();
        true
    }

    fn paint(&mut self) {
        let t = now();
        let mut alpha = ease_out_cubic(((t - self.t_open) / 0.18) as f32);
        let mut zoom = 0.86 + 0.14 * ease_out_back(((t - self.t_open) / 0.32) as f32);
        if let Some(tc) = self.closing {
            let k = ((t - tc) / 0.16) as f32;
            alpha *= 1.0 - ease_out_cubic(k);
            zoom *= 1.0 - 0.07 * ease_out_cubic(k);
        }
        let lk = ease_out_cubic(((t - self.t_level) / 0.28) as f32);
        let level_zoom = match self.level_dir {
            1 => 1.10 - 0.10 * lk,
            -1 => 0.90 + 0.10 * lk,
            _ => 1.0,
        };
        let rot = self.page_dir as f32 * 26.0 * (1.0 - lk);
        let items: Vec<(String, char, u32, bool)> =
            self.page_items().iter().map(|i| (i.label.clone(), i.icon, i.color, matches!(i.kind, Kind::Sub(..)))).collect();
        let n = items.len();
        let hover = self.hover;
        let hover_k = self.hover_k.clone();
        let center_k = self.center_k;
        let depth = self.stack.len();
        let pages = self.pages();
        let page = self.level().page;
        let (title, hint) = match hover {
            Hit::Item(i) if i < n => {
                let it = &self.page_items()[i];
                let hint = if !it.hint.is_empty() {
                    it.hint.clone()
                } else if matches!(it.kind, Kind::Sub(..)) {
                    "Open  ›".to_string()
                } else {
                    "Click to run".to_string()
                };
                (it.label.clone(), hint)
            }
            Hit::Center => (if depth > 1 { "Back".into() } else { "Close".into() }, if depth > 1 { "Right-click also goes back".into() } else { "Esc".into() }),
            _ => (
                self.level().title.clone(),
                if depth > 1 {
                    "‹ Back".into()
                } else if pages > 1 {
                    "Scroll for more".into()
                } else {
                    "Esc to close".into()
                },
            ),
        };
        let open_t = t - self.t_open.max(self.t_level);
        let s = self.scale;
        self.surf.draw(s, |c| {
            let cx = HALF;
            let cy = HALF;
            let z = zoom;
            let ro = R_OUT * z;
            let ri = R_IN * z;
            // soft shadow + base disc
            c.shadow_circle(cx, cy, ro + 4.0, 22.0, 0.55);
            c.fill_circle(cx, cy, ro + 5.0, rgba(17, 17, 22, 0.96));
            c.stroke_circle(cx, cy, ro + 5.0, 1.0, rgba(255, 255, 255, 0.07));

            let zl = z * level_zoom;
            let ring_alpha = lk.max(if self.level_dir == 0 && self.page_dir == 0 { 1.0 } else { 0.0 });
            if n > 0 {
                let step = 360.0 / n as f32;
                for (i, (label, icon, color, is_sub)) in items.iter().enumerate() {
                    let hk = hover_k.get(i).copied().unwrap_or(0.0);
                    // staggered grow-in
                    let wk = ease_out_back(((open_t - i as f64 * 0.018) / 0.30) as f32).max(0.0);
                    let a_mid = -90.0 + i as f32 * step + rot;
                    let a0 = a_mid - step / 2.0;
                    let a1 = a_mid + step / 2.0;
                    let r0 = R_IN * zl + 5.0;
                    let r1 = r0 + ((R_OUT - R_IN - 9.0) * zl + PUSH * hk) * wk;
                    let base = rgba(255, 255, 255, 0.05);
                    let col = mix(base, hex(*color, 0.95), hk);
                    c.wedge(cx, cy, r0, r1.max(r0 + 0.5), a0, a1, 3.5, crate::gfx::fade(col, ring_alpha));
                    if hk > 0.01 {
                        // outer glow edge
                        c.arc(cx, cy, r1 + 3.0, a0 + 3.0, a1 - 3.0, 2.0, hex(*color, 0.55 * hk * ring_alpha));
                    }
                    let mid = (r0 + r1) / 2.0;
                    let ar = a_mid * PI / 180.0;
                    let (px, py) = (cx + mid * ar.cos(), cy + mid * ar.sin());
                    let icol = mix(hex(*color, 1.0), rgba(255, 255, 255, 1.0), hk);
                    let tcol = mix(rgba(255, 255, 255, 0.80), rgba(255, 255, 255, 1.0), hk);
                    let ta = ring_alpha * wk.min(1.0);
                    c.icon(*icon, px, py - 9.0, 19.0 + 2.0 * hk, crate::gfx::fade(icol, ta));
                    let lw = (2.0 * PI * mid / n as f32 - 10.0).clamp(54.0, 92.0);
                    c.text(label, px - lw / 2.0, py + 4.0, lw, 18.0, 11.0, 600, crate::gfx::fade(tcol, ta), Align::Center);
                    if *is_sub {
                        let er = r1 - 7.0;
                        c.fill_circle(cx + er * ar.cos(), cy + er * ar.sin(), 2.2, crate::gfx::fade(rgba(255, 255, 255, 0.35 + 0.5 * hk), ta));
                    }
                }
            }

            // center hub
            let hub = ri - 2.0 + 3.0 * center_k;
            let hub_col = mix(rgba(30, 30, 38, 1.0), if depth > 1 { hex(ACCENT, 0.55) } else { hex(RED, 0.45) }, center_k * 0.8);
            c.fill_circle(cx, cy, hub, hub_col);
            c.stroke_circle(cx, cy, hub, 1.0, rgba(255, 255, 255, 0.10));
            let tw = hub * 2.0 - 16.0;
            c.text(&title, cx - tw / 2.0, cy - 16.0, tw, 20.0, 12.5, 650, rgba(255, 255, 255, 0.96), Align::Center);
            c.text(&hint, cx - tw / 2.0, cy + 2.0, tw, 16.0, 10.0, 500, rgba(255, 255, 255, 0.55), Align::Center);
            if pages > 1 {
                let dot = 5.0;
                let total = pages as f32 * dot + (pages as f32 - 1.0) * 4.0;
                let mut x = cx - total / 2.0;
                for p in 0..pages {
                    let on = p == page;
                    let w = if on { dot * 2.0 } else { dot };
                    c.fill_rrect(x - if on { dot / 2.0 } else { 0.0 }, cy + 22.0, w, dot, dot / 2.0, rgba(255, 255, 255, if on { 0.9 } else { 0.3 }));
                    x += dot + 4.0;
                }
            }
        });
        self.surf.present(self.hwnd, Some(self.pos), alpha);
    }
}

unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => return LRESULT(MA_ACTIVATE as isize),
        WM_MOUSEMOVE => {
            ui::track_leave(hwnd);
            with(|r| {
                if !r.armed {
                    let c = ui::cursor();
                    if (c.x - r.ctx.pt.x).abs() + (c.y - r.ctx.pt.y).abs() < 6 {
                        return;
                    }
                    r.armed = true;
                }
                let h = r.hit(ui::x_of(lp), ui::y_of(lp));
                r.set_hover(h);
            });
            return LRESULT(0);
        }
        windows::Win32::UI::Controls::WM_MOUSELEAVE => {
            with(|r| r.set_hover(Hit::None));
            return LRESULT(0);
        }
        WM_LBUTTONUP => {
            with(|r| {
                let h = r.hit(ui::x_of(lp), ui::y_of(lp));
                r.activate(h);
            });
            return LRESULT(0);
        }
        WM_RBUTTONUP => {
            with(|r| r.back());
            return LRESULT(0);
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp.0 >> 16) & 0xFFFF) as u16 as i16;
            with(|r| r.change_page(if delta < 0 { 1 } else { -1 }));
            return LRESULT(0);
        }
        WM_KEYDOWN => {
            let vk = VIRTUAL_KEY(wp.0 as u16);
            with(|r| match vk {
                VK_ESCAPE | VK_BACK => r.back(),
                VK_RIGHT | VK_DOWN | VK_TAB => r.change_page(1),
                VK_LEFT | VK_UP => r.change_page(-1),
                VK_RETURN => {
                    let h = r.hover;
                    r.activate(h)
                }
                _ if r.shortcut(vk) => {}
                _ => {
                    let k = vk.0;
                    if (0x31..=0x39).contains(&k) {
                        r.activate(Hit::Item((k - 0x31) as usize));
                    }
                }
            });
            return LRESULT(0);
        }
        WM_ACTIVATE => {
            if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE {
                ui::log("radial deactivated -> close");
                with(|r| r.close());
            }
            return LRESULT(0);
        }
        WM_TIMER => {
            let alive = with(|r| r.tick()).unwrap_or(true);
            if !alive {
                destroy_now();
            }
            return LRESULT(0);
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

/// Render a finished (fully animated-in) menu straight to a PNG, without a window.
/// `open` = wedge indices of submenus to enter; `hover` = highlighted wedge.
pub fn snapshot(ctx: Rc<Ctx>, title: String, items: Vec<Item>, open: &[usize], page: usize, hover: Option<usize>, scale: f32, out: &std::path::Path) -> std::result::Result<(), String> {
    let size = (HALF * 2.0 * scale) as i32;
    let surf = Surface::new(size, size).map_err(|e| e.message())?;
    let t = now() - 10.0;
    let n = items.len();
    let mut r = Radial {
        hwnd: HWND::default(),
        surf,
        scale,
        pos: (0, 0),
        ctx,
        stack: vec![Level { title, items, page: 0 }],
        hover: Hit::None,
        hover_k: vec![0.0; n.max(PAGE)],
        center_k: 0.0,
        t_open: t,
        t_level: t,
        level_dir: 0,
        page_dir: 0,
        closing: None,
        last_tick: t,
        armed: true,
    };
    for &i in open {
        let Some(Item { kind: Kind::Sub(title, items), .. }) = r.page_items().get(i).cloned() else { break };
        r.hover_k.resize(r.hover_k.len().max(items.len()), 0.0);
        r.stack.push(Level { title, items, page: 0 });
    }
    r.stack.last_mut().unwrap().page = page;
    r.level_dir = 0;
    if let Some(h) = hover {
        r.hover = Hit::Item(h);
        r.hover_k[h] = 1.0;
    }
    r.paint();
    r.surf.save_png(out)
}
