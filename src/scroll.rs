//! Scrolling capture (Alt+Shift+5): after the area is dragged in the overlay, the area is
//! re-grabbed every TICK_MS while the user scrolls the window underneath (or Space turns on
//! auto-scroll). Each new frame is matched row-by-row against the previous one to find how far
//! it scrolled, and only the newly revealed rows are appended. Rows that stay put while the
//! content moves (sticky headers / footers) are detected on the first scroll and kept once.
//!
//! Enter (or Alt+Shift+5 again) finishes, Esc cancels. Auto-scroll finishes by itself when the
//! content stops moving (end of the page).

use crate::capture::{Img, Surface};
use crate::clip::wide;
use std::{
    cell::RefCell,
    mem::zeroed,
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::LibraryLoader::GetModuleHandleW,
    UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

pub const FRAME_CLASS: &str = "ShotClipScrollFrame";
pub const TIMER_ID: usize = 40;
pub const HK_DONE: i32 = 20;
pub const HK_CANCEL: i32 = 21;
pub const HK_AUTO: i32 = 22;
const TICK_MS: u32 = 120;
const MAX_HEIGHT: usize = 40_000;
/// Ticks without any change before auto-scroll decides the page has ended (~1.2 s).
const AUTO_IDLE_END: u32 = 10;
const KEY: u32 = 0x00FF00FF; // colour-keyed (transparent) area of the frame window
const BORDER: i32 = 3;

struct Session {
    main: HWND,
    frame: HWND,
    rect: RECT,
    w: i32,
    h: i32,
    st: Stitch,
    auto: bool,
    idle: u32,
    font: HFONT,
    dpi: i32,
    bar_below: bool,
}

thread_local! {
    static S: RefCell<Option<Session>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Session) -> R) -> Option<R> {
    S.with(|s| s.try_borrow_mut().ok()?.as_mut().map(f))
}

pub fn active() -> bool {
    with(|_| ()).is_some()
}

fn row_bytes(w: i32) -> usize {
    (w * 4) as usize
}

fn row_hashes(px: &[u8], w: i32) -> Vec<u64> {
    px.chunks_exact(row_bytes(w))
        .map(|row| {
            // FNV-1a over B, G, R (alpha is undefined in screen grabs)
            let mut h: u64 = 0xcbf29ce484222325;
            for p in row.chunks_exact(4) {
                for &b in &p[..3] {
                    h = (h ^ b as u64).wrapping_mul(0x100000001b3);
                }
            }
            h
        })
        .collect()
}

fn uniform_rows(px: &[u8], w: i32) -> Vec<bool> {
    px.chunks_exact(row_bytes(w)).map(|row| row.chunks_exact(4).all(|p| p[..3] == row[..3])).collect()
}

/// Rows the new frame `b` moved up relative to `a` within the content band [t, h - bt).
/// Uniform rows (blank lines) carry no position information and are skipped.
fn find_dy(a: &[u64], a_uniform: &[bool], b: &[u64], t: usize, bt: usize) -> Option<usize> {
    let h = a.len();
    let ch = h.checked_sub(t + bt)?;
    if ch < 40 {
        return None;
    }
    let min_overlap = (ch / 5).max(20);
    let mut best: Option<(usize, f32)> = None;
    for dy in 1..=ch - min_overlap {
        let (mut cmp, mut ok) = (0usize, 0usize);
        for i in 0..ch - dy {
            let ai = t + i + dy;
            if a_uniform[ai] {
                continue;
            }
            cmp += 1;
            if b[t + i] == a[ai] {
                ok += 1;
            }
        }
        if cmp >= 8 && ok * 100 >= cmp * 90 {
            let score = ok as f32 / cmp as f32;
            if best.is_none_or(|(_, s)| score > s) {
                best = Some((dy, score));
            }
        }
    }
    best.map(|(dy, _)| dy)
}

/// Frame-by-frame stitching state (BGRA rows of width `w`), independent of any window.
struct Stitch {
    w: i32,
    h: usize,
    first: Vec<u8>,
    last: Vec<u8>,
    last_hash: Vec<u64>,
    /// (header rows, footer rows) that don't scroll; known after the first scroll.
    bands: Option<(usize, usize)>,
    content: Vec<u8>,
    /// The last frame couldn't be matched (scrolled too far in one tick).
    lost: bool,
}

#[derive(PartialEq, Debug)]
enum Feed {
    Same,
    Scrolled(usize),
    Absorbed,
    Lost,
}

impl Stitch {
    fn new(w: i32, first: Vec<u8>) -> Stitch {
        let h = first.len() / row_bytes(w);
        Stitch { w, h, last_hash: row_hashes(&first, w), last: first.clone(), first, bands: None, content: Vec::new(), lost: false }
    }

    fn height(&self) -> usize {
        match self.bands {
            Some((t, bt)) => t + self.content.len() / row_bytes(self.w) + bt,
            None => self.h,
        }
    }

    fn feed(&mut self, b: Vec<u8>) -> Feed {
        let hb = row_hashes(&b, self.w);
        if hb == self.last_hash {
            return Feed::Same;
        }
        let (h, rb) = (self.h, row_bytes(self.w));
        let (t, bt) = self.bands.unwrap_or_else(|| {
            let t = hb.iter().zip(&self.last_hash).take_while(|(x, y)| x == y).count();
            let bt = hb.iter().rev().zip(self.last_hash.iter().rev()).take_while(|(x, y)| x == y).count();
            (t.min(h / 3), bt.min(h / 3))
        });
        let a_uniform = uniform_rows(&self.last, self.w);
        match find_dy(&self.last_hash, &a_uniform, &hb, t, bt) {
            Some(dy) => {
                if self.bands.is_none() {
                    self.bands = Some((t, bt));
                    self.content = self.first[t * rb..(h - bt) * rb].to_vec();
                }
                self.content.extend_from_slice(&b[(h - bt - dy) * rb..(h - bt) * rb]);
                self.last = b;
                self.last_hash = hb;
                self.lost = false;
                Feed::Scrolled(dy)
            }
            // Mostly unchanged in place: something animated, not a scroll. Absorb it.
            None if hb.iter().zip(&self.last_hash).filter(|(x, y)| x == y).count() * 2 >= h => {
                self.last = b;
                self.last_hash = hb;
                Feed::Absorbed
            }
            // Moved too far (or upwards) to match: keep the old frame so scrolling back recovers.
            None => {
                self.lost = true;
                Feed::Lost
            }
        }
    }

    /// header (from the first frame) + content + footer (from the last frame).
    fn image(self) -> Vec<u8> {
        let rb = row_bytes(self.w);
        match self.bands {
            Some((t, bt)) => {
                let mut px = Vec::with_capacity(self.height() * rb);
                px.extend_from_slice(&self.first[..t * rb]);
                px.extend_from_slice(&self.content);
                px.extend_from_slice(&self.last[(self.h - bt) * rb..]);
                px
            }
            None => self.last,
        }
    }
}

fn grab(r: &RECT) -> Option<Vec<u8>> {
    let s = unsafe { Surface::grab(r.left, r.top, r.right - r.left, r.bottom - r.top)? };
    Some(s.px().to_vec())
}

/// Starts a session on `rect` (virtual-screen coordinates).
pub unsafe fn start(main: HWND, rect: RECT) {
    if active() {
        return;
    }
    let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
    if w < 8 || h < 60 {
        return;
    }
    let Some(first) = grab(&rect) else { return };
    let dpi = {
        let sdc = GetDC(null_mut());
        let d = GetDeviceCaps(sdc, LOGPIXELSY as _);
        ReleaseDC(null_mut(), sdc);
        d
    };
    let font = CreateFontW(-(14 * dpi / 96), 0, 0, 0, 600, 0, 0, 0, 1, 0, 0, 5, 0, wide("Segoe UI").as_ptr());

    // Frame window: border around the area + an info bar below (or above if no room).
    let bar_h = 30 * dpi / 96;
    let bar_w = (w + 2 * BORDER).max(620 * dpi / 96);
    let screen_bottom = GetSystemMetrics(SM_YVIRTUALSCREEN) + GetSystemMetrics(SM_CYVIRTUALSCREEN);
    let bar_below = rect.bottom + BORDER + bar_h <= screen_bottom;
    let (x, y) = (rect.left - BORDER, if bar_below { rect.top - BORDER } else { rect.top - BORDER - bar_h });
    let frame = CreateWindowExW(
        WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        wide(FRAME_CLASS).as_ptr(),
        wide("ShotClip scroll").as_ptr(),
        WS_POPUP,
        x,
        y,
        bar_w,
        h + 2 * BORDER + bar_h,
        null_mut(),
        null_mut(),
        GetModuleHandleW(null()),
        null(),
    );
    if frame.is_null() {
        DeleteObject(font);
        return;
    }
    SetLayeredWindowAttributes(frame, KEY, 0, LWA_COLORKEY);
    SetWindowDisplayAffinity(frame, WDA_EXCLUDEFROMCAPTURE); // never in our own grabs

    S.with(|s| {
        *s.borrow_mut() = Some(Session {
            main,
            frame,
            rect,
            w,
            h,
            st: Stitch::new(w, first),
            auto: false,
            idle: 0,
            font,
            dpi,
            bar_below,
        })
    });
    ShowWindow(frame, SW_SHOWNOACTIVATE);
    RegisterHotKey(main, HK_DONE, MOD_NOREPEAT, VK_RETURN as u32);
    RegisterHotKey(main, HK_CANCEL, MOD_NOREPEAT, VK_ESCAPE as u32);
    RegisterHotKey(main, HK_AUTO, MOD_NOREPEAT, VK_SPACE as u32);
    SetTimer(main, TIMER_ID, TICK_MS, None);
}

pub unsafe fn tick() {
    let Some((rect, auto)) = with(|s| (s.rect, s.auto)) else { return };
    let Some(b) = grab(&rect) else { return };
    let done = with(|s| match s.st.feed(b) {
        Feed::Same => {
            s.idle += 1;
            s.auto && s.idle >= AUTO_IDLE_END
        }
        _ => {
            s.idle = 0;
            s.st.height() >= MAX_HEIGHT
        }
    })
    .unwrap_or(false);
    if done {
        return finish(true);
    }
    if auto {
        mouse_event(MOUSEEVENTF_WHEEL, 0, 0, -120i32 as _, 0); // one notch down
    }
    if let Some(f) = with(|s| s.frame) {
        InvalidateRect(f, null(), 1);
    }
}

pub unsafe fn on_hotkey(id: i32) {
    match id {
        HK_DONE => finish(true),
        HK_CANCEL => finish(false),
        HK_AUTO => {
            let on = with(|s| {
                s.auto = !s.auto;
                s.idle = 0;
                (s.auto, s.rect)
            });
            if let Some((true, r)) = on {
                // The wheel goes to the window under the cursor.
                SetCursorPos((r.left + r.right) / 2, (r.top + r.bottom) / 2);
            }
            if let Some(f) = with(|s| s.frame) {
                InvalidateRect(f, null(), 1);
            }
        }
        _ => {}
    }
}

/// Ends the session; with `save`, hands the stitched image to the optimiser.
pub unsafe fn finish(save: bool) {
    let Some(s) = S.with(|s| s.borrow_mut().take()) else { return };
    KillTimer(s.main, TIMER_ID);
    for id in [HK_DONE, HK_CANCEL, HK_AUTO] {
        UnregisterHotKey(s.main, id);
    }
    DestroyWindow(s.frame);
    DeleteObject(s.font);
    if !save {
        return;
    }
    let rb = row_bytes(s.w);
    let px = s.st.image();
    let img = Img { w: s.w, h: (px.len() / rb) as i32, px, alpha: false };
    crate::on_capture(s.main, vec![img.into_rgba()], 0);
}

unsafe fn paint_frame(hwnd: HWND) {
    let mut ps: PAINTSTRUCT = zeroed();
    let dc = BeginPaint(hwnd, &mut ps);
    with(|s| {
        let mut cr: RECT = zeroed();
        GetClientRect(hwnd, &mut cr);
        let key = CreateSolidBrush(KEY);
        FillRect(dc, &cr, key);
        DeleteObject(key);

        let bar_h = 30 * s.dpi / 96;
        let top = if s.bar_below { 0 } else { bar_h };
        let (fw, fh) = (s.w + 2 * BORDER, s.h + 2 * BORDER);
        let colour = CreateSolidBrush(if s.st.lost { 0x003C3CE6 } else { 0x00E08A1E }); // red / blue
        for r in [
            RECT { left: 0, top, right: fw, bottom: top + BORDER },
            RECT { left: 0, top: top + fh - BORDER, right: fw, bottom: top + fh },
            RECT { left: 0, top, right: BORDER, bottom: top + fh },
            RECT { left: fw - BORDER, top, right: fw, bottom: top + fh },
        ] {
            FillRect(dc, &r, colour);
        }
        DeleteObject(colour);

        let bar = if s.bar_below {
            RECT { left: 0, top: fh, right: cr.right, bottom: fh + bar_h }
        } else {
            RECT { left: 0, top: 0, right: cr.right, bottom: bar_h }
        };
        let bg = CreateSolidBrush(0x00302820);
        FillRect(dc, &bar, bg);
        DeleteObject(bg);
        let text = if s.st.lost {
            "⚠ 너무 빨리 스크롤했어요. 조금 위로 되돌린 뒤 천천히 · Enter 완료 · Esc 취소".to_string()
        } else if s.auto {
            format!("자동 스크롤 중 · {}px · Space 멈춤 · Enter 완료 · Esc 취소", s.st.height())
        } else {
            format!("스크롤 캡처 · {}px · 휠로 스크롤 · Space 자동 · Enter 완료 · Esc 취소", s.st.height())
        };
        let t: Vec<u16> = text.encode_utf16().collect();
        let old = SelectObject(dc, s.font);
        SetBkMode(dc, TRANSPARENT as _);
        SetTextColor(dc, 0x00FFFFFF);
        let mut tr = RECT { left: bar.left + 10, ..bar };
        DrawTextW(dc, t.as_ptr(), t.len() as i32, &mut tr, DT_SINGLELINE | DT_VCENTER | DT_LEFT | DT_END_ELLIPSIS);
        SelectObject(dc, old);
    });
    EndPaint(hwnd, &ps);
}

pub unsafe extern "system" fn frame_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => paint_frame(hwnd),
        WM_ERASEBKGND => return 1,
        WM_MOUSEACTIVATE => return MA_NOACTIVATE as LRESULT,
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tall "page" of distinct rows with blank gaps, and a fixed header / footer.
    fn page(w: i32, rows: usize) -> Vec<u8> {
        let mut px = Vec::new();
        for y in 0..rows {
            for x in 0..w {
                let blank = y % 37 < 6;
                let v = if blank { 255 } else { ((x as usize * 7 + y * 13) % 251) as u8 };
                px.extend_from_slice(&[v, v.wrapping_add(y as u8), 255 - v, 255]);
            }
        }
        px
    }

    fn view(page: &[u8], w: i32, top: usize, h: usize, header: usize, footer: usize) -> Vec<u8> {
        let rb = row_bytes(w);
        let mut v = vec![0x40u8; header * rb]; // sticky header
        v.extend_from_slice(&page[(top + header) * rb..(top + h - footer) * rb]);
        v.extend(std::iter::repeat_n(0x80u8, footer * rb)); // sticky footer
        v
    }

    #[test]
    fn stitches_scrolled_frames_with_sticky_bands() {
        let (w, h, header, footer) = (64, 300, 40, 20);
        let full = page(w, 1500);
        let mut st = Stitch::new(w, view(&full, w, 0, h, header, footer));
        let mut top = 0;
        for step in [90, 0, 120, 45, 100, 100, 77] {
            top += step;
            let f = st.feed(view(&full, w, top, h, header, footer));
            if step == 0 {
                assert_eq!(f, Feed::Same);
            } else {
                assert_eq!(f, Feed::Scrolled(step), "step {step}");
            }
        }
        assert_eq!(st.bands, Some((header, footer)));
        let img = st.image();
        let rb = row_bytes(w);
        assert_eq!(img.len() / rb, top + h);
        // Content rows line up with the page.
        assert_eq!(&img[header * rb..(top + h - footer) * rb], &full[header * rb..(top + h - footer) * rb]);
    }

    #[test]
    fn too_far_is_lost_and_recovers() {
        let (w, h) = (64, 200);
        let full = page(w, 1000);
        let mut st = Stitch::new(w, view(&full, w, 0, h, 0, 0));
        assert_eq!(st.feed(view(&full, w, 400, h, 0, 0)), Feed::Lost);
        assert_eq!(st.feed(view(&full, w, 60, h, 0, 0)), Feed::Scrolled(60));
        assert_eq!(st.image().len() / row_bytes(w), 260);
    }
}
