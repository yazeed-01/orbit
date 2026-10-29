//! A small dark, modal text prompt.

use std::cell::{Cell, RefCell};

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::ui;

const CLASS: PCWSTR = w!("OrbitInput");
const BG: COLORREF = COLORREF(0x00221C1C); // BGR
const FIELD: COLORREF = COLORREF(0x002E2626);

struct State {
    title: Vec<u16>,
    hint: Vec<u16>,
    scale: f32,
    font_title: HFONT,
    font_text: HFONT,
    font_hint: HFONT,
    field_brush: HBRUSH,
    edit: HWND,
}

thread_local! {
    static ST: RefCell<Option<State>> = const { RefCell::new(None) };
    static CANCEL: Cell<bool> = const { Cell::new(false) };
    static ACTIVATED: Cell<bool> = const { Cell::new(false) };
}

pub fn register() {
    unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(proc_),
            hInstance: ui::hinstance(),
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: CreateSolidBrush(BG),
            ..Default::default()
        };
        RegisterClassW(&wc);
    }
}

fn font(px: f32, weight: i32) -> HFONT {
    unsafe {
        CreateFontW(
            -(px.round() as i32),
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET.0 as u32,
            OUT_DEFAULT_PRECIS.0 as u32,
            CLIP_DEFAULT_PRECIS.0 as u32,
            CLEARTYPE_QUALITY.0 as u32,
            0,
            w!("Segoe UI"),
        )
    }
}

/// Show the prompt and block (pumping messages) until Enter / Esc.
pub fn prompt(title: &str, hint: &str, default: &str) -> Option<String> {
    let p = ui::cursor();
    let mon = ui::monitor_at(p);
    let s = mon.scale;
    let (w, h) = ((460.0 * s) as i32, (156.0 * s) as i32);
    let x = ((mon.work.left + mon.work.right) / 2 - w / 2).max(mon.work.left);
    let y = ((mon.work.top + mon.work.bottom) / 2 - h / 2 - (60.0 * s) as i32).max(mon.work.top);
    CANCEL.with(|c| c.set(false));
    ACTIVATED.with(|c| c.set(false));
    unsafe {
        let hwnd = CreateWindowExW(WS_EX_TOPMOST | WS_EX_TOOLWINDOW, CLASS, w!("Orbit"), WS_POPUP, x, y, w, h, None, None, ui::hinstance(), None).ok()?;
        let dark = BOOL(1);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as _, 4);
        let corner = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &corner as *const _ as _, 4);
        let border = COLORREF(0x00FF5C7C); // accent (BGR of #7C5CFF)
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, &border as *const _ as _, 4);

        let pad = (22.0 * s) as i32;
        let edit_y = (52.0 * s) as i32;
        let edit_h = (34.0 * s) as i32;
        let inner = (10.0 * s) as i32;
        let edit = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("EDIT"),
            &HSTRING::from(default),
            WS_CHILD | WS_VISIBLE | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
            pad + inner,
            edit_y + (edit_h - (22.0 * s) as i32) / 2,
            w - 2 * pad - 2 * inner,
            (22.0 * s) as i32,
            hwnd,
            None,
            ui::hinstance(),
            None,
        )
        .ok()?;
        let st = State {
            title: title.encode_utf16().collect(),
            hint: hint.encode_utf16().collect(),
            scale: s,
            font_title: font(16.0 * s, 600),
            font_text: font(15.0 * s, 400),
            font_hint: font(12.0 * s, 400),
            field_brush: CreateSolidBrush(FIELD),
            edit,
        };
        SendMessageW(edit, WM_SETFONT, WPARAM(st.font_text.0 as usize), LPARAM(1));
        ST.with(|x| *x.borrow_mut() = Some(st));
        let _ = ShowWindow(hwnd, SW_SHOW);
        ui::force_foreground(hwnd);
        let _ = SetFocus(edit);
        SendMessageW(edit, windows::Win32::UI::Controls::EM_SETSEL, WPARAM(0), LPARAM(-1));

        let mut result = None;
        let mut msg = MSG::default();
        loop {
            if CANCEL.with(|c| c.get()) {
                break;
            }
            if !GetMessageW(&mut msg, None, 0, 0).as_bool() {
                PostQuitMessage(msg.wParam.0 as i32);
                break;
            }
            if msg.message == WM_KEYDOWN && (msg.hwnd == edit || msg.hwnd == hwnd) {
                if msg.wParam.0 == VK_RETURN.0 as usize {
                    let mut buf = vec![0u16; 1024];
                    let n = GetWindowTextW(edit, &mut buf);
                    result = Some(String::from_utf16_lossy(&buf[..n as usize]));
                    break;
                }
                if msg.wParam.0 == VK_ESCAPE.0 as usize {
                    break;
                }
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = DestroyWindow(hwnd);
        if let Some(st) = ST.with(|x| x.borrow_mut().take()) {
            let _ = DeleteObject(st.font_title);
            let _ = DeleteObject(st.font_text);
            let _ = DeleteObject(st.font_hint);
            let _ = DeleteObject(st.field_brush);
        }
        result
    }
}

unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            ST.with(|st| {
                if let Some(st) = st.borrow().as_ref() {
                    let s = st.scale;
                    let mut rc = RECT::default();
                    let _ = GetClientRect(hwnd, &mut rc);
                    let bg = CreateSolidBrush(BG);
                    FillRect(hdc, &rc, bg);
                    let _ = DeleteObject(bg);
                    SetBkMode(hdc, TRANSPARENT);
                    let pad = (22.0 * s) as i32;
                    // title
                    SelectObject(hdc, st.font_title);
                    SetTextColor(hdc, COLORREF(0x00FFFFFF));
                    let mut r = RECT { left: pad, top: (16.0 * s) as i32, right: rc.right - pad, bottom: (42.0 * s) as i32 };
                    let mut t = st.title.clone();
                    DrawTextW(hdc, &mut t, &mut r, DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS);
                    // field
                    let edit_y = (52.0 * s) as i32;
                    let edit_h = (34.0 * s) as i32;
                    let pen = CreatePen(PS_SOLID, (1.5 * s).max(1.0) as i32, COLORREF(0x00FF5C7C));
                    let old_pen = SelectObject(hdc, pen);
                    let old_br = SelectObject(hdc, st.field_brush);
                    let rad = (10.0 * s) as i32;
                    let _ = RoundRect(hdc, pad, edit_y, rc.right - pad, edit_y + edit_h, rad, rad);
                    SelectObject(hdc, old_pen);
                    SelectObject(hdc, old_br);
                    let _ = DeleteObject(pen);
                    // hint
                    SelectObject(hdc, st.font_hint);
                    SetTextColor(hdc, COLORREF(0x00A0A0A8));
                    let mut r = RECT { left: pad, top: edit_y + edit_h + (8.0 * s) as i32, right: rc.right - pad, bottom: rc.bottom - (8.0 * s) as i32 };
                    let mut t = st.hint.clone();
                    t.extend("\n↵ Enter to apply   ·   Esc to cancel".encode_utf16());
                    DrawTextW(hdc, &mut t, &mut r, DT_WORDBREAK);
                }
            });
            let _ = EndPaint(hwnd, &ps);
            return LRESULT(0);
        }
        WM_CTLCOLOREDIT => {
            let hdc = HDC(wp.0 as _);
            SetTextColor(hdc, COLORREF(0x00FFFFFF));
            SetBkColor(hdc, FIELD);
            let br = ST.with(|st| st.borrow().as_ref().map(|s| s.field_brush.0 as isize).unwrap_or(0));
            return LRESULT(br);
        }
        WM_ACTIVATE => {
            let state = (wp.0 & 0xFFFF) as u32;
            if state == WA_INACTIVE {
                if ACTIVATED.with(|a| a.get()) {
                    CANCEL.with(|c| c.set(true));
                }
            } else {
                ACTIVATED.with(|a| a.set(true));
                if let Some(e) = ST.with(|st| st.borrow().as_ref().map(|s| s.edit.0 as isize)) {
                    let _ = SetFocus(HWND(e as _));
                }
            }
            return LRESULT(0);
        }
        WM_NCHITTEST => {
            // drag anywhere outside the field
            let r = DefWindowProcW(hwnd, msg, wp, lp);
            if r.0 == HTCLIENT as isize {
                return LRESULT(HTCAPTION as isize);
            }
            return r;
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}
