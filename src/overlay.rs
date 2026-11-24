//! Full-screen overlay over a frozen screenshot, shared by every interactive tool:
//!
//! - Area (Alt+Shift+4): drag a rectangle; Space toggles Window (hover + click a window)
//! - Ocr (Alt+Shift+6): drag a rectangle, its text goes to the clipboard
//! - Scroll (Alt+Shift+5): drag the rectangle to stitch, then scroll (see scroll.rs)
//! - Color (Alt+Shift+7): magnifier; click / Enter copies the hex colour
//! - Ruler (Alt+Shift+8): edge-to-edge distances at the cursor, drag measures a box;
//!   click copies the size and the ruler stays open
//!
//! Esc / right click closes. Arrow keys nudge the cursor by one pixel (colour, ruler).

use crate::capture::{contains, virtual_screen, Img, Surface};
use crate::clip::wide;
use std::{
    cell::RefCell,
    ffi::c_void,
    mem::{size_of, zeroed},
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::{Dwm::*, Gdi::*},
    Storage::Xps::PrintWindow,
    System::LibraryLoader::GetModuleHandleW,
    UI::{HiDpi::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

pub const OVERLAY_CLASS: &str = "ShotClipCaptureOverlay";
const PW_RENDERFULLCONTENT: u32 = 2;
const MAG_N: i32 = 15; // magnified pixels per side (odd: the centre is the picked pixel)

#[derive(PartialEq, Clone, Copy)]
pub enum Mode {
    Area,
    Window,
    Ocr,
    Scroll,
    Color,
    Ruler,
}

impl Mode {
    fn drags(self) -> bool {
        matches!(self, Mode::Area | Mode::Ocr | Mode::Scroll | Mode::Ruler)
    }
    fn hint(self) -> Option<&'static str> {
        match self {
            Mode::Ocr => Some("텍스트 인식 · 드래그"),
            Mode::Scroll => Some("스크롤 캡처 · 스크롤할 영역을 드래그"),
            _ => None,
        }
    }
}

struct Overlay {
    main: HWND,
    src: Surface,
    back: Surface,
    tint: Surface,
    font: HFONT,
    border: HBRUSH,
    pen: HPEN,
    dpi: i32,
    mode: Mode,
    start: Option<POINT>,
    cur: POINT,
    /// Virtual-screen position of the overlay's (0, 0).
    origin: POINT,
    /// Top-level windows in Z order, with frames in overlay (client) coordinates.
    wins: Vec<(HWND, RECT)>,
    hover: Option<usize>,
}

impl Drop for Overlay {
    fn drop(&mut self) {
        unsafe {
            DeleteObject(self.font);
            DeleteObject(self.border);
            DeleteObject(self.pen);
        }
    }
}

thread_local! {
    static OV: RefCell<Option<Overlay>> = const { RefCell::new(None) };
}

fn with_ov<R>(f: impl FnOnce(&mut Overlay) -> R) -> Option<R> {
    OV.with(|o| o.try_borrow_mut().ok()?.as_mut().map(f))
}

pub fn is_open() -> bool {
    with_ov(|_| ()).is_some()
}

unsafe extern "system" fn window_cb(hwnd: HWND, lp: LPARAM) -> BOOL {
    let out = &mut *(lp as *mut Vec<(HWND, RECT)>);
    if IsWindowVisible(hwnd) == 0 || IsIconic(hwnd) != 0 {
        return 1;
    }
    let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
    if ex & WS_EX_TRANSPARENT != 0 {
        return 1; // click-through overlays
    }
    let mut cloaked = 0u32;
    DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED as _, &mut cloaked as *mut _ as *mut c_void, 4);
    if cloaked != 0 {
        return 1; // other virtual desktops, suspended UWP apps
    }
    let mut cls = [0u16; 64];
    let n = GetClassNameW(hwnd, cls.as_mut_ptr(), cls.len() as i32);
    let cls = String::from_utf16_lossy(&cls[..n.max(0) as usize]);
    if cls == "Progman" || cls == "WorkerW" {
        return 1; // the desktop
    }
    let shell = cls == "Shell_TrayWnd" || cls == "Shell_SecondaryTrayWnd";
    if !shell && GetWindowTextLengthW(hwnd) == 0 {
        return 1; // untitled helpers / invisible hosts
    }
    let mut r: RECT = zeroed();
    if DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS as _, &mut r as *mut _ as *mut c_void, size_of::<RECT>() as u32) != 0 {
        GetWindowRect(hwnd, &mut r);
    }
    if r.right - r.left > 4 && r.bottom - r.top > 4 {
        out.push((hwnd, r));
    }
    1
}

/// Freezes the screen and opens the overlay in `mode`.
pub unsafe fn start(main: HWND, mode: Mode) {
    if is_open() {
        return;
    }
    let vs = virtual_screen();
    let (w, h) = (vs.right - vs.left, vs.bottom - vs.top);
    let (Some(src), Some(back), Some(tint)) = (Surface::grab(vs.left, vs.top, w, h), Surface::new(w, h), Surface::new(1, 1)) else {
        return;
    };
    let mut wins: Vec<(HWND, RECT)> = Vec::new();
    EnumWindows(Some(window_cb), &mut wins as *mut _ as LPARAM);
    for (_, r) in &mut wins {
        *r = RECT { left: r.left - vs.left, top: r.top - vs.top, right: r.right - vs.left, bottom: r.bottom - vs.top };
    }
    let mut cur: POINT = zeroed();
    GetCursorPos(&mut cur);
    let dpi = {
        let sdc = GetDC(null_mut());
        let d = GetDeviceCaps(sdc, LOGPIXELSY as _);
        ReleaseDC(null_mut(), sdc);
        d
    };
    let font = CreateFontW(-(15 * dpi / 96), 0, 0, 0, 600, 0, 0, 0, 1, 0, 0, 5, 0, wide("Segoe UI").as_ptr());
    OV.with(|o| {
        *o.borrow_mut() = Some(Overlay {
            main,
            src,
            back,
            tint,
            font,
            border: CreateSolidBrush(0x00E0E0E0),
            pen: CreatePen(PS_SOLID, (dpi / 96).max(1), 0x00782DFF), // #FF2D78
            dpi,
            origin: POINT { x: vs.left, y: vs.top },
            mode,
            start: None,
            cur: POINT { x: cur.x - vs.left, y: cur.y - vs.top },
            wins,
            hover: None,
        })
    });
    let hwnd = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
        wide(OVERLAY_CLASS).as_ptr(),
        wide("ShotClip capture").as_ptr(),
        WS_POPUP,
        vs.left,
        vs.top,
        w,
        h,
        null_mut(),
        null_mut(),
        GetModuleHandleW(null()),
        null(),
    );
    if hwnd.is_null() {
        OV.with(|o| o.borrow_mut().take());
        return;
    }
    ShowWindow(hwnd, SW_SHOW);
    SetForegroundWindow(hwnd);
    SetFocus(hwnd);
}

fn norm(a: POINT, b: POINT) -> RECT {
    RECT { left: a.x.min(b.x), top: a.y.min(b.y), right: a.x.max(b.x), bottom: a.y.max(b.y) }
}

unsafe fn fill(ov: &Overlay, r: &RECT, bgr: [u8; 3], alpha: u8) {
    let p = ov.tint.bits;
    *p = bgr[0];
    *p.add(1) = bgr[1];
    *p.add(2) = bgr[2];
    *p.add(3) = 0;
    let bf = BLENDFUNCTION { BlendOp: 0, BlendFlags: 0, SourceConstantAlpha: alpha, AlphaFormat: 0 };
    AlphaBlend(ov.back.dc, r.left, r.top, r.right - r.left, r.bottom - r.top, ov.tint.dc, 0, 0, 1, 1, bf);
}

unsafe fn text_size(ov: &Overlay, t: &[u16]) -> SIZE {
    let old = SelectObject(ov.back.dc, ov.font);
    let mut sz: SIZE = zeroed();
    GetTextExtentPoint32W(ov.back.dc, t.as_ptr(), t.len() as i32, &mut sz);
    SelectObject(ov.back.dc, old);
    sz
}

/// Outlined white text with its top-left at (x, y).
unsafe fn text_at(ov: &Overlay, x: i32, y: i32, t: &[u16]) {
    let dc = ov.back.dc;
    let old = SelectObject(dc, ov.font);
    SetBkMode(dc, TRANSPARENT as _);
    SetTextColor(dc, 0x00000000);
    for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1), (1, 1)] {
        TextOutW(dc, x + dx, y + dy, t.as_ptr(), t.len() as i32);
    }
    SetTextColor(dc, 0x00FFFFFF);
    TextOutW(dc, x, y, t.as_ptr(), t.len() as i32);
    SelectObject(dc, old);
}

/// Text next to the cursor, flipped to stay on screen.
unsafe fn label(ov: &Overlay, text: &str) {
    let t: Vec<u16> = text.encode_utf16().collect();
    let sz = text_size(ov, &t);
    let mut x = ov.cur.x + 14;
    let mut y = ov.cur.y + 14;
    if x + sz.cx + 4 > ov.back.w {
        x = ov.cur.x - 14 - sz.cx;
    }
    if y + sz.cy + 4 > ov.back.h {
        y = ov.cur.y - 14 - sz.cy;
    }
    text_at(ov, x, y, &t);
}

/// Display scale (1.0 = 100 %) of the monitor under the cursor.
unsafe fn scale_at(ov: &Overlay) -> f32 {
    let p = POINT { x: ov.cur.x + ov.origin.x, y: ov.cur.y + ov.origin.y };
    let mon = MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST);
    let (mut dx, mut dy) = (96u32, 96u32);
    GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
    dx as f32 / 96.0
}

/// "320 × 48" plus the size at 100 % when the display is scaled.
unsafe fn dims(ov: &Overlay, w: i32, h: i32) -> String {
    let s = scale_at(ov);
    if (s - 1.0).abs() < 0.01 {
        format!("{w} × {h}")
    } else {
        format!("{w} × {h}  ({:.0} × {:.0} @{:.0}%)", w as f32 / s, h as f32 / s, s * 100.0)
    }
}

fn similar(a: [u8; 3], b: [u8; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| (*x as i32 - y as i32).abs() <= 10)
}

/// Inclusive extents of the same-coloured run through the cursor: (left, right, top, bottom).
fn spans(ov: &Overlay) -> (i32, i32, i32, i32) {
    let (s, c) = (&ov.src, ov.cur);
    let base = s.pixel(c.x, c.y);
    let (mut l, mut r, mut t, mut b) = (c.x, c.x, c.y, c.y);
    while l > 0 && similar(s.pixel(l - 1, c.y), base) {
        l -= 1;
    }
    while r < s.w - 1 && similar(s.pixel(r + 1, c.y), base) {
        r += 1;
    }
    while t > 0 && similar(s.pixel(c.x, t - 1), base) {
        t -= 1;
    }
    while b < s.h - 1 && similar(s.pixel(c.x, b + 1), base) {
        b += 1;
    }
    (l, r, t, b)
}

unsafe fn line(dc: HDC, x0: i32, y0: i32, x1: i32, y1: i32) {
    MoveToEx(dc, x0, y0, null_mut());
    LineTo(dc, x1, y1);
}

unsafe fn paint_ruler(ov: &Overlay) {
    let dc = ov.back.dc;
    let old = SelectObject(dc, ov.pen);
    let tick = 5 * ov.dpi / 96;
    if let Some(s) = ov.start {
        let r = norm(s, ov.cur);
        fill(ov, &r, [120, 45, 255], 40);
        let ob = SelectObject(dc, GetStockObject(NULL_BRUSH));
        Rectangle(dc, r.left, r.top, r.right + 1, r.bottom + 1);
        SelectObject(dc, ob);
        label(ov, &dims(ov, r.right - r.left, r.bottom - r.top));
    } else {
        let (l, r, t, b) = spans(ov);
        let c = ov.cur;
        line(dc, l, c.y, r + 1, c.y);
        line(dc, l, c.y - tick, l, c.y + tick + 1);
        line(dc, r, c.y - tick, r, c.y + tick + 1);
        line(dc, c.x, t, c.x, b + 1);
        line(dc, c.x - tick, t, c.x + tick + 1, t);
        line(dc, c.x - tick, b, c.x + tick + 1, b);
        label(ov, &dims(ov, r - l + 1, b - t + 1));
    }
    SelectObject(dc, old);
}

fn hex(p: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", p[2], p[1], p[0])
}

unsafe fn paint_color(ov: &Overlay) {
    let dc = ov.back.dc;
    let cell = (10 * ov.dpi / 96).max(8);
    let size = MAG_N * cell;
    let c = ov.cur;
    let gap = 24 * ov.dpi / 96;
    let text_h = 22 * ov.dpi / 96;
    let mut x = c.x + gap;
    let mut y = c.y + gap;
    if x + size > ov.back.w {
        x = c.x - gap - size;
    }
    if y + size + text_h * 2 > ov.back.h {
        y = c.y - gap - size - text_h * 2;
    }
    SetStretchBltMode(dc, COLORONCOLOR as _);
    StretchBlt(dc, x, y, size, size, ov.src.dc, c.x - MAG_N / 2, c.y - MAG_N / 2, MAG_N, MAG_N, SRCCOPY);
    let black = GetStockObject(BLACK_BRUSH) as HBRUSH;
    let white = GetStockObject(WHITE_BRUSH) as HBRUSH;
    FrameRect(dc, &RECT { left: x - 1, top: y - 1, right: x + size + 1, bottom: y + size + 1 }, black);
    let (cx, cy) = (x + (MAG_N / 2) * cell, y + (MAG_N / 2) * cell);
    FrameRect(dc, &RECT { left: cx - 1, top: cy - 1, right: cx + cell + 1, bottom: cy + cell + 1 }, black);
    FrameRect(dc, &RECT { left: cx, top: cy, right: cx + cell, bottom: cy + cell }, white);
    let p = ov.src.pixel(c.x, c.y);
    let t1: Vec<u16> = hex(p).encode_utf16().collect();
    let t2: Vec<u16> = format!("{}, {}, {}", p[2], p[1], p[0]).encode_utf16().collect();
    text_at(ov, x, y + size + 4, &t1);
    text_at(ov, x, y + size + 4 + text_h, &t2);
}

unsafe fn paint(hwnd: HWND) {
    let mut ps: PAINTSTRUCT = zeroed();
    let hdc = BeginPaint(hwnd, &mut ps);
    with_ov(|ov| {
        let (w, h) = (ov.src.w, ov.src.h);
        BitBlt(ov.back.dc, 0, 0, w, h, ov.src.dc, 0, 0, SRCCOPY);
        match ov.mode {
            Mode::Area | Mode::Ocr | Mode::Scroll => {
                if let Some(s) = ov.start {
                    let r = norm(s, ov.cur);
                    fill(ov, &r, [128, 128, 128], 70);
                    FrameRect(ov.back.dc, &r, ov.border);
                    label(ov, &format!("{}  {}", r.right - r.left, r.bottom - r.top));
                } else if let Some(h) = ov.mode.hint() {
                    label(ov, h);
                } else {
                    label(ov, &format!("{}  {}", ov.cur.x, ov.cur.y));
                }
            }
            Mode::Window => {
                if let Some(r) = ov.hover.map(|i| ov.wins[i].1) {
                    fill(ov, &r, [255, 150, 70], 80);
                }
            }
            Mode::Ruler => paint_ruler(ov),
            Mode::Color => paint_color(ov),
        }
        BitBlt(hdc, 0, 0, w, h, ov.back.dc, 0, 0, SRCCOPY);
    });
    EndPaint(hwnd, &ps);
}

fn update_hover(ov: &mut Overlay) {
    ov.hover = match ov.mode {
        Mode::Window => ov.wins.iter().position(|(_, r)| contains(r, ov.cur.x, ov.cur.y)),
        _ => None,
    };
}

enum Pick {
    /// A dragged rectangle (overlay coordinates); what it means depends on the mode.
    Rect(RECT),
    /// Window handle + its visible frame, in overlay coordinates.
    Window(HWND, RECT),
}

/// Renders `target` on its own (occluded parts included, like macOS) via PrintWindow and
/// crops it to `frame` (virtual-screen coords). `seen` is the frozen screen crop of the same
/// frame; the rendering is only trusted if it matches that around the clicked point, which
/// is always visible (catches black output and DPI-virtualised apps rendered at the wrong scale).
unsafe fn print_window(target: HWND, frame: RECT, seen: &Img, click: POINT) -> Option<Img> {
    let mut wr: RECT = zeroed();
    GetWindowRect(target, &mut wr);
    let s = Surface::new(wr.right - wr.left, wr.bottom - wr.top)?;
    if PrintWindow(target, s.dc, PW_RENDERFULLCONTENT) == 0 {
        return None;
    }
    GdiFlush();
    let img = s.crop(&RECT {
        left: frame.left - wr.left,
        top: frame.top - wr.top,
        right: frame.right - wr.left,
        bottom: frame.bottom - wr.top,
    })?;
    if (img.w, img.h) != (seen.w, seen.h) || img.diff(seen, click.x, click.y, 24)? > 12 {
        return None;
    }
    Some(img)
}

enum Outcome {
    Capture(Img, Option<(HWND, RECT, POINT)>),
    Ocr(Img, u32),
    Scroll(RECT),
}

/// Closes the overlay and acts on the pick, if any.
unsafe fn finish(hwnd: HWND, pick: Option<Pick>) {
    let got = with_ov(|ov| {
        let o = ov.origin;
        let cur = ov.cur;
        let abs = |r: RECT| RECT { left: r.left + o.x, top: r.top + o.y, right: r.right + o.x, bottom: r.bottom + o.y };
        let out = match (pick, ov.mode) {
            (None, _) => None,
            (Some(Pick::Rect(r)), Mode::Ocr) => ov.src.crop(&r).map(|i| Outcome::Ocr(i, (scale_at(ov) * 96.0) as u32)),
            (Some(Pick::Rect(r)), Mode::Scroll) => Some(Outcome::Scroll(abs(r))),
            (Some(Pick::Rect(r)), _) => ov.src.crop(&r).map(|i| Outcome::Capture(i, None)),
            (Some(Pick::Window(w, r)), _) => ov
                .src
                .crop(&r)
                .map(|seen| Outcome::Capture(seen, Some((w, abs(r), POINT { x: cur.x - r.left, y: cur.y - r.top })))),
        };
        out.map(|g| (ov.main, g))
    })
    .flatten();
    DestroyWindow(hwnd); // WM_DESTROY drops the overlay state
    let Some((main, out)) = got else { return };
    match out {
        Outcome::Capture(seen, None) => crate::on_capture(main, vec![seen.into_rgba()], 0),
        Outcome::Capture(seen, Some((w, frame, click))) => {
            let mut img = print_window(w, frame, &seen, click).unwrap_or(seen);
            if IsZoomed(w) == 0 {
                img.round_corners((8 * GetDpiForWindow(w) / 96) as i32);
            }
            crate::on_capture(main, vec![img.into_rgba()], 0);
        }
        Outcome::Ocr(img, dpi) => crate::on_ocr(main, img, dpi),
        Outcome::Scroll(r) => crate::scroll::start(main, r),
    }
}

/// Puts `text` on the clipboard (with a notification); `close` ends the overlay first.
unsafe fn copy_text(hwnd: HWND, title: &str, text: &str, close: bool) {
    let main = with_ov(|ov| ov.main);
    if close {
        DestroyWindow(hwnd);
    }
    if let Some(main) = main {
        crate::on_text(main, title, text);
    }
}

unsafe fn color_pick(hwnd: HWND) {
    if let Some(p) = with_ov(|ov| ov.src.pixel(ov.cur.x, ov.cur.y)) {
        let h = hex(p);
        copy_text(hwnd, &format!("색상 복사 · {h}"), &h, true);
    }
}

unsafe fn on_key(hwnd: HWND, vk: u32, lp: LPARAM) -> bool {
    let Some(mode) = with_ov(|ov| ov.mode) else { return false };
    match vk as u16 {
        VK_ESCAPE => finish(hwnd, None),
        VK_SPACE if lp & (1 << 30) == 0 && matches!(mode, Mode::Area | Mode::Window) => {
            with_ov(|ov| {
                if ov.start.is_none() {
                    ov.mode = if ov.mode == Mode::Area { Mode::Window } else { Mode::Area };
                    update_hover(ov);
                }
            });
            SetCursor(cursor());
            InvalidateRect(hwnd, null(), 0);
        }
        // Pixel-precise nudging for the picker and the ruler.
        k @ (VK_LEFT | VK_RIGHT | VK_UP | VK_DOWN) if matches!(mode, Mode::Color | Mode::Ruler) => {
            let mut p: POINT = zeroed();
            GetCursorPos(&mut p);
            match k {
                VK_LEFT => p.x -= 1,
                VK_RIGHT => p.x += 1,
                VK_UP => p.y -= 1,
                _ => p.y += 1,
            }
            SetCursorPos(p.x, p.y);
        }
        VK_RETURN if mode == Mode::Color => color_pick(hwnd),
        _ => return false,
    }
    true
}

unsafe fn cursor() -> HCURSOR {
    let window = with_ov(|ov| ov.mode == Mode::Window).unwrap_or(false);
    LoadCursorW(null_mut(), if window { IDC_HAND } else { IDC_CROSS })
}

fn lp_point(lp: LPARAM) -> POINT {
    POINT { x: (lp & 0xFFFF) as i16 as i32, y: ((lp >> 16) & 0xFFFF) as i16 as i32 }
}

unsafe fn on_lbutton_up(hwnd: HWND, p: POINT) {
    let Some((r, mode)) = with_ov(|ov| ov.start.take().map(|s| (norm(s, p), ov.mode))).flatten() else {
        return;
    };
    let real = r.right - r.left > 2 && r.bottom - r.top > 2;
    if mode != Mode::Ruler {
        // A click without a drag cancels, like macOS.
        return finish(hwnd, real.then_some(Pick::Rect(r)));
    }
    // Ruler: drag = box size, click = edge-to-edge spans at the cursor. It stays open.
    let text = if real {
        format!("{} × {}", r.right - r.left, r.bottom - r.top)
    } else {
        let (l, rr, t, b) = with_ov(|ov| spans(ov)).unwrap_or_default();
        format!("{} × {}", rr - l + 1, b - t + 1)
    };
    copy_text(hwnd, &format!("크기 복사 · {text}"), &text, false);
    InvalidateRect(hwnd, null(), 0);
}

pub unsafe extern "system" fn overlay_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => paint(hwnd),
        WM_ERASEBKGND => return 1,
        WM_SETCURSOR => {
            SetCursor(cursor());
            return 1;
        }
        WM_MOUSEMOVE => {
            with_ov(|ov| {
                ov.cur = lp_point(lp);
                update_hover(ov);
            });
            InvalidateRect(hwnd, null(), 0);
        }
        WM_LBUTTONDOWN => {
            let p = lp_point(lp);
            let Some(mode) = with_ov(|ov| {
                ov.cur = p;
                ov.mode
            }) else {
                return 0;
            };
            if mode.drags() {
                with_ov(|ov| ov.start = Some(p));
                SetCapture(hwnd);
            } else if mode == Mode::Color {
                color_pick(hwnd);
            } else if let Some(pick) = with_ov(|ov| ov.hover.map(|i| Pick::Window(ov.wins[i].0, ov.wins[i].1))).flatten() {
                finish(hwnd, Some(pick));
            }
        }
        WM_LBUTTONUP => {
            ReleaseCapture();
            on_lbutton_up(hwnd, lp_point(lp));
        }
        WM_RBUTTONDOWN => finish(hwnd, None),
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            if !on_key(hwnd, wp as u32, lp) && msg == WM_SYSKEYDOWN {
                return 0; // keep Alt from reaching DefWindowProc (menu mode)
            }
        }
        WM_SYSKEYUP | WM_SYSCHAR => return 0,
        WM_SYSCOMMAND if (wp & 0xFFF0) as u32 == SC_KEYMENU => return 0,
        WM_ACTIVATE if (wp & 0xFFFF) as u32 == WA_INACTIVE => {
            PostMessageW(hwnd, WM_CLOSE, 0, 0); // focus lost (Alt+Tab, Win key): cancel
        }
        WM_DESTROY => {
            OV.with(|o| o.borrow_mut().take());
        }
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    0
}
