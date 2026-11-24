//! macOS-style capture: Alt+Shift+3 (full screen, one image per monitor) and Alt+Shift+4
//! (frozen screen; drag = rectangle, Space = window picking, Esc / right click = cancel).
//! Captured images are handed to `crate::on_capture`, which runs them through the optimiser.

use crate::clip::wide;
use crate::optimize::Rgba;
use std::{
    cell::RefCell,
    ffi::c_void,
    mem::{size_of, zeroed},
    path::{Path, PathBuf},
    ptr::{null, null_mut},
    slice,
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::{Dwm::*, Gdi::*},
    Storage::Xps::PrintWindow,
    System::{Com::CoTaskMemFree, LibraryLoader::GetModuleHandleW, SystemInformation::GetLocalTime},
    UI::{HiDpi::GetDpiForWindow, Input::KeyboardAndMouse::*, Shell::*, WindowsAndMessaging::*},
};

const PW_RENDERFULLCONTENT: u32 = 2;
pub const OVERLAY_CLASS: &str = "ShrinkClipCaptureOverlay";

// ---------------------------------------------------------------- imagesimages

/// BGRA, top-down. The alpha channel only counts when `alpha` is set.
struct Img {
    w: i32,
    h: i32,
    px: Vec<u8>,
    alpha: bool,
}

impl Img {
    /// Mean absolute RGB difference over the square of `half` px around (x, y).
    fn diff(&self, other: &Img, x: i32, y: i32, half: i32) -> Option<u32> {
        let (mut sum, mut n) = (0u64, 0u64);
        for yy in (y - half).max(0)..(y + half).min(self.h).min(other.h) {
            for xx in (x - half).max(0)..(x + half).min(self.w).min(other.w) {
                let a = &self.px[((yy * self.w + xx) * 4) as usize..][..3];
                let b = &other.px[((yy * other.w + xx) * 4) as usize..][..3];
                sum += a.iter().zip(b).map(|(p, q)| (*p as i32 - *q as i32).unsigned_abs() as u64).sum::<u64>();
                n += 3;
            }
        }
        (n > 0).then(|| (sum / n) as u32)
    }

    /// Makes the corners outside a rounded rect of radius `r` transparent (anti-aliased).
    fn round_corners(&mut self, r: i32) {
        if r < 1 || self.w < 2 * r || self.h < 2 * r {
            return;
        }
        self.alpha = true;
        for p in self.px.chunks_exact_mut(4) {
            p[3] = 255;
        }
        let rf = r as f32;
        for dy in 0..r {
            for dx in 0..r {
                let (cx, cy) = (rf - dx as f32 - 0.5, rf - dy as f32 - 0.5);
                let cover = (rf - (cx * cx + cy * cy).sqrt() + 0.5).clamp(0.0, 1.0);
                if cover >= 1.0 {
                    continue;
                }
                let a = (cover * 255.0) as u8;
                for (x, y) in [(dx, dy), (self.w - 1 - dx, dy), (dx, self.h - 1 - dy), (self.w - 1 - dx, self.h - 1 - dy)] {
                    self.px[((y * self.w + x) * 4 + 3) as usize] = a;
                }
            }
        }
    }
}

/// A 32-bit top-down DIB section selected into its own memory DC.
struct Surface {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    w: i32,
    h: i32,
}

impl Surface {
    unsafe fn new(w: i32, h: i32) -> Option<Surface> {
        let dc = CreateCompatibleDC(null_mut());
        if dc.is_null() {
            return None;
        }
        let mut bi: BITMAPINFO = zeroed();
        bi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        bi.bmiHeader.biWidth = w;
        bi.bmiHeader.biHeight = -h;
        bi.bmiHeader.biPlanes = 1;
        bi.bmiHeader.biBitCount = 32;
        bi.bmiHeader.biCompression = BI_RGB;
        let mut bits = null_mut();
        let bmp = CreateDIBSection(dc, &bi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
        if bmp.is_null() {
            DeleteDC(dc);
            return None;
        }
        let old = SelectObject(dc, bmp);
        Some(Surface { dc, bmp, old, bits: bits as *mut u8, w, h })
    }

    /// Copies the given screen area (virtual-screen coordinates) into a new surface.
    unsafe fn grab(x: i32, y: i32, w: i32, h: i32) -> Option<Surface> {
        let s = Surface::new(w, h)?;
        let sdc = GetDC(null_mut());
        BitBlt(s.dc, 0, 0, w, h, sdc, x, y, SRCCOPY | CAPTUREBLT);
        ReleaseDC(null_mut(), sdc);
        GdiFlush();
        Some(s)
    }

    fn crop(&self, r: &RECT) -> Option<Img> {
        let (l, t) = (r.left.max(0), r.top.max(0));
        let (rr, b) = (r.right.min(self.w), r.bottom.min(self.h));
        if rr - l < 1 || b - t < 1 {
            return None;
        }
        let src = unsafe { slice::from_raw_parts(self.bits, (self.w * self.h * 4) as usize) };
        let (w, h) = (rr - l, b - t);
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for y in t..b {
            let o = ((y * self.w + l) * 4) as usize;
            px.extend_from_slice(&src[o..o + (w * 4) as usize]);
        }
        Some(Img { w, h, px, alpha: false })
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            DeleteObject(self.bmp);
            DeleteDC(self.dc);
        }
    }
}


impl Img {
    fn into_rgba(self) -> Rgba {
        let alpha = self.alpha;
        let px = self.px.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], if alpha { p[3] } else { 255 }]).collect();
        Rgba { w: self.w as u32, h: self.h as u32, px }
    }
}

// ---------------------------------------------------------------- outputoutput

pub unsafe fn desktop_dir() -> PathBuf {
    let mut p: windows_sys::core::PWSTR = null_mut();
    if SHGetKnownFolderPath(&FOLDERID_Desktop, 0, null_mut(), &mut p) == 0 && !p.is_null() {
        let len = (0..).take_while(|&i| *p.add(i) != 0).count();
        let s = String::from_utf16_lossy(slice::from_raw_parts(p, len));
        CoTaskMemFree(p as *const c_void);
        return PathBuf::from(s);
    }
    PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default()).join("Desktop")
}

/// "스크린샷 2026-11-24 오후 3.22.56" (Korean macOS naming).
pub unsafe fn base_name() -> String {
    let mut t: SYSTEMTIME = zeroed();
    GetLocalTime(&mut t);
    let (ampm, h12) = match t.wHour {
        0 => ("오전", 12),
        h @ 1..=11 => ("오전", h),
        12 => ("오후", 12),
        h => ("오후", h - 12),
    };
    format!(
        "스크린샷 {:04}-{:02}-{:02} {} {}.{:02}.{:02}",
        t.wYear, t.wMonth, t.wDay, ampm, h12, t.wMinute, t.wSecond
    )
}

pub fn unique(dir: &Path, base: &str) -> PathBuf {
    let p = dir.join(format!("{base}.png"));
    if !p.exists() {
        return p;
    }
    (2..).map(|i| dir.join(format!("{base} ({i}).png"))).find(|p| !p.exists()).unwrap()
}

// ---------------------------------------------------------------- full screen

unsafe extern "system" fn monitor_cb(_: HMONITOR, _: HDC, r: *mut RECT, lp: LPARAM) -> BOOL {
    (*(lp as *mut Vec<RECT>)).push(*r);
    1
}

unsafe fn virtual_screen() -> RECT {
    let (x, y) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
    RECT { left: x, top: y, right: x + GetSystemMetrics(SM_CXVIRTUALSCREEN), bottom: y + GetSystemMetrics(SM_CYVIRTUALSCREEN) }
}

fn contains(r: &RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

pub unsafe fn capture_full(hwnd: HWND) {
    // The focused app saw Alt down/up with our key swallowed in between; a dummy key
    // keeps it from treating that as an Alt tap (menu bar activation).
    keybd_event(0xE8, 0, 0, 0);
    keybd_event(0xE8, 0, KEYEVENTF_KEYUP, 0);

    let vs = virtual_screen();
    let Some(s) = Surface::grab(vs.left, vs.top, vs.right - vs.left, vs.bottom - vs.top) else { return };
    let mut mons: Vec<RECT> = Vec::new();
    EnumDisplayMonitors(null_mut(), null(), Some(monitor_cb), &mut mons as *mut _ as LPARAM);
    // Primary first, then left-to-right.
    mons.sort_by_key(|r| (!contains(r, 0, 0), r.left, r.top));
    let mut cur: POINT = zeroed();
    GetCursorPos(&mut cur);
    let clip_idx = mons.iter().position(|r| contains(r, cur.x, cur.y)).unwrap_or(0);
    let imgs: Vec<Img> = mons
        .iter()
        .filter_map(|r| {
            let rel = RECT { left: r.left - vs.left, top: r.top - vs.top, right: r.right - vs.left, bottom: r.bottom - vs.top };
            s.crop(&rel)
        })
        .collect();
    drop(s);
    let clip_idx = clip_idx.min(imgs.len().saturating_sub(1));
    crate::on_capture(hwnd, imgs.into_iter().map(Img::into_rgba).collect(), clip_idx);
}

// ---------------------------------------------------------------- area / window overlay

#[derive(PartialEq, Clone, Copy)]
enum Mode {
    Area,
    Window,
}

struct Overlay {
    main: HWND,
    src: Surface,
    back: Surface,
    tint: Surface,
    font: HFONT,
    border: HBRUSH,
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
        }
    }
}

thread_local! {
    static OV: RefCell<Option<Overlay>> = const { RefCell::new(None) };
}

fn with_ov<R>(f: impl FnOnce(&mut Overlay) -> R) -> Option<R> {
    OV.with(|o| o.try_borrow_mut().ok()?.as_mut().map(f))
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

pub unsafe fn start_area(main: HWND) {
    if with_ov(|_| ()).is_some() {
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
            origin: POINT { x: vs.left, y: vs.top },
            mode: Mode::Area,
            start: None,
            cur: POINT { x: cur.x - vs.left, y: cur.y - vs.top },
            wins,
            hover: None,
        })
    });
    let hwnd = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
        wide(OVERLAY_CLASS).as_ptr(),
        wide("ShrinkClip capture").as_ptr(),
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

unsafe fn label(ov: &Overlay, text: &str) {
    let dc = ov.back.dc;
    let old = SelectObject(dc, ov.font);
    SetBkMode(dc, TRANSPARENT as _);
    let t: Vec<u16> = text.encode_utf16().collect();
    let mut sz: SIZE = zeroed();
    GetTextExtentPoint32W(dc, t.as_ptr(), t.len() as i32, &mut sz);
    let mut x = ov.cur.x + 14;
    let mut y = ov.cur.y + 14;
    if x + sz.cx + 4 > ov.back.w {
        x = ov.cur.x - 14 - sz.cx;
    }
    if y + sz.cy + 4 > ov.back.h {
        y = ov.cur.y - 14 - sz.cy;
    }
    SetTextColor(dc, 0x00000000);
    for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1), (1, 1)] {
        TextOutW(dc, x + dx, y + dy, t.as_ptr(), t.len() as i32);
    }
    SetTextColor(dc, 0x00FFFFFF);
    TextOutW(dc, x, y, t.as_ptr(), t.len() as i32);
    SelectObject(dc, old);
}

unsafe fn paint(hwnd: HWND) {
    let mut ps: PAINTSTRUCT = zeroed();
    let hdc = BeginPaint(hwnd, &mut ps);
    with_ov(|ov| {
        let (w, h) = (ov.src.w, ov.src.h);
        BitBlt(ov.back.dc, 0, 0, w, h, ov.src.dc, 0, 0, SRCCOPY);
        match ov.mode {
            Mode::Area => {
                if let Some(s) = ov.start {
                    let r = norm(s, ov.cur);
                    fill(ov, &r, [128, 128, 128], 70);
                    FrameRect(ov.back.dc, &r, ov.border);
                    label(ov, &format!("{}  {}", r.right - r.left, r.bottom - r.top));
                } else {
                    label(ov, &format!("{}  {}", ov.cur.x, ov.cur.y));
                }
            }
            Mode::Window => {
                if let Some(r) = ov.hover.map(|i| ov.wins[i].1) {
                    fill(ov, &r, [255, 150, 70], 80);
                }
            }
        }
        BitBlt(hdc, 0, 0, w, h, ov.back.dc, 0, 0, SRCCOPY);
    });
    EndPaint(hwnd, &ps);
}

fn update_hover(ov: &mut Overlay) {
    ov.hover = match ov.mode {
        Mode::Window => ov.wins.iter().position(|(_, r)| contains(r, ov.cur.x, ov.cur.y)),
        Mode::Area => None,
    };
}

/// Ends the overlay; with a rect, captures that part of the frozen screen.
enum Pick {
    Area(RECT),
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

/// Ends the overlay and captures the pick, if any.
unsafe fn finish(hwnd: HWND, pick: Option<Pick>) {
    let got = with_ov(|ov| {
        let o = ov.origin;
        let cur = ov.cur;
        match pick {
            None => None,
            Some(Pick::Area(r)) => ov.src.crop(&r).map(|i| (i, None)),
            Some(Pick::Window(w, r)) => ov.src.crop(&r).map(|seen| {
                let abs = RECT { left: r.left + o.x, top: r.top + o.y, right: r.right + o.x, bottom: r.bottom + o.y };
                (seen, Some((w, abs, POINT { x: cur.x - r.left, y: cur.y - r.top })))
            }),
        }
        .map(|g| (ov.main, g))
    })
    .flatten();
    DestroyWindow(hwnd); // WM_DESTROY drops the overlay state
    let Some((main, (seen, win))) = got else { return };
    let img = match win {
        None => seen,
        Some((w, frame, click)) => {
            let mut img = print_window(w, frame, &seen, click).unwrap_or(seen);
            if IsZoomed(w) == 0 {
                img.round_corners((8 * GetDpiForWindow(w) / 96) as i32);
            }
            img
        }
    };
    crate::on_capture(main, vec![img.into_rgba()], 0);
}

unsafe fn on_key(hwnd: HWND, vk: u32, lp: LPARAM) -> bool {
    match vk as u16 {
        VK_ESCAPE => finish(hwnd, None),
        VK_SPACE if lp & (1 << 30) == 0 => {
            with_ov(|ov| {
                if ov.start.is_none() {
                    ov.mode = if ov.mode == Mode::Area { Mode::Window } else { Mode::Area };
                    update_hover(ov);
                }
            });
            SetCursor(cursor());
            InvalidateRect(hwnd, null(), 0);
        }
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
            let pick = with_ov(|ov| {
                ov.cur = p;
                match ov.mode {
                    Mode::Area => {
                        ov.start = Some(p);
                        None
                    }
                    Mode::Window => Some(ov.hover.map(|i| Pick::Window(ov.wins[i].0, ov.wins[i].1))),
                }
            })
            .flatten();
            match pick {
                Some(Some(pick)) => finish(hwnd, Some(pick)),
                Some(None) => {} // window mode, nothing under the cursor
                None => {
                    SetCapture(hwnd);
                }
            }
        }
        WM_LBUTTONUP => {
            ReleaseCapture();
            let p = lp_point(lp);
            let r = with_ov(|ov| ov.start.map(|s| norm(s, p))).flatten();
            if let Some(r) = r {
                // A click without a drag cancels, like macOS.
                let real = r.right - r.left > 2 && r.bottom - r.top > 2;
                finish(hwnd, real.then_some(Pick::Area(r)));
            }
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

