//! Post-capture editor (opens after every capture, like Shottr on macOS).
//!
//! The capture is already optimised, copied and saved before this window opens; the editor only
//! re-exports when something was changed. Closing (Esc / Enter / Ctrl+C / ✕) exports the edited
//! image through the `export` callback (main.rs sends it through the same pipeline, overwriting
//! the same Desktop file) and closes. Ctrl+S saves a copy elsewhere.
//!
//! Keys (our own Windows choice): A arrow, R rectangle, O oval, L line, D pen, T text,
//! H highlighter, B blur, E erase, N counter, C crop; Ctrl+Z / Ctrl+Y undo/redo, Delete removes
//! the selection, arrow keys nudge it, Tab copies the colour under the cursor, + / - / 0 / 1 zoom.

use crate::annot::{self, Obj, Scale, Shape, Tool};
use crate::capture::Surface;
use crate::clip::wide;
use crate::optimize::Rgba;
use std::{
    cell::{Cell, RefCell},
    mem::{size_of, zeroed},
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    UI::{
        Controls::Dialogs::*,
        HiDpi::*,
        Input::KeyboardAndMouse::*,
        WindowsAndMessaging::*,
    },
};

pub const EDITOR_CLASS: &str = "ShotClipEditor";

type Export = Box<dyn FnMut(Rgba)>;

const TOOLS: [(Tool, char, &str); 11] = [
    (Tool::Arrow, 'A', "화살표"),
    (Tool::Rect, 'R', "사각형"),
    (Tool::Oval, 'O', "원"),
    (Tool::Line, 'L', "선"),
    (Tool::Pen, 'D', "펜"),
    (Tool::Text, 'T', "텍스트"),
    (Tool::Highlight, 'H', "형광펜"),
    (Tool::Blur, 'B', "흐림"),
    (Tool::Erase, 'E', "지우기"),
    (Tool::Counter, 'N', "번호"),
    (Tool::Crop, 'C', "자르기"),
];
const COLORS: [u32; 8] = [0xE5383B, 0xF77F00, 0xFFD60A, 0x2DC653, 0x1E88E5, 0x8E44AD, 0x111111, 0xFFFFFF];
const HL_DEFAULT: u32 = 0xFFE14D;

#[derive(Clone, Copy, PartialEq)]
enum Item {
    Tool(Tool),
    Color(u32),
    Size(u8),
    Undo,
    Save,
    Done,
}

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    None,
    Draw,
    Move { last: (f32, f32), moved: bool },
    Crop { start: (f32, f32) },
    Pan { last: (i32, i32) },
}

type Crop = Option<(f32, f32, f32, f32)>;

struct Ed {
    hwnd: HWND,
    standalone: bool,
    base: Rgba,
    scale: Scale,
    objs: Vec<Obj>,
    crop: Crop,
    undo: Vec<(Vec<Obj>, Crop)>,
    redo: Vec<(Vec<Obj>, Crop)>,
    /// Render of `objs` (RGBA); `work` = committed + live object; `disp` = `work` as a BGRA DIB.
    committed: Rgba,
    work: Rgba,
    disp: Surface,
    /// Region of `work` that currently differs from `committed`.
    live_box: Option<(f32, f32, f32, f32)>,
    live: Option<Obj>,
    /// Index the live object came from (moving / re-editing keeps its z-order).
    live_at: Option<usize>,
    live_crop: Crop,
    editing_text: bool,
    sel: Option<usize>,
    drag: Drag,
    tool: Tool,
    color: u32,
    hl_color: u32,
    size: u8,
    zoom: f32,
    ox: f32,
    oy: f32,
    fit: bool,
    hover: Option<(f32, f32)>,
    space: bool,
    edited: bool,
    status: String,
    layout: Vec<(RECT, Item)>,
    export: Export,
    title: String,
}

thread_local! {
    static ED: RefCell<Option<Ed>> = const { RefCell::new(None) };
    static OPEN: Cell<HWND> = const { Cell::new(null_mut()) };
    /// Tool / colour / size survive between editor windows.
    static PREFS: Cell<(Tool, u32, u8)> = const { Cell::new((Tool::Arrow, COLORS[0], 1)) };
}

pub fn is_open() -> bool {
    !OPEN.with(Cell::get).is_null()
}

/// Things to do after the state has been put back (they may re-enter the window procedure).
enum After {
    Nothing,
    Finish,
    SaveAs,
}

// ---------------------------------------------------------------- open / close

/// Opens the editor for `img`. An editor that is already open is finished first (exporting its
/// edits), so there is only ever one.
pub unsafe fn open(img: Rgba, title: &str, standalone: bool, export: Export) {
    let prev = OPEN.with(Cell::get);
    if !prev.is_null() {
        finish(prev);
    }
    let Some(disp) = Surface::new(img.w as i32, img.h as i32) else { return };
    let hinst = windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(null());

    // Size: image 1:1 if it fits the work area of the monitor under the cursor, else fit.
    let mut pt: POINT = zeroed();
    GetCursorPos(&mut pt);
    let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
    let mut mi: MONITORINFO = zeroed();
    mi.cbSize = size_of::<MONITORINFO>() as u32;
    GetMonitorInfoW(mon, &mut mi);
    let wa = mi.rcWork;
    let mut dpi_x = 96u32;
    let mut dpi_y = 96u32;
    GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
    let ui = dpi_x as f32 / 96.0;
    let (tb, sb) = ((44.0 * ui) as i32, (26.0 * ui) as i32);
    let (aw, ah) = (wa.right - wa.left, wa.bottom - wa.top);
    let min_w = (900.0 * ui) as i32;
    let mut r = RECT {
        left: 0,
        top: 0,
        right: (img.w as i32 + 40).max(min_w).min(aw * 92 / 100),
        bottom: (img.h as i32 + tb + sb + 40).max((360.0 * ui) as i32).min(ah * 92 / 100),
    };
    let style = WS_OVERLAPPEDWINDOW;
    AdjustWindowRectExForDpi(&mut r, style, 0, 0, dpi_x);
    let (w, h) = ((r.right - r.left).min(aw), (r.bottom - r.top).min(ah));
    let (x, y) = (wa.left + (aw - w) / 2, wa.top + (ah - h) / 2);

    let full_title = format!("{title} — ShotClip 편집");
    let hwnd = CreateWindowExW(
        0,
        wide(EDITOR_CLASS).as_ptr(),
        wide(&full_title).as_ptr(),
        style,
        x,
        y,
        w,
        h,
        null_mut(),
        null_mut(),
        hinst,
        null(),
    );
    if hwnd.is_null() {
        return;
    }
    let (tool, color, size) = PREFS.with(Cell::get);
    let committed = Rgba { w: img.w, h: img.h, px: img.px.clone() };
    let work = Rgba { w: img.w, h: img.h, px: img.px.clone() };
    let mut ed = Ed {
        hwnd,
        standalone,
        scale: Scale(ui.max(1.0)),
        base: img,
        objs: Vec::new(),
        crop: None,
        undo: Vec::new(),
        redo: Vec::new(),
        committed,
        work,
        disp,
        live_box: None,
        live: None,
        live_at: None,
        live_crop: None,
        editing_text: false,
        sel: None,
        drag: Drag::None,
        tool,
        color,
        hl_color: HL_DEFAULT,
        size,
        zoom: 1.0,
        ox: 0.0,
        oy: 0.0,
        fit: true,
        hover: None,
        space: false,
        edited: false,
        status: String::new(),
        layout: Vec::new(),
        export,
        title: title.to_string(),
    };
    to_disp(&ed.work, &mut ed.disp, None);
    ED.with(|c| *c.borrow_mut() = Some(ed));
    OPEN.with(|c| c.set(hwnd));
    with(|e| fit_view(e));
    ShowWindow(hwnd, SW_SHOWNORMAL);
    SetForegroundWindow(hwnd);
}

/// Exports (if edited) and closes.
unsafe fn finish(hwnd: HWND) {
    let Some(mut e) = ED.with(|c| c.borrow_mut().take()) else {
        DestroyWindow(hwnd);
        return;
    };
    commit_live(&mut e);
    PREFS.with(|c| c.set((e.tool, e.color, e.size)));
    // Unchanged (or everything undone): the original is already on the clipboard/Desktop.
    if !e.objs.is_empty() || e.crop.is_some() {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| result(&e)));
        match r {
            Ok(img) => (e.export)(img),
            Err(_) => crate::updater::log("editor: export panicked"),
        }
    }
    OPEN.with(|c| c.set(null_mut()));
    let standalone = e.standalone;
    drop(e);
    DestroyWindow(hwnd);
    if standalone {
        PostQuitMessage(0);
    }
}

/// Final image: annotations + crop.
fn result(e: &Ed) -> Rgba {
    let img = annot::render(&e.base, &e.objs, e.scale);
    match e.crop.and_then(|c| clip_rect(c, img.w, img.h)) {
        Some((x0, y0, x1, y1)) => {
            let w = (x1 - x0) as usize;
            let mut px = Vec::with_capacity(w * (y1 - y0) as usize * 4);
            for y in y0..y1 {
                let o = (y as usize * img.w as usize + x0 as usize) * 4;
                px.extend_from_slice(&img.px[o..o + w * 4]);
            }
            Rgba { w: w as u32, h: y1 - y0, px }
        }
        None => img,
    }
}

fn clip_rect(c: (f32, f32, f32, f32), w: u32, h: u32) -> Option<(u32, u32, u32, u32)> {
    let x0 = c.0.min(c.2).round().clamp(0.0, w as f32) as u32;
    let x1 = c.0.max(c.2).round().clamp(0.0, w as f32) as u32;
    let y0 = c.1.min(c.3).round().clamp(0.0, h as f32) as u32;
    let y1 = c.1.max(c.3).round().clamp(0.0, h as f32) as u32;
    (x1 > x0 + 1 && y1 > y0 + 1).then_some((x0, y0, x1, y1))
}

fn with<R>(f: impl FnOnce(&mut Ed) -> R) -> Option<R> {
    ED.with(|c| c.try_borrow_mut().ok()?.as_mut().map(f))
}

// ---------------------------------------------------------------- pixels

/// Copies RGBA `src` into the BGRA DIB (whole image or a region).
fn to_disp(src: &Rgba, dst: &mut Surface, region: Option<(u32, u32, u32, u32)>) {
    let (x0, y0, x1, y1) = region.unwrap_or((0, 0, src.w, src.h));
    let w = src.w as usize;
    let out = unsafe { std::slice::from_raw_parts_mut(dst.bits, w * src.h as usize * 4) };
    for y in y0 as usize..y1 as usize {
        let row = y * w * 4;
        for x in x0 as usize..x1 as usize {
            let i = row + x * 4;
            out[i] = src.px[i + 2];
            out[i + 1] = src.px[i + 1];
            out[i + 2] = src.px[i];
            out[i + 3] = 255;
        }
    }
}

fn union(a: Option<(f32, f32, f32, f32)>, b: Option<(f32, f32, f32, f32)>) -> Option<(f32, f32, f32, f32)> {
    match (a, b) {
        (Some(a), Some(b)) => Some((a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))),
        (a, b) => a.or(b),
    }
}

/// Re-renders all committed objects (after undo, commit, delete …).
fn rebuild(e: &mut Ed) {
    e.committed = annot::render(&e.base, &e.objs, e.scale);
    e.work = Rgba { w: e.committed.w, h: e.committed.h, px: e.committed.px.clone() };
    e.live_box = None;
    draw_live(e);
    to_disp(&e.work, &mut e.disp, None);
}

/// Updates `work`/`disp` for the current live object, touching only the changed region.
fn draw_live(e: &mut Ed) {
    let new_box = e.live.as_ref().map(|o| o.bounds(e.scale));
    let dirty = union(e.live_box, new_box);
    let Some(r) = dirty.and_then(|d| clip_rect((d.0 - 2.0, d.1 - 2.0, d.2 + 2.0, d.3 + 2.0), e.base.w, e.base.h)) else {
        e.live_box = new_box;
        return;
    };
    let w = e.base.w as usize;
    for y in r.1 as usize..r.3 as usize {
        let (a, b) = ((y * w + r.0 as usize) * 4, (y * w + r.2 as usize) * 4);
        e.work.px[a..b].copy_from_slice(&e.committed.px[a..b]);
    }
    if let Some(o) = &e.live {
        annot::draw(&mut e.work, &e.base, o, e.scale);
    }
    e.live_box = new_box;
    to_disp(&e.work, &mut e.disp, Some(r));
}

fn snapshot(e: &mut Ed) {
    e.undo.push((e.objs.clone(), e.crop));
    e.redo.clear();
    e.edited = true;
}

/// Puts the live object (new / moved / edited text) into `objs`.
fn commit_live(e: &mut Ed) {
    e.editing_text = false;
    let Some(o) = e.live.take() else { return };
    let at = e.live_at.take();
    if o.degenerate() {
        if at.is_some() {
            // An existing object that became empty (text erased): drop it.
            e.edited = true;
        }
        e.sel = None;
    } else {
        let i = at.unwrap_or(e.objs.len()).min(e.objs.len());
        e.objs.insert(i, o);
        e.sel = Some(i);
    }
    rebuild(e);
}

/// Takes object `i` out of `objs` into the live slot (for moving / editing).
fn lift(e: &mut Ed, i: usize) {
    snapshot(e);
    let o = e.objs.remove(i);
    e.live = Some(o);
    e.live_at = Some(i);
    e.sel = None;
    rebuild(e);
}

fn undo(e: &mut Ed, redo: bool) {
    commit_live(e);
    let (from, to) = if redo { (&mut e.redo, &mut e.undo) } else { (&mut e.undo, &mut e.redo) };
    if let Some(state) = from.pop() {
        to.push((e.objs.clone(), e.crop));
        (e.objs, e.crop) = state;
        e.sel = None;
        e.edited = true;
        rebuild(e);
    }
}

// ---------------------------------------------------------------- view

unsafe fn dpi(e: &Ed) -> f32 {
    GetDpiForWindow(e.hwnd) as f32 / 96.0
}

unsafe fn bars(e: &Ed) -> (i32, i32) {
    let u = dpi(e);
    ((44.0 * u) as i32, (26.0 * u) as i32)
}

unsafe fn canvas(e: &Ed) -> RECT {
    let mut rc: RECT = zeroed();
    GetClientRect(e.hwnd, &mut rc);
    let (tb, sb) = bars(e);
    RECT { left: 0, top: tb, right: rc.right, bottom: (rc.bottom - sb).max(tb) }
}

unsafe fn fit_view(e: &mut Ed) {
    let c = canvas(e);
    let (cw, ch) = ((c.right - c.left - 24) as f32, (c.bottom - c.top - 24) as f32);
    e.zoom = (cw / e.base.w as f32).min(ch / e.base.h as f32).clamp(0.05, 1.0);
    e.fit = true;
    clamp_view(e, true);
    InvalidateRect(e.hwnd, null(), 0);
}

/// Centres the image when it is smaller than the canvas, else keeps it covering the canvas.
unsafe fn clamp_view(e: &mut Ed, center: bool) {
    let c = canvas(e);
    let (iw, ih) = (e.base.w as f32 * e.zoom, e.base.h as f32 * e.zoom);
    let (cw, ch) = ((c.right - c.left) as f32, (c.bottom - c.top) as f32);
    let (l, t) = (c.left as f32, c.top as f32);
    if iw <= cw {
        e.ox = l + (cw - iw) / 2.0;
    } else if center {
        e.ox = l + (cw - iw) / 2.0;
    } else {
        e.ox = e.ox.min(l + 40.0).max(l + cw - iw - 40.0);
    }
    if ih <= ch {
        e.oy = t + (ch - ih) / 2.0;
    } else if center {
        e.oy = t + (ch - ih) / 2.0;
    } else {
        e.oy = e.oy.min(t + 40.0).max(t + ch - ih - 40.0);
    }
    e.ox = e.ox.round();
    e.oy = e.oy.round();
}

unsafe fn zoom_at(e: &mut Ed, z: f32, cx: f32, cy: f32) {
    let z = z.clamp(0.05, 16.0);
    let (ix, iy) = ((cx - e.ox) / e.zoom, (cy - e.oy) / e.zoom);
    e.zoom = z;
    e.ox = cx - ix * z;
    e.oy = cy - iy * z;
    e.fit = false;
    clamp_view(e, false);
    InvalidateRect(e.hwnd, null(), 0);
}

unsafe fn zoom_step(e: &mut Ed, up: bool) {
    const STEPS: [f32; 13] = [0.1, 0.17, 0.25, 0.33, 0.5, 0.67, 0.75, 1.0, 1.5, 2.0, 3.0, 4.0, 8.0];
    let next = if up {
        STEPS.iter().copied().find(|&s| s > e.zoom + 0.001).unwrap_or(16.0)
    } else {
        STEPS.iter().rev().copied().find(|&s| s < e.zoom - 0.001).unwrap_or(0.05)
    };
    let c = canvas(e);
    let (cx, cy) = match e.hover {
        Some((x, y)) => (e.ox + x * e.zoom, e.oy + y * e.zoom),
        None => (((c.left + c.right) / 2) as f32, ((c.top + c.bottom) / 2) as f32),
    };
    zoom_at(e, next, cx, cy);
}

fn to_img(e: &Ed, x: i32, y: i32) -> (f32, f32) {
    ((x as f32 - e.ox) / e.zoom, (y as f32 - e.oy) / e.zoom)
}

fn to_client(e: &Ed, x: f32, y: f32) -> (i32, i32) {
    ((e.ox + x * e.zoom).round() as i32, (e.oy + y * e.zoom).round() as i32)
}

// ---------------------------------------------------------------- paint

unsafe fn ui_font(px: i32, bold: bool) -> HFONT {
    CreateFontW(
        -px,
        0,
        0,
        0,
        if bold { 600 } else { 400 },
        0,
        0,
        0,
        DEFAULT_CHARSET as u32,
        0,
        0,
        CLEARTYPE_QUALITY as u32,
        0,
        wide("Malgun Gothic").as_ptr(),
    )
}

unsafe fn fill(dc: HDC, r: &RECT, c: u32) {
    let b = CreateSolidBrush(rgb(c));
    FillRect(dc, r, b);
    DeleteObject(b);
}

fn rgb(c: u32) -> COLORREF {
    ((c & 0xFF) << 16) | (c & 0xFF00) | ((c >> 16) & 0xFF)
}

unsafe fn text_w(dc: HDC, s: &str) -> i32 {
    let w = wide(s);
    let mut sz: SIZE = zeroed();
    GetTextExtentPoint32W(dc, w.as_ptr(), w.len() as i32 - 1, &mut sz);
    sz.cx
}

unsafe fn draw_text(dc: HDC, s: &str, r: &RECT, color: u32, flags: u32) {
    let w = wide(s);
    let mut r = *r;
    SetTextColor(dc, rgb(color));
    DrawTextW(dc, w.as_ptr(), w.len() as i32 - 1, &mut r, flags | DT_NOPREFIX | DT_SINGLELINE | DT_VCENTER);
}

unsafe fn paint(e: &mut Ed) {
    let hwnd = e.hwnd;
    let mut ps: PAINTSTRUCT = zeroed();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mut rc: RECT = zeroed();
    GetClientRect(hwnd, &mut rc);
    let (cw, ch) = (rc.right.max(1), rc.bottom.max(1));
    let mdc = CreateCompatibleDC(hdc);
    let mbmp = CreateCompatibleBitmap(hdc, cw, ch);
    let old = SelectObject(mdc, mbmp);
    SetBkMode(mdc, TRANSPARENT as i32);

    let c = canvas(e);
    fill(mdc, &rc, 0x1E1E1E);

    // Image: blit only the visible source rectangle.
    let z = e.zoom;
    let (w, h) = (e.base.w as f32, e.base.h as f32);
    let sx0 = ((c.left as f32 - e.ox) / z).floor().clamp(0.0, w);
    let sy0 = ((c.top as f32 - e.oy) / z).floor().clamp(0.0, h);
    let sx1 = ((c.right as f32 - e.ox) / z).ceil().clamp(0.0, w);
    let sy1 = ((c.bottom as f32 - e.oy) / z).ceil().clamp(0.0, h);
    if sx1 > sx0 && sy1 > sy0 {
        let save = SaveDC(mdc);
        IntersectClipRect(mdc, c.left, c.top, c.right, c.bottom);
        SetStretchBltMode(mdc, if z < 1.0 && e.drag == Drag::None { HALFTONE } else { COLORONCOLOR });
        SetBrushOrgEx(mdc, 0, 0, null_mut());
        let (dx0, dy0) = to_client(e, sx0, sy0);
        let (dx1, dy1) = to_client(e, sx1, sy1);
        StretchBlt(
            mdc,
            dx0,
            dy0,
            dx1 - dx0,
            dy1 - dy0,
            e.disp.dc,
            sx0 as i32,
            sy0 as i32,
            (sx1 - sx0) as i32,
            (sy1 - sy0) as i32,
            SRCCOPY,
        );
        paint_crop(e, mdc);
        paint_selection(e, mdc);
        RestoreDC(mdc, save);
    }
    paint_toolbar(e, mdc, cw);
    paint_status(e, mdc, cw, ch);

    BitBlt(hdc, 0, 0, cw, ch, mdc, 0, 0, SRCCOPY);
    SelectObject(mdc, old);
    DeleteObject(mbmp);
    DeleteDC(mdc);
    EndPaint(hwnd, &ps);
}

unsafe fn shade(dc: HDC, r: RECT, alpha: u8) {
    if r.right <= r.left || r.bottom <= r.top {
        return;
    }
    let Some(s) = Surface::new(1, 1) else { return };
    *s.bits = 0;
    let bf = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: alpha, AlphaFormat: 0 };
    GdiAlphaBlend(dc, r.left, r.top, r.right - r.left, r.bottom - r.top, s.dc, 0, 0, 1, 1, bf);
}

unsafe fn frame(dc: HDC, r: RECT, color: u32, style: PEN_STYLE, width: i32) {
    let pen = CreatePen(style, width, rgb(color));
    let op = SelectObject(dc, pen);
    let ob = SelectObject(dc, GetStockObject(NULL_BRUSH));
    Rectangle(dc, r.left, r.top, r.right, r.bottom);
    SelectObject(dc, ob);
    SelectObject(dc, op);
    DeleteObject(pen);
}

unsafe fn paint_crop(e: &Ed, dc: HDC) {
    let Some(c) = e.live_crop.or(e.crop) else { return };
    let (x0, y0) = to_client(e, c.0.min(c.2), c.1.min(c.3));
    let (x1, y1) = to_client(e, c.0.max(c.2), c.1.max(c.3));
    let (ix0, iy0) = to_client(e, 0.0, 0.0);
    let (ix1, iy1) = to_client(e, e.base.w as f32, e.base.h as f32);
    let a = 150;
    shade(dc, RECT { left: ix0, top: iy0, right: ix1, bottom: y0 }, a);
    shade(dc, RECT { left: ix0, top: y1, right: ix1, bottom: iy1 }, a);
    shade(dc, RECT { left: ix0, top: y0, right: x0, bottom: y1 }, a);
    shade(dc, RECT { left: x1, top: y0, right: ix1, bottom: y1 }, a);
    frame(dc, RECT { left: x0 - 1, top: y0 - 1, right: x1 + 1, bottom: y1 + 1 }, 0xFFFFFF, PS_SOLID, 1);
}

unsafe fn paint_selection(e: &Ed, dc: HDC) {
    let target = match (e.sel, &e.live) {
        (Some(i), _) => e.objs.get(i),
        (None, Some(o)) if e.editing_text => Some(o),
        _ => None,
    };
    let Some(o) = target else { return };
    let b = o.bounds(e.scale);
    let (x0, y0) = to_client(e, b.0, b.1);
    let (x1, y1) = to_client(e, b.2, b.3);
    frame(dc, RECT { left: x0 - 3, top: y0 - 3, right: x1 + 3, bottom: y1 + 3 }, 0x3DA5FF, PS_DOT, 1);
    if e.editing_text {
        if let Shape::Text(t) = &o.shape {
            // Caret after the last character.
            let px = e.scale.font_px(o.size);
            let last = t.rsplit('\n').next().unwrap_or("");
            let lines = t.split('\n').count() as i32;
            let line_h = annot::text_size("Ag", px).1;
            let lw = if last.is_empty() { 0 } else { annot::text_size(last, px).0 };
            let (cx, cy) = to_client(e, o.a.0 + lw as f32 + 1.0, o.a.1 + (line_h * (lines - 1)) as f32);
            let hh = (line_h as f32 * e.zoom) as i32;
            let r = RECT { left: cx, top: cy, right: cx + 2, bottom: cy + hh };
            fill(dc, &r, 0x3DA5FF);
        }
    }
}

unsafe fn paint_toolbar(e: &mut Ed, dc: HDC, cw: i32) {
    let u = dpi(e);
    let (tb, _) = bars(e);
    fill(dc, &RECT { left: 0, top: 0, right: cw, bottom: tb }, 0x2B2B2B);
    let f = ui_font((13.0 * u) as i32, false);
    let fk = ui_font((11.0 * u) as i32, true);
    let of = SelectObject(dc, f);
    let pad = (6.0 * u) as i32;
    let (top, bot) = (pad, tb - pad);
    let mut x = pad;
    e.layout.clear();
    for (t, key, name) in TOOLS {
        SelectObject(dc, fk);
        let kw = text_w(dc, &key.to_string());
        SelectObject(dc, f);
        let w = text_w(dc, name) + kw + (16.0 * u) as i32;
        let r = RECT { left: x, top, right: x + w, bottom: bot };
        if e.tool == t {
            fill(dc, &r, 0x4A4A4A);
            fill(dc, &RECT { left: r.left, top: r.bottom - (2.0 * u) as i32, right: r.right, bottom: r.bottom }, 0x3DA5FF);
        }
        let inner = RECT { left: r.left + (6.0 * u) as i32, ..r };
        SelectObject(dc, fk);
        draw_text(dc, &key.to_string(), &inner, 0x8A8A8A, DT_LEFT);
        SelectObject(dc, f);
        let inner = RECT { left: inner.left + kw + (4.0 * u) as i32, ..r };
        draw_text(dc, name, &inner, 0xF0F0F0, DT_LEFT);
        e.layout.push((r, Item::Tool(t)));
        x += w + (2.0 * u) as i32;
    }
    x += (10.0 * u) as i32;
    let cur = if e.tool == Tool::Highlight { e.hl_color } else { e.color };
    let sw = (bot - top) - (6.0 * u) as i32;
    for c in COLORS.iter().copied().chain((e.tool == Tool::Highlight).then_some(HL_DEFAULT)) {
        let r = RECT { left: x, top: top + (3.0 * u) as i32, right: x + sw, bottom: top + (3.0 * u) as i32 + sw };
        if c == cur {
            fill(dc, &RECT { left: r.left - 3, top: r.top - 3, right: r.right + 3, bottom: r.bottom + 3 }, 0xF0F0F0);
        }
        fill(dc, &r, c);
        e.layout.push((r, Item::Color(c)));
        x += sw + (5.0 * u) as i32;
    }
    x += (8.0 * u) as i32;
    for s in 0..3u8 {
        let r = RECT { left: x, top, right: x + sw + (4.0 * u) as i32, bottom: bot };
        if e.size == s {
            fill(dc, &r, 0x4A4A4A);
        }
        let d = [(5.0 * u) as i32, (9.0 * u) as i32, (14.0 * u) as i32][s as usize];
        let (cx, cy) = ((r.left + r.right) / 2, (r.top + r.bottom) / 2);
        let b = CreateSolidBrush(rgb(0xF0F0F0));
        let ob = SelectObject(dc, b);
        let op = SelectObject(dc, GetStockObject(NULL_PEN));
        Ellipse(dc, cx - d / 2, cy - d / 2, cx + d / 2 + 1, cy + d / 2 + 1);
        SelectObject(dc, op);
        SelectObject(dc, ob);
        DeleteObject(b);
        e.layout.push((r, Item::Size(s)));
        x += r.right - r.left + (2.0 * u) as i32;
    }
    // Right side: undo, save, done.
    let mut rx = cw - pad;
    for (label, item, accent) in [("완료 · 복사  Esc", Item::Done, true), ("저장  Ctrl+S", Item::Save, false), ("되돌리기", Item::Undo, false)] {
        let w = text_w(dc, label) + (20.0 * u) as i32;
        let r = RECT { left: rx - w, top, right: rx, bottom: bot };
        if r.left < x + pad {
            break;
        }
        fill(dc, &r, if accent { 0x1E6FD9 } else { 0x3A3A3A });
        draw_text(dc, label, &r, 0xFFFFFF, DT_CENTER);
        e.layout.push((r, item));
        rx -= w + (6.0 * u) as i32;
    }
    SelectObject(dc, of);
    DeleteObject(f);
    DeleteObject(fk);
}

unsafe fn paint_status(e: &Ed, dc: HDC, cw: i32, ch: i32) {
    let u = dpi(e);
    let (_, sb) = bars(e);
    let r = RECT { left: 0, top: ch - sb, right: cw, bottom: ch };
    fill(dc, &r, 0x2B2B2B);
    let f = ui_font((12.0 * u) as i32, false);
    let of = SelectObject(dc, f);
    let (w, h) = match e.crop.and_then(|c| clip_rect(c, e.base.w, e.base.h)) {
        Some((x0, y0, x1, y1)) => (x1 - x0, y1 - y0),
        None => (e.base.w, e.base.h),
    };
    let mut left = format!("{w} × {h}   {}%", (e.zoom * 100.0).round());
    if let Some((x, y)) = e.hover.filter(|&(x, y)| x >= 0.0 && y >= 0.0 && x < e.base.w as f32 && y < e.base.h as f32) {
        left += &format!("   ({}, {})", x as u32, y as u32);
    }
    if !e.status.is_empty() {
        left += "   ";
        left += &e.status;
    }
    let lr = RECT { left: (10.0 * u) as i32, ..r };
    draw_text(dc, &left, &lr, 0xC8C8C8, DT_LEFT);
    let hint = if e.editing_text {
        "입력 중 · Enter 끝내기 · Shift+Enter 줄바꿈"
    } else {
        "Ctrl+Z 되돌리기 · Del 삭제 · Tab 색상 복사 · 휠 이동 · Ctrl+휠 확대 · 0 원래 크기 · 1 맞춤"
    };
    let rr = RECT { right: cw - (10.0 * u) as i32, ..r };
    draw_text(dc, hint, &rr, 0x8A8A8A, DT_RIGHT);
    SelectObject(dc, of);
    DeleteObject(f);
}

// ---------------------------------------------------------------- input

fn cur_color(e: &Ed) -> u32 {
    if e.tool == Tool::Highlight { e.hl_color } else { e.color }
}

fn new_shape(e: &Ed, p: (f32, f32)) -> Option<Shape> {
    Some(match e.tool {
        Tool::Arrow => Shape::Arrow,
        Tool::Rect => Shape::Rect,
        Tool::Oval => Shape::Oval,
        Tool::Line => Shape::Line,
        Tool::Pen => Shape::Pen(vec![p]),
        Tool::Highlight => Shape::Highlight,
        Tool::Blur => Shape::Blur,
        Tool::Erase => Shape::Erase,
        Tool::Text | Tool::Counter | Tool::Crop => return None,
    })
}

fn set_tool(e: &mut Ed, t: Tool) {
    commit_live(e);
    e.tool = t;
    e.sel = None;
    e.status.clear();
}

/// Applies colour/size to the selection too (like most editors).
fn restyle_selection(e: &mut Ed, color: Option<u32>, size: Option<u8>) {
    let Some(i) = e.sel else { return };
    snapshot(e);
    if let Some(o) = e.objs.get_mut(i) {
        if let Some(c) = color {
            o.color = c;
        }
        if let Some(s) = size {
            o.size = s;
        }
    }
    rebuild(e);
}

unsafe fn click_item(e: &mut Ed, item: Item) -> After {
    match item {
        Item::Tool(t) => set_tool(e, t),
        Item::Color(c) => {
            if e.tool == Tool::Highlight {
                e.hl_color = c;
            } else {
                e.color = c;
            }
            if e.editing_text {
                if let Some(o) = e.live.as_mut() {
                    o.color = c;
                }
                draw_live(e);
            } else {
                restyle_selection(e, Some(c), None);
            }
        }
        Item::Size(s) => {
            e.size = s;
            if e.editing_text {
                if let Some(o) = e.live.as_mut() {
                    o.size = s;
                }
                draw_live(e);
            } else {
                restyle_selection(e, None, Some(s));
            }
        }
        Item::Undo => undo(e, false),
        Item::Save => return After::SaveAs,
        Item::Done => return After::Finish,
    }
    After::Nothing
}

fn constrain(e: &Ed, a: (f32, f32), mut b: (f32, f32)) -> (f32, f32) {
    let shift = unsafe { GetKeyState(VK_SHIFT as i32) } < 0;
    if !shift {
        return b;
    }
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    match e.tool {
        Tool::Arrow | Tool::Line => {
            // Snap to 45°.
            let ang = dy.atan2(dx);
            let snap = (ang / std::f32::consts::FRAC_PI_4).round() * std::f32::consts::FRAC_PI_4;
            let len = dx.hypot(dy);
            b = (a.0 + len * snap.cos(), a.1 + len * snap.sin());
        }
        _ => {
            let m = dx.abs().max(dy.abs());
            b = (a.0 + m * dx.signum(), a.1 + m * dy.signum());
        }
    }
    b
}

unsafe fn on_down(e: &mut Ed, x: i32, y: i32, button: u32) -> After {
    SetFocus(e.hwnd);
    if let Some(&(_, item)) = e.layout.iter().find(|(r, _)| x >= r.left && x < r.right && y >= r.top && y < r.bottom) {
        if button == 0 {
            return click_item(e, item);
        }
    }
    let c = canvas(e);
    if y < c.top || y >= c.bottom {
        return After::Nothing;
    }
    if button != 0 || e.space {
        e.drag = Drag::Pan { last: (x, y) };
        SetCapture(e.hwnd);
        return After::Nothing;
    }
    let p = to_img(e, x, y);
    let tol = 4.0 / e.zoom;

    if e.editing_text {
        if e.live.as_ref().is_some_and(|o| o.hit(e.scale, p.0, p.1, tol)) {
            return After::Nothing; // click inside the text being edited
        }
        commit_live(e);
    }
    if e.tool == Tool::Crop {
        e.drag = Drag::Crop { start: p };
        e.live_crop = Some((p.0, p.1, p.0, p.1));
        SetCapture(e.hwnd);
        return After::Nothing;
    }
    // Existing object under the cursor: select & move (text with the text tool: edit).
    if let Some(i) = (0..e.objs.len()).rev().find(|&i| e.objs[i].hit(e.scale, p.0, p.1, tol)) {
        let is_text = matches!(e.objs[i].shape, Shape::Text(_));
        lift(e, i);
        if is_text && e.tool == Tool::Text {
            e.editing_text = true;
            draw_live(e);
            InvalidateRect(e.hwnd, null(), 0);
            return After::Nothing;
        }
        e.drag = Drag::Move { last: p, moved: false };
        SetCapture(e.hwnd);
        return After::Nothing;
    }
    e.sel = None;
    let (color, size) = (cur_color(e), e.size);
    match e.tool {
        Tool::Text => {
            snapshot(e);
            let px = e.scale.font_px(size);
            let a = (p.0, p.1 - px as f32 * 0.6);
            e.live = Some(Obj { shape: Shape::Text(String::new()), a, b: a, color, size });
            e.live_at = None;
            e.editing_text = true;
            draw_live(e);
        }
        Tool::Counter => {
            snapshot(e);
            let n = e.objs.iter().filter_map(|o| if let Shape::Counter(n) = o.shape { Some(n) } else { None }).max().unwrap_or(0) + 1;
            e.objs.push(Obj { shape: Shape::Counter(n), a: p, b: p, color, size });
            e.sel = Some(e.objs.len() - 1);
            rebuild(e);
        }
        _ => {
            if let Some(shape) = new_shape(e, p) {
                snapshot(e);
                e.live = Some(Obj { shape, a: p, b: p, color, size });
                e.live_at = None;
                e.drag = Drag::Draw;
                SetCapture(e.hwnd);
            }
        }
    }
    InvalidateRect(e.hwnd, null(), 0);
    After::Nothing
}

unsafe fn on_move(e: &mut Ed, x: i32, y: i32) {
    let p = to_img(e, x, y);
    e.hover = Some(p);
    match e.drag {
        Drag::None => {}
        Drag::Pan { last } => {
            e.ox += (x - last.0) as f32;
            e.oy += (y - last.1) as f32;
            e.fit = false;
            clamp_view(e, false);
            e.drag = Drag::Pan { last: (x, y) };
        }
        Drag::Draw => {
            let a = e.live.as_ref().map(|o| o.a).unwrap_or(p);
            let b = constrain(e, a, p);
            if let Some(o) = e.live.as_mut() {
                match &mut o.shape {
                    Shape::Pen(pts) => {
                        let l = *pts.last().unwrap();
                        if (l.0 - p.0).hypot(l.1 - p.1) >= 1.5 / e.zoom.min(1.0) {
                            pts.push(p);
                        }
                    }
                    _ => o.b = b,
                }
            }
            draw_live(e);
        }
        Drag::Move { last, .. } => {
            if let Some(o) = e.live.as_mut() {
                o.translate(p.0 - last.0, p.1 - last.1);
            }
            e.drag = Drag::Move { last: p, moved: true };
            draw_live(e);
        }
        Drag::Crop { start } => {
            let b = constrain(e, start, p);
            let (w, h) = (e.base.w as f32, e.base.h as f32);
            e.live_crop = Some((start.0.clamp(0.0, w), start.1.clamp(0.0, h), b.0.clamp(0.0, w), b.1.clamp(0.0, h)));
        }
    }
    InvalidateRect(e.hwnd, null(), 0);
}

unsafe fn on_up(e: &mut Ed) {
    let drag = std::mem::replace(&mut e.drag, Drag::None);
    ReleaseCapture();
    match drag {
        Drag::Draw => {
            let keep = e.live.as_ref().is_some_and(|o| !o.degenerate());
            if keep {
                commit_live(e);
                // The drawn object stays selected only for a moment's restyle; keep drawing next.
            } else {
                e.live = None;
                e.undo.pop(); // nothing was added
                draw_live(e);
            }
        }
        Drag::Move { moved, .. } => {
            commit_live(e);
            if !moved {
                // A plain click only selected it; don't keep an undo step.
                if let Some(state) = e.undo.pop() {
                    debug_assert_eq!(state.0.len(), e.objs.len());
                }
            }
        }
        Drag::Crop { .. } => {
            if let Some(c) = e.live_crop.take().filter(|&c| clip_rect(c, e.base.w, e.base.h).is_some()) {
                snapshot(e);
                e.crop = Some(c);
            }
        }
        _ => {}
    }
    InvalidateRect(e.hwnd, null(), 0);
}

unsafe fn copy_color(e: &mut Ed) {
    let Some((x, y)) = e.hover else { return };
    if x < 0.0 || y < 0.0 || x >= e.base.w as f32 || y >= e.base.h as f32 {
        return;
    }
    let i = ((y as usize) * e.base.w as usize + x as usize) * 4;
    let p = &e.work.px[i..i + 3];
    let hex = format!("#{:02X}{:02X}{:02X}", p[0], p[1], p[2]);
    e.status = if crate::clip::write_text(e.hwnd, &hex) { format!("{hex} 복사함") } else { "복사 실패".into() };
}

unsafe fn on_key(e: &mut Ed, vk: u32) -> After {
    let ctrl = GetKeyState(VK_CONTROL as i32) < 0;
    let shift = GetKeyState(VK_SHIFT as i32) < 0;
    if e.editing_text {
        match vk {
            0x0D if !shift => commit_live(e), // Enter
            0x1B => commit_live(e),           // Esc
            _ => return After::Nothing,       // characters arrive as WM_CHAR
        }
        InvalidateRect(e.hwnd, null(), 0);
        return After::Nothing;
    }
    if ctrl {
        match vk {
            0x5A if shift => undo(e, true), // Ctrl+Shift+Z
            0x5A => undo(e, false),
            0x59 => undo(e, true), // Ctrl+Y
            0x43 => return After::Finish,
            0x53 => return After::SaveAs,
            0x30 | 0x60 => zoom_at_center(e, 1.0),
            0xBB | 0x6B => zoom_step(e, true),
            0xBD | 0x6D => zoom_step(e, false),
            _ => {}
        }
        InvalidateRect(e.hwnd, null(), 0);
        return After::Nothing;
    }
    match vk {
        0x1B => {
            if e.drag != Drag::None {
                // Cancel the drag in progress.
                e.drag = Drag::None;
                ReleaseCapture();
                e.live_crop = None;
                if e.live_at.is_none() && e.live.take().is_some() {
                    e.undo.pop();
                    draw_live(e);
                } else {
                    commit_live(e);
                }
            } else if e.sel.take().is_none() {
                return After::Finish;
            }
        }
        0x0D => return After::Finish,
        VK_DELETE_U | VK_BACK_U => {
            if let Some(i) = e.sel.take() {
                snapshot(e);
                e.objs.remove(i);
                rebuild(e);
            }
        }
        0x09 => copy_color(e),
        0x20 => e.space = true,
        0x25..=0x28 => {
            if let Some(i) = e.sel {
                let d = if shift { 10.0 } else { 1.0 };
                let (dx, dy) = [(-d, 0.0), (0.0, -d), (d, 0.0), (0.0, d)][(vk - 0x25) as usize];
                snapshot(e);
                e.objs[i].translate(dx, dy);
                rebuild(e);
            }
        }
        0xBB | 0x6B => zoom_step(e, true),
        0xBD | 0x6D => zoom_step(e, false),
        0x30 | 0x60 => zoom_at_center(e, 1.0),
        0x31 | 0x61 => fit_view(e),
        _ => {
            if let Some(&(t, _, _)) = TOOLS.iter().find(|(_, k, _)| *k as u32 == vk) {
                set_tool(e, t);
            }
        }
    }
    InvalidateRect(e.hwnd, null(), 0);
    After::Nothing
}

const VK_DELETE_U: u32 = 0x2E;
const VK_BACK_U: u32 = 0x08;

unsafe fn zoom_at_center(e: &mut Ed, z: f32) {
    let c = canvas(e);
    zoom_at(e, z, ((c.left + c.right) / 2) as f32, ((c.top + c.bottom) / 2) as f32);
    if z == 1.0 {
        clamp_view(e, true);
    }
}

unsafe fn on_char(e: &mut Ed, ch: u16) {
    if !e.editing_text {
        return;
    }
    let Some(o) = e.live.as_mut() else { return };
    let Shape::Text(t) = &mut o.shape else { return };
    match ch {
        0x08 => {
            t.pop();
        }
        0x0D => {
            if GetKeyState(VK_SHIFT as i32) < 0 {
                t.push('\n');
            }
        }
        0x1B | 0x09 => {}
        c if c < 0x20 => {}
        c => {
            if let Some(c) = char::from_u32(c as u32) {
                t.push(c);
            }
        }
    }
    draw_live(e);
    InvalidateRect(e.hwnd, null(), 0);
}

// ---------------------------------------------------------------- save as

unsafe fn save_as(hwnd: HWND) {
    let Some((img, title)) = with(|e| {
        commit_live(e);
        (std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| result(e))).ok(), e.title.clone())
    }) else {
        return;
    };
    let Some(img) = img else { return };
    let mut buf = [0u16; 1024];
    let name = wide(&format!("{title}.png"));
    let n = name.len().min(buf.len()) - 1;
    buf[..n].copy_from_slice(&name[..n]);
    let filter: Vec<u16> = "PNG 이미지 (*.png)\0*.png\0\0".encode_utf16().collect();
    let dir = wide(&crate::capture::desktop_dir().to_string_lossy());
    let ext = wide("png");
    let mut of: OPENFILENAMEW = zeroed();
    of.lStructSize = size_of::<OPENFILENAMEW>() as u32;
    of.hwndOwner = hwnd;
    of.lpstrFilter = filter.as_ptr();
    of.lpstrFile = buf.as_mut_ptr();
    of.nMaxFile = buf.len() as u32;
    of.lpstrInitialDir = dir.as_ptr();
    of.lpstrDefExt = ext.as_ptr();
    of.Flags = OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR;
    if GetSaveFileNameW(&mut of) == 0 {
        return;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let path = String::from_utf16_lossy(&buf[..len]);
    let msg = match crate::optimize::encode_png(&img).map_err(|e| e.to_string()).and_then(|png| std::fs::write(&path, png).map_err(|e| e.to_string())) {
        Ok(()) => format!("저장함: {path}"),
        Err(err) => format!("저장 실패: {err}"),
    };
    with(|e| e.status = msg);
    InvalidateRect(hwnd, null(), 0);
}

// ---------------------------------------------------------------- window procedure

unsafe fn handle(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<(LRESULT, After)> {
    let x = (lp & 0xFFFF) as i16 as i32;
    let y = ((lp >> 16) & 0xFFFF) as i16 as i32;
    let r = with(|e| -> Option<After> {
        Some(match msg {
            WM_PAINT => {
                paint(e);
                After::Nothing
            }
            WM_ERASEBKGND => After::Nothing,
            WM_SIZE => {
                if e.fit {
                    fit_view(e);
                } else {
                    clamp_view(e, false);
                }
                After::Nothing
            }
            WM_LBUTTONDOWN => on_down(e, x, y, 0),
            WM_RBUTTONDOWN | WM_MBUTTONDOWN => on_down(e, x, y, 1),
            WM_LBUTTONDBLCLK => {
                // Double-click a text with any tool: edit it.
                let p = to_img(e, x, y);
                if let Some(i) = (0..e.objs.len()).rev().find(|&i| matches!(e.objs[i].shape, Shape::Text(_)) && e.objs[i].hit(e.scale, p.0, p.1, 2.0)) {
                    lift(e, i);
                    e.editing_text = true;
                    draw_live(e);
                    InvalidateRect(hwnd, null(), 0);
                }
                After::Nothing
            }
            WM_MOUSEMOVE => {
                on_move(e, x, y);
                After::Nothing
            }
            WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP => {
                if e.drag != Drag::None {
                    on_up(e);
                }
                After::Nothing
            }
            WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
                let delta = ((wp >> 16) & 0xFFFF) as i16 as f32 / 120.0;
                let ctrl = GetKeyState(VK_CONTROL as i32) < 0;
                let shift = GetKeyState(VK_SHIFT as i32) < 0;
                if msg == WM_MOUSEWHEEL && ctrl {
                    let mut pt = POINT { x, y };
                    ScreenToClient(hwnd, &mut pt);
                    zoom_at(e, e.zoom * 1.15f32.powf(delta), pt.x as f32, pt.y as f32);
                } else {
                    let step = 60.0 * dpi(e) * delta;
                    if msg == WM_MOUSEHWHEEL || shift {
                        e.ox -= if msg == WM_MOUSEHWHEEL { -step } else { step };
                    } else {
                        e.oy += step;
                    }
                    e.fit = false;
                    clamp_view(e, false);
                    InvalidateRect(hwnd, null(), 0);
                }
                After::Nothing
            }
            WM_KEYDOWN => on_key(e, wp as u32),
            WM_KEYUP => {
                if wp == 0x20 {
                    e.space = false;
                }
                After::Nothing
            }
            WM_CHAR => {
                on_char(e, wp as u16);
                After::Nothing
            }
            WM_SETCURSOR if (lp & 0xFFFF) as u32 == HTCLIENT => {
                let mut pt: POINT = zeroed();
                GetCursorPos(&mut pt);
                ScreenToClient(hwnd, &mut pt);
                let c = canvas(e);
                let id = if pt.y < c.top || pt.y >= c.bottom {
                    IDC_ARROW
                } else if e.space || matches!(e.drag, Drag::Pan { .. }) {
                    IDC_SIZEALL
                } else if e.tool == Tool::Text {
                    IDC_IBEAM
                } else {
                    IDC_CROSS
                };
                SetCursor(LoadCursorW(null_mut(), id));
                return None; // handled, return TRUE below
            }
            WM_GETMINMAXINFO => return None,
            _ => return None,
        })
    });
    match r {
        Some(Some(a)) => Some((0, a)),
        Some(None) if msg == WM_SETCURSOR && (lp & 0xFFFF) as u32 == HTCLIENT => Some((1, After::Nothing)),
        _ => None,
    }
}

pub unsafe extern "system" fn editor_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_CLOSE => {
            finish(hwnd);
            return 0;
        }
        WM_DESTROY => {
            if OPEN.with(Cell::get) == hwnd {
                // Destroyed without finish (e.g. app exit): drop the state.
                OPEN.with(|c| c.set(null_mut()));
                ED.with(|c| c.borrow_mut().take());
            }
            return 0;
        }
        _ => {}
    }
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handle(hwnd, msg, wp, lp)));
    match r {
        Ok(Some((res, after))) => {
            match after {
                After::Nothing => {}
                After::Finish => finish(hwnd),
                After::SaveAs => save_as(hwnd),
            }
            res
        }
        Ok(None) => DefWindowProcW(hwnd, msg, wp, lp),
        Err(_) => {
            crate::updater::log("editor: panic in window procedure; closing editor");
            OPEN.with(|c| c.set(null_mut()));
            let _ = ED.with(|c| c.try_borrow_mut().map(|mut b| b.take()));
            DestroyWindow(hwnd);
            0
        }
    }
}

/// Registers the window class (call once at start-up).
pub unsafe fn register(hinst: HINSTANCE) {
    let mut wc: WNDCLASSW = zeroed();
    wc.style = CS_DBLCLKS;
    wc.lpfnWndProc = Some(editor_proc);
    wc.hInstance = hinst;
    wc.hCursor = LoadCursorW(null_mut(), IDC_ARROW);
    let name = wide(EDITOR_CLASS);
    wc.lpszClassName = name.as_ptr();
    RegisterClassW(&wc);
}
