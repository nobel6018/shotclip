//! macOS-style capture: Alt+Shift+3 (full screen, one image per monitor) and Alt+Shift+4
//! (see overlay.rs).
//! Captured images are handed to `crate::on_capture`, which runs them through the optimiser.

use crate::optimize::Rgba;
use std::{
    ffi::c_void,
    mem::{size_of, zeroed},
    path::{Path, PathBuf},
    ptr::{null, null_mut},
    slice,
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::{Com::CoTaskMemFree, SystemInformation::GetLocalTime},
    UI::{Input::KeyboardAndMouse::*, Shell::*, WindowsAndMessaging::*},
};

// ---------------------------------------------------------------- images

/// BGRA, top-down. The alpha channel only counts when `alpha` is set.
pub(crate) struct Img {
    pub w: i32,
    pub h: i32,
    pub px: Vec<u8>,
    pub alpha: bool,
}

impl Img {
    /// Mean absolute RGB difference over the square of `half` px around (x, y).
    pub fn diff(&self, other: &Img, x: i32, y: i32, half: i32) -> Option<u32> {
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
    pub fn round_corners(&mut self, r: i32) {
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
pub(crate) struct Surface {
    pub dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    pub bits: *mut u8,
    pub w: i32,
    pub h: i32,
}

impl Surface {
    pub fn px(&self) -> &[u8] {
        unsafe { slice::from_raw_parts(self.bits, (self.w * self.h * 4) as usize) }
    }

    /// (B, G, R) at a clamped position.
    pub fn pixel(&self, x: i32, y: i32) -> [u8; 3] {
        let (x, y) = (x.clamp(0, self.w - 1), y.clamp(0, self.h - 1));
        let p = &self.px()[((y * self.w + x) * 4) as usize..];
        [p[0], p[1], p[2]]
    }
}

impl Surface {
    pub unsafe fn new(w: i32, h: i32) -> Option<Surface> {
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
    pub unsafe fn grab(x: i32, y: i32, w: i32, h: i32) -> Option<Surface> {
        let s = Surface::new(w, h)?;
        let sdc = GetDC(null_mut());
        BitBlt(s.dc, 0, 0, w, h, sdc, x, y, SRCCOPY | CAPTUREBLT);
        ReleaseDC(null_mut(), sdc);
        GdiFlush();
        Some(s)
    }

    pub fn crop(&self, r: &RECT) -> Option<Img> {
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
    pub fn into_rgba(self) -> Rgba {
        let alpha = self.alpha;
        let px = self.px.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], if alpha { p[3] } else { 255 }]).collect();
        Rgba { w: self.w as u32, h: self.h as u32, px }
    }
}

// ---------------------------------------------------------------- output

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

pub(crate) unsafe fn virtual_screen() -> RECT {
    let (x, y) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
    RECT { left: x, top: y, right: x + GetSystemMetrics(SM_CXVIRTUALSCREEN), bottom: y + GetSystemMetrics(SM_CYVIRTUALSCREEN) }
}

pub(crate) fn contains(r: &RECT, x: i32, y: i32) -> bool {
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

