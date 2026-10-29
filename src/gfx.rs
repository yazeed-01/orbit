//! Direct2D / DirectWrite rendering onto per-pixel-alpha layered windows.

use std::cell::RefCell;
use std::collections::HashMap;
use std::f32::consts::PI;

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub type Color = D2D1_COLOR_F;

pub const fn rgba(r: u8, g: u8, b: u8, a: f32) -> Color {
    Color { r: r as f32 / 255.0, g: g as f32 / 255.0, b: b as f32 / 255.0, a }
}

pub fn hex(c: u32, a: f32) -> Color {
    rgba((c >> 16) as u8, (c >> 8) as u8, c as u8, a)
}

pub fn mix(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    Color { r: a.r + (b.r - a.r) * t, g: a.g + (b.g - a.g) * t, b: a.b + (b.b - a.b) * t, a: a.a + (b.a - a.a) * t }
}

pub fn fade(c: Color, k: f32) -> Color {
    Color { a: c.a * k.clamp(0.0, 1.0), ..c }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Align {
    Left,
    Center,
    Right,
}

struct Gfx {
    d2d: ID2D1Factory,
    dw: IDWriteFactory,
    round: ID2D1StrokeStyle,
    icon_font: Vec<u16>,
    formats: RefCell<HashMap<(u32, u16, Align, bool, bool), IDWriteTextFormat>>,
}

thread_local! {
    static G: RefCell<Option<std::rc::Rc<Gfx>>> = const { RefCell::new(None) };
}

fn g() -> std::rc::Rc<Gfx> {
    G.with(|g| g.borrow().as_ref().expect("gfx not initialised").clone())
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn init() -> Result<()> {
    unsafe {
        let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
        let round = d2d.CreateStrokeStyle(
            &D2D1_STROKE_STYLE_PROPERTIES {
                startCap: D2D1_CAP_STYLE_ROUND,
                endCap: D2D1_CAP_STYLE_ROUND,
                dashCap: D2D1_CAP_STYLE_ROUND,
                lineJoin: D2D1_LINE_JOIN_ROUND,
                miterLimit: 10.0,
                dashStyle: D2D1_DASH_STYLE_SOLID,
                dashOffset: 0.0,
            },
            None,
        )?;
        // Prefer the Windows 11 icon font, fall back to the Windows 10 one.
        let mut icon_font = "Segoe MDL2 Assets";
        let mut coll = None;
        if dw.GetSystemFontCollection(&mut coll, false).is_ok() {
            if let Some(coll) = coll {
                let mut idx = 0u32;
                let mut exists = BOOL(0);
                let name = wide("Segoe Fluent Icons");
                if coll.FindFamilyName(PCWSTR(name.as_ptr()), &mut idx, &mut exists).is_ok() && exists.as_bool() {
                    icon_font = "Segoe Fluent Icons";
                }
            }
        }
        let gfx = Gfx { d2d, dw, round, icon_font: wide(icon_font), formats: RefCell::new(HashMap::new()) };
        G.with(|g| *g.borrow_mut() = Some(std::rc::Rc::new(gfx)));
    }
    Ok(())
}

fn format(size: f32, weight: u16, align: Align, wrap: bool, icon: bool) -> IDWriteTextFormat {
    let gx = g();
    let key = ((size * 100.0) as u32, weight, align, wrap, icon);
    if let Some(f) = gx.formats.borrow().get(&key) {
        return f.clone();
    }
    unsafe {
        let family = if icon { gx.icon_font.clone() } else { wide("Segoe UI Variable Text") };
        let f = gx
            .dw
            .CreateTextFormat(
                PCWSTR(family.as_ptr()),
                None,
                DWRITE_FONT_WEIGHT(weight as i32),
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                w!("en-us"),
            )
            .expect("text format");
        let _ = f.SetTextAlignment(match align {
            Align::Left => DWRITE_TEXT_ALIGNMENT_LEADING,
            Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
            Align::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
        });
        let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
        if !wrap {
            let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
            if let Ok(sign) = gx.dw.CreateEllipsisTrimmingSign(&f) {
                let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
                let _ = f.SetTrimming(&trim, &sign);
            }
        }
        gx.formats.borrow_mut().insert(key, f.clone());
        f
    }
}

/// Measure text; returns (width, height) in DIPs.
pub fn measure(s: &str, size: f32, weight: u16, max_w: f32, wrap: bool) -> (f32, f32) {
    let gx = g();
    let f = format(size, weight, Align::Left, wrap, false);
    let w: Vec<u16> = s.encode_utf16().collect();
    unsafe {
        if let Ok(layout) = gx.dw.CreateTextLayout(&w, &f, max_w, 10000.0) {
            let mut m = DWRITE_TEXT_METRICS::default();
            if layout.GetMetrics(&mut m).is_ok() {
                return (m.widthIncludingTrailingWhitespace, m.height);
            }
        }
    }
    (s.chars().count() as f32 * size * 0.55, size * 1.4)
}

/// An offscreen 32-bit premultiplied surface bound to a GDI memory DC, pushed to a
/// layered window with `UpdateLayeredWindow`.
pub struct Surface {
    pub w: i32,
    pub h: i32,
    memdc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    rt: ID2D1DCRenderTarget,
    brush: ID2D1SolidColorBrush,
    bits: *mut u8,
}

impl Surface {
    pub fn new(w: i32, h: i32) -> Result<Self> {
        unsafe {
            let memdc = CreateCompatibleDC(None);
            let bi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bmp = CreateDIBSection(memdc, &bi, DIB_RGB_COLORS, &mut bits, None, 0)?;
            let old = SelectObject(memdc, bmp);
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let rt = g().d2d.CreateDCRenderTarget(&props)?;
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            let brush = rt.CreateSolidColorBrush(&rgba(255, 255, 255, 1.0), None)?;
            Ok(Self { w, h, memdc, bmp, old, rt, brush, bits: bits as *mut u8 })
        }
    }

    /// Render a frame. `scale` is the DPI scale; drawing code works in DIPs.
    pub fn draw(&mut self, scale: f32, f: impl FnOnce(&Canvas)) {
        unsafe {
            let rc = RECT { left: 0, top: 0, right: self.w, bottom: self.h };
            if self.rt.BindDC(self.memdc, &rc).is_err() {
                return;
            }
            self.rt.BeginDraw();
            self.rt.Clear(Some(&rgba(0, 0, 0, 0.0)));
            let canvas = Canvas { rt: &self.rt, brush: &self.brush, s: scale };
            f(&canvas);
            let _ = self.rt.EndDraw(None, None);
        }
    }

    /// Write the current frame to a PNG on a dark backdrop (README screenshots).
    pub fn save_png(&self, path: &std::path::Path) -> std::result::Result<(), String> {
        let n = (self.w * self.h * 4) as usize;
        let src = unsafe { std::slice::from_raw_parts(self.bits, n) };
        let mut rgba = vec![0u8; n];
        // composite onto a dark "desktop" so shadows read well on light and dark pages
        let bg = [17u32, 17, 23];
        for (o, p) in rgba.chunks_mut(4).zip(src.chunks(4)) {
            let ia = 255 - p[3] as u32;
            let mix = |c: u8, b: u32| ((c as u32 * 255 + b * ia) / 255).min(255) as u8;
            o.copy_from_slice(&[mix(p[2], bg[0]), mix(p[1], bg[1]), mix(p[0], bg[2]), 255]);
        }
        crate::util::write_png(path, &rgba, self.w as u32, self.h as u32)
    }

    /// Push to screen. `pos` = new top-left in pixels, or None to keep the current position.
    pub fn present(&self, hwnd: HWND, pos: Option<(i32, i32)>, alpha: f32) {
        unsafe {
            let size = SIZE { cx: self.w, cy: self.h };
            let src = POINT { x: 0, y: 0 };
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: (alpha.clamp(0.0, 1.0) * 255.0) as u8,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let dst = pos.map(|(x, y)| POINT { x, y });
            let screen = GetDC(None);
            let _ = UpdateLayeredWindow(
                hwnd,
                screen,
                dst.as_ref().map(|p| p as *const POINT),
                Some(&size),
                self.memdc,
                Some(&src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
            ReleaseDC(None, screen);
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.memdc, self.old);
            let _ = DeleteObject(self.bmp);
            let _ = DeleteDC(self.memdc);
        }
    }
}

/// Drawing API in DIPs (multiplied by `s` internally).
pub struct Canvas<'a> {
    rt: &'a ID2D1DCRenderTarget,
    brush: &'a ID2D1SolidColorBrush,
    pub s: f32,
}

fn pt(x: f32, y: f32) -> D2D_POINT_2F {
    D2D_POINT_2F { x, y }
}

impl Canvas<'_> {
    fn paint(&self, c: Color) -> &ID2D1SolidColorBrush {
        unsafe { self.brush.SetColor(&c) };
        self.brush
    }

    pub fn fill_circle(&self, cx: f32, cy: f32, r: f32, c: Color) {
        let s = self.s;
        let e = D2D1_ELLIPSE { point: pt(cx * s, cy * s), radiusX: r * s, radiusY: r * s };
        unsafe { self.rt.FillEllipse(&e, self.paint(c)) };
    }

    pub fn stroke_circle(&self, cx: f32, cy: f32, r: f32, w: f32, c: Color) {
        let s = self.s;
        let e = D2D1_ELLIPSE { point: pt(cx * s, cy * s), radiusX: r * s, radiusY: r * s };
        unsafe { self.rt.DrawEllipse(&e, self.paint(c), w * s, None) };
    }

    pub fn fill_rrect(&self, x: f32, y: f32, w: f32, h: f32, r: f32, c: Color) {
        let s = self.s;
        let rr = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F { left: x * s, top: y * s, right: (x + w) * s, bottom: (y + h) * s },
            radiusX: r * s,
            radiusY: r * s,
        };
        unsafe { self.rt.FillRoundedRectangle(&rr, self.paint(c)) };
    }

    pub fn stroke_rrect(&self, x: f32, y: f32, w: f32, h: f32, r: f32, lw: f32, c: Color) {
        let s = self.s;
        let o = lw * 0.5;
        let rr = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F { left: (x + o) * s, top: (y + o) * s, right: (x + w - o) * s, bottom: (y + h - o) * s },
            radiusX: r * s,
            radiusY: r * s,
        };
        unsafe { self.rt.DrawRoundedRectangle(&rr, self.paint(c), lw * s, None) };
    }

    /// Fill rectangle given in raw pixels (used for crisp QR modules).
    pub fn fill_rect_px(&self, x: f32, y: f32, w: f32, h: f32, c: Color) {
        let r = D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h };
        unsafe { self.rt.FillRectangle(&r, self.paint(c)) };
    }

    pub fn line(&self, x1: f32, y1: f32, x2: f32, y2: f32, w: f32, c: Color) {
        let s = self.s;
        unsafe { self.rt.DrawLine(pt(x1 * s, y1 * s), pt(x2 * s, y2 * s), self.paint(c), w * s, &g().round) };
    }

    /// Polyline with round caps (used for animated check marks).
    pub fn polyline(&self, pts: &[(f32, f32)], w: f32, c: Color) {
        if pts.len() < 2 {
            return;
        }
        let s = self.s;
        unsafe {
            let Ok(geom) = g().d2d.CreatePathGeometry() else { return };
            let Ok(sink) = geom.Open() else { return };
            sink.BeginFigure(pt(pts[0].0 * s, pts[0].1 * s), D2D1_FIGURE_BEGIN_HOLLOW);
            for p in &pts[1..] {
                sink.AddLine(pt(p.0 * s, p.1 * s));
            }
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            let _ = sink.Close();
            self.rt.DrawGeometry(&geom, self.paint(c), w * s, &g().round);
        }
    }

    /// Stroke an arc from angle a0 to a1 (degrees, 0 = 3 o'clock, clockwise).
    pub fn arc(&self, cx: f32, cy: f32, r: f32, a0: f32, a1: f32, w: f32, c: Color) {
        if (a1 - a0).abs() < 0.01 {
            return;
        }
        let s = self.s;
        unsafe {
            let Ok(geom) = g().d2d.CreatePathGeometry() else { return };
            let Ok(sink) = geom.Open() else { return };
            let p = |a: f32| {
                let r_ = a * PI / 180.0;
                pt((cx + r * r_.cos()) * s, (cy + r * r_.sin()) * s)
            };
            sink.BeginFigure(p(a0), D2D1_FIGURE_BEGIN_HOLLOW);
            // split into <=180 degree segments
            let steps = ((a1 - a0).abs() / 170.0).ceil().max(1.0) as i32;
            for i in 1..=steps {
                let a = a0 + (a1 - a0) * i as f32 / steps as f32;
                sink.AddArc(&D2D1_ARC_SEGMENT {
                    point: p(a),
                    size: D2D_SIZE_F { width: r * s, height: r * s },
                    rotationAngle: 0.0,
                    sweepDirection: if a1 > a0 { D2D1_SWEEP_DIRECTION_CLOCKWISE } else { D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE },
                    arcSize: D2D1_ARC_SIZE_SMALL,
                });
            }
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            let _ = sink.Close();
            self.rt.DrawGeometry(&geom, self.paint(c), w * s, &g().round);
        }
    }

    /// Fill an annular wedge between radii r0..r1 and angles a0..a1 (degrees).
    pub fn wedge(&self, cx: f32, cy: f32, r0: f32, r1: f32, a0: f32, a1: f32, gap: f32, c: Color) {
        let s = self.s;
        // constant-width gap: convert to angle per radius
        let g0 = (gap / r0.max(1.0)).to_degrees() * 0.5;
        let g1 = (gap / r1.max(1.0)).to_degrees() * 0.5;
        let (o0, o1) = (a0 + g1, a1 - g1);
        let (i0, i1) = (a0 + g0, a1 - g0);
        if o1 <= o0 || i1 <= i0 {
            return;
        }
        unsafe {
            let Ok(geom) = g().d2d.CreatePathGeometry() else { return };
            let Ok(sink) = geom.Open() else { return };
            let p = |r: f32, a: f32| {
                let r_ = a * PI / 180.0;
                pt((cx + r * r_.cos()) * s, (cy + r * r_.sin()) * s)
            };
            let arc = |sink: &ID2D1GeometrySink, r: f32, from: f32, to: f32| {
                let steps = ((to - from).abs() / 170.0).ceil().max(1.0) as i32;
                for i in 1..=steps {
                    let a = from + (to - from) * i as f32 / steps as f32;
                    sink.AddArc(&D2D1_ARC_SEGMENT {
                        point: p(r, a),
                        size: D2D_SIZE_F { width: r * s, height: r * s },
                        rotationAngle: 0.0,
                        sweepDirection: if to > from { D2D1_SWEEP_DIRECTION_CLOCKWISE } else { D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE },
                        arcSize: D2D1_ARC_SIZE_SMALL,
                    });
                }
            };
            sink.BeginFigure(p(r1, o0), D2D1_FIGURE_BEGIN_FILLED);
            arc(&sink, r1, o0, o1);
            sink.AddLine(p(r0, i1));
            arc(&sink, r0, i1, i0);
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            let _ = sink.Close();
            self.rt.FillGeometry(&geom, self.paint(c), None);
        }
    }

    pub fn text(&self, t: &str, x: f32, y: f32, w: f32, h: f32, size: f32, weight: u16, c: Color, align: Align) {
        self.text_ex(t, x, y, w, h, size, weight, c, align, false)
    }

    pub fn text_wrap(&self, t: &str, x: f32, y: f32, w: f32, h: f32, size: f32, weight: u16, c: Color, align: Align) {
        self.text_ex(t, x, y, w, h, size, weight, c, align, true)
    }

    fn text_ex(&self, t: &str, x: f32, y: f32, w: f32, h: f32, size: f32, weight: u16, c: Color, align: Align, wrap: bool) {
        if t.is_empty() {
            return;
        }
        let s = self.s;
        let f = format(size * s, weight, align, wrap, false);
        let ws: Vec<u16> = t.encode_utf16().collect();
        let r = D2D_RECT_F { left: x * s, top: y * s, right: (x + w) * s, bottom: (y + h) * s };
        unsafe {
            self.rt.DrawText(&ws, &f, &r, self.paint(c), D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
        }
    }

    pub fn icon(&self, glyph: char, cx: f32, cy: f32, size: f32, c: Color) {
        let s = self.s;
        let f = format(size * s, 400, Align::Center, false, true);
        let mut buf = [0u16; 2];
        let ws = glyph.encode_utf16(&mut buf);
        let half = size * 0.9;
        let r = D2D_RECT_F { left: (cx - half) * s, top: (cy - half) * s, right: (cx + half) * s, bottom: (cy + half) * s };
        unsafe {
            self.rt.DrawText(ws, &f, &r, self.paint(c), D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
        }
    }

    /// Soft drop shadow made of stacked translucent rounded rects.
    pub fn shadow_rrect(&self, x: f32, y: f32, w: f32, h: f32, r: f32, spread: f32, strength: f32) {
        let n = 10;
        for i in 0..n {
            let k = i as f32 / n as f32;
            let e = spread * (1.0 - k);
            self.fill_rrect(x - e, y - e + spread * 0.35, w + 2.0 * e, h + 2.0 * e, r + e, rgba(0, 0, 0, strength / n as f32));
        }
    }

    pub fn shadow_circle(&self, cx: f32, cy: f32, r: f32, spread: f32, strength: f32) {
        let n = 10;
        for i in 0..n {
            let k = i as f32 / n as f32;
            self.fill_circle(cx, cy + spread * 0.3, r + spread * (1.0 - k), rgba(0, 0, 0, strength / n as f32));
        }
    }
}

/// Render an HICON (used for the tray) by drawing into a surface.
pub fn make_icon(size: i32, f: impl FnOnce(&Canvas)) -> Option<HICON> {
    unsafe {
        let mut surf = Surface::new(size, size).ok()?;
        surf.draw(1.0, f);
        let mask = CreateBitmap(size, size, 1, 1, None);
        let info = ICONINFO { fIcon: TRUE, xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: surf.bmp };
        // the color bitmap must not be selected into a DC
        SelectObject(surf.memdc, surf.old);
        let icon = CreateIconIndirect(&info).ok();
        SelectObject(surf.memdc, surf.bmp);
        let _ = DeleteObject(mask);
        icon
    }
}
