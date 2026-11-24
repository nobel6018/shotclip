//! Post-capture editor (opens after every capture, like Shottr on macOS).
//!
//! The capture is already optimised, copied and saved before this window opens; the editor only
//! re-exports when something was changed. Closing (Esc / Enter / Ctrl+C / ✕) exports the edited
//! image through the `export` callback (main.rs sends it through the same pipeline, overwriting
//! the same Desktop file) and closes. Ctrl+S saves a copy elsewhere.
//!
//! Usability follows Shottr 1.10.0b (keys in keymap.rs, evidence in parity/shottr_keys.md):
//! V select (drag = area selection, then R/O/B/S/M/Del turn it into an object, arrows move it,
//! Ctrl+arrows resize, [ ] shrink/grow, Enter crops, Ctrl+C copies, Ctrl+O reads text), A arrow,
//! R rectangle, O oval, L line, D pen, T text, H highlighter, B blur, S spotlight, M magnifier,
//! C counter, E erase; Tab / Shift+Tab copy the colour (darkest) under the cursor, arrows / 1 / 2
//! held measure, P px/pt, Ctrl+D duplicate, Ctrl+[ ] size, Ctrl+1 / Ctrl+0 / Ctrl+2 zoom.

use crate::annot::{self, Obj, Scale, Shape, Tool};
use crate::keymap::{self, Act, Ctx, Key, Mods};
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
        Input::{Ime::*, KeyboardAndMouse::*},
        WindowsAndMessaging::*,
    },
};

pub const EDITOR_CLASS: &str = "ShotClipEditor";

type Export = Box<dyn FnMut(Rgba)>;

const TOOLS: [(Tool, char, &str); 13] = [
    (Tool::Select, 'V', "선택"),
    (Tool::Arrow, 'A', "화살표"),
    (Tool::Rect, 'R', "사각형"),
    (Tool::Oval, 'O', "원"),
    (Tool::Line, 'L', "선"),
    (Tool::Pen, 'D', "펜"),
    (Tool::Text, 'T', "텍스트"),
    (Tool::Highlight, 'H', "형광펜"),
    (Tool::Blur, 'B', "흐림"),
    (Tool::Spotlight, 'S', "강조"),
    (Tool::Magnifier, 'M', "돋보기"),
    (Tool::Counter, 'C', "번호"),
    (Tool::Erase, 'E', "지우기"),
];
/// Custom message: OCR finished on the worker thread.
const WM_EDITOR_OCR: u32 = WM_APP + 40;
const COLORS: [u32; 8] = [0xE5383B, 0xF77F00, 0xFFD60A, 0x2DC653, 0x1E88E5, 0x8E44AD, 0x111111, 0xFFFFFF];
const HL_DEFAULT: u32 = 0xFFE14D;

#[derive(Clone, Copy, PartialEq)]
enum Item {
    Tool(Tool),
    Color(u32),
    Size(u8),
    Tail,
    Undo,
    Save,
    Done,
}

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    None,
    Draw,
    Move { last: (f32, f32), moved: bool },
    /// New area selection (Select tool).
    Area { start: (f32, f32) },
    /// Moving the area selection.
    AreaMove { last: (f32, f32) },
    Pan { last: (i32, i32) },
    /// Text tool drag: the tail tip stays where the drag started, the box follows the cursor.
    TextTail { start: (f32, f32) },
    /// Dragging the tip of an existing text's tail.
    Tip,
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
    /// Area selection (Shottr "cutter"), image coordinates.
    area: Crop,
    /// P: sizes in points (physical px / scale) instead of pixels.
    pt_units: bool,
    /// Measuring while an arrow / 1 / 2 is held: true = horizontal.
    ruler: Option<bool>,
    editing_text: bool,
    /// IME composition in progress (shown after the text, not yet part of it).
    comp: String,
    /// Tail on/off for new text boxes.
    tail: bool,
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
    static PREFS: Cell<(Tool, u32, u8)> = const { Cell::new((Tool::Select, COLORS[0], 1)) };
    /// Ctrl+C / Ctrl+V of an object (survives between editor windows, like Shottr).
    static OBJ_CLIP: RefCell<Option<Obj>> = const { RefCell::new(None) };
}

static OCR_OUT: std::sync::Mutex<Option<Result<String, String>>> = std::sync::Mutex::new(None);

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
        area: None,
        pt_units: false,
        ruler: None,
        editing_text: false,
        comp: String::new(),
        tail: true,
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
    set_ime(hwnd, false);
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
/// The live object as displayed: text being typed includes the IME composition.
fn shown(e: &Ed) -> Option<Obj> {
    let mut o = e.live.clone()?;
    if let Shape::Text(t) = &mut o.shape {
        t.push_str(&e.comp);
    }
    Some(o)
}

fn draw_live(e: &mut Ed) {
    let live = shown(e);
    if let Some(o) = live.as_ref().filter(|o| matches!(o.shape, Shape::Spotlight)) {
        let mut all = e.objs.clone();
        all.insert(e.live_at.unwrap_or(all.len()).min(all.len()), o.clone());
        e.work = annot::render(&e.base, &all, e.scale);
        e.live_box = Some((0.0, 0.0, e.base.w as f32, e.base.h as f32));
        to_disp(&e.work, &mut e.disp, None);
        return;
    }
    let new_box = live.as_ref().map(|o| o.bounds(e.scale));
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
    if let Some(o) = &live {
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
    if e.editing_text {
        // Keep a syllable still being composed (click elsewhere / Esc mid-composition).
        let comp = std::mem::take(&mut e.comp);
        if let Some(Obj { shape: Shape::Text(t), .. }) = e.live.as_mut() {
            t.push_str(&comp);
        }
        e.editing_text = false;
        unsafe {
            let h = ImmGetContext(e.hwnd);
            if !h.is_null() {
                ImmNotifyIME(h, NI_COMPOSITIONSTR, CPS_CANCEL, 0);
                ImmReleaseContext(e.hwnd, h);
            }
            set_ime(e.hwnd, false);
        }
    }
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
        // Only moved/re-edited objects stay selected: a fresh drawing must not make the next
        // Esc "deselect" instead of finishing (Shottr: Esc = copy & close).
        e.sel = at.map(|_| i);
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

/// Single-key tools need raw keys: with the Korean IME in 한글 mode, letters arrive as
/// VK_PROCESSKEY. The IME is detached except while a text object is being typed.
unsafe fn set_ime(hwnd: HWND, on: bool) {
    ImmAssociateContextEx(hwnd, null_mut(), if on { IACE_DEFAULT } else { 0 });
}

fn start_text(e: &mut Ed) {
    e.editing_text = true;
    unsafe { set_ime(e.hwnd, true) };
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

unsafe fn shade_outside(e: &Ed, dc: HDC, c: (f32, f32, f32, f32), a: u8) -> RECT {
    let (x0, y0) = to_client(e, c.0.min(c.2), c.1.min(c.3));
    let (x1, y1) = to_client(e, c.0.max(c.2), c.1.max(c.3));
    let (ix0, iy0) = to_client(e, 0.0, 0.0);
    let (ix1, iy1) = to_client(e, e.base.w as f32, e.base.h as f32);
    shade(dc, RECT { left: ix0, top: iy0, right: ix1, bottom: y0 }, a);
    shade(dc, RECT { left: ix0, top: y1, right: ix1, bottom: iy1 }, a);
    shade(dc, RECT { left: ix0, top: y0, right: x0, bottom: y1 }, a);
    shade(dc, RECT { left: x1, top: y0, right: ix1, bottom: y1 }, a);
    RECT { left: x0, top: y0, right: x1, bottom: y1 }
}

unsafe fn paint_crop(e: &Ed, dc: HDC) {
    if let Some(c) = e.crop {
        let r = shade_outside(e, dc, c, 150);
        frame(dc, RECT { left: r.left - 1, top: r.top - 1, right: r.right + 1, bottom: r.bottom + 1 }, 0xFFFFFF, PS_SOLID, 1);
    }
    if let Some(c) = e.area {
        let r = shade_outside(e, dc, c, 90);
        let r = RECT { left: r.left - 1, top: r.top - 1, right: r.right + 1, bottom: r.bottom + 1 };
        frame(dc, r, 0xFFFFFF, PS_SOLID, 1);
        frame(dc, r, 0x3DA5FF, PS_DOT, 1);
        // Size label above the selection (Shottr shows it next to the cutter).
        let (w, h) = area_size(e, c);
        let label = format!(" {w} × {h} ");
        let f = ui_font((12.0 * dpi(e)) as i32, true);
        let of = SelectObject(dc, f);
        let tw = text_w(dc, &label);
        let th = (18.0 * dpi(e)) as i32;
        let top = if r.top - th - 4 >= canvas(e).top { r.top - th - 4 } else { r.bottom + 4 };
        let lr = RECT { left: r.left, top, right: r.left + tw, bottom: top + th };
        fill(dc, &lr, 0x1E6FD9);
        draw_text(dc, &label, &lr, 0xFFFFFF, DT_LEFT);
        SelectObject(dc, of);
        DeleteObject(f);
    }
    paint_ruler(e, dc);
}

/// Width × height of an area in the current units.
fn area_size(e: &Ed, c: (f32, f32, f32, f32)) -> (u32, u32) {
    let (w, h) = ((c.2 - c.0).abs(), (c.3 - c.1).abs());
    let k = if e.pt_units { e.scale.0 } else { 1.0 };
    ((w / k).round() as u32, (h / k).round() as u32)
}

/// Ruler (Shottr: hold an arrow / 1 / 2): from the pixel under the cursor out to where the
/// colour changes, horizontally or vertically.
fn ruler_span(e: &Ed, horiz: bool) -> Option<(u32, u32, u32)> {
    let (x, y) = e.hover?;
    let (w, h) = (e.base.w as i64, e.base.h as i64);
    let (x, y) = (x.floor() as i64, y.floor() as i64);
    if x < 0 || y < 0 || x >= w || y >= h {
        return None;
    }
    let px = |x: i64, y: i64| {
        let i = ((y * w + x) * 4) as usize;
        [e.work.px[i] as i32, e.work.px[i + 1] as i32, e.work.px[i + 2] as i32]
    };
    let c0 = px(x, y);
    let same = |x: i64, y: i64| px(x, y).iter().zip(c0).map(|(a, b)| (a - b).abs()).max().unwrap() <= 24;
    let (pos, len, fixed) = if horiz { (x, w, y) } else { (y, h, x) };
    let at = |t: i64| if horiz { same(t, fixed) } else { same(fixed, t) };
    let mut lo = pos;
    while lo > 0 && at(lo - 1) {
        lo -= 1;
    }
    let mut hi = pos;
    while hi + 1 < len && at(hi + 1) {
        hi += 1;
    }
    Some((lo as u32, hi as u32 + 1, fixed as u32))
}

unsafe fn paint_ruler(e: &Ed, dc: HDC) {
    let Some(horiz) = e.ruler else { return };
    let Some((lo, hi, fixed)) = ruler_span(e, horiz) else { return };
    let f = fixed as f32 + 0.5;
    let (a, b) = if horiz { (to_client(e, lo as f32, f), to_client(e, hi as f32, f)) } else { (to_client(e, f, lo as f32), to_client(e, f, hi as f32)) };
    let pen = CreatePen(PS_SOLID, 1, rgb(0xFF3B30));
    let op = SelectObject(dc, pen);
    MoveToEx(dc, a.0, a.1, null_mut());
    LineTo(dc, b.0, b.1);
    let t = 5;
    for p in [a, b] {
        if horiz {
            MoveToEx(dc, p.0, p.1 - t, null_mut());
            LineTo(dc, p.0, p.1 + t + 1);
        } else {
            MoveToEx(dc, p.0 - t, p.1, null_mut());
            LineTo(dc, p.0 + t + 1, p.1);
        }
    }
    SelectObject(dc, op);
    DeleteObject(pen);
    let k = if e.pt_units { e.scale.0 } else { 1.0 };
    let label = format!(" {} {} ", ((hi - lo) as f32 / k).round(), if e.pt_units { "pt" } else { "px" });
    let f = ui_font((12.0 * dpi(e)) as i32, true);
    let of = SelectObject(dc, f);
    let tw = text_w(dc, &label);
    let th = (18.0 * dpi(e)) as i32;
    let (cx, cy) = ((a.0 + b.0) / 2, (a.1 + b.1) / 2);
    let lr = if horiz { RECT { left: cx - tw / 2, top: cy - th - 6, right: cx + tw / 2 + 1, bottom: cy - 6 } } else { RECT { left: cx + 8, top: cy - th / 2, right: cx + 8 + tw, bottom: cy + th / 2 } };
    fill(dc, &lr, 0xFF3B30);
    draw_text(dc, &label, &lr, 0xFFFFFF, DT_LEFT);
    SelectObject(dc, of);
    DeleteObject(f);
}

unsafe fn paint_selection(e: &Ed, dc: HDC) {
    let live = shown(e);
    let target = match (e.sel, &live) {
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
    // Right side first (Done must never be pushed out): undo, save, done.
    let mut rx = cw - pad;
    let narrow = cw < (1500.0 * u) as i32;
    let right: [(&str, Item, bool); 3] = if narrow {
        [("완료", Item::Done, true), ("저장", Item::Save, false), ("되돌리기", Item::Undo, false)]
    } else {
        [("완료 · 복사  Esc", Item::Done, true), ("저장  Ctrl+S", Item::Save, false), ("되돌리기", Item::Undo, false)]
    };
    for (label, item, accent) in right {
        let w = text_w(dc, label) + (20.0 * u) as i32;
        let r = RECT { left: rx - w, top, right: rx, bottom: bot };
        fill(dc, &r, if accent { 0x1E6FD9 } else { 0x3A3A3A });
        draw_text(dc, label, &r, 0xFFFFFF, DT_CENTER);
        e.layout.push((r, item));
        rx -= w + (6.0 * u) as i32;
    }
    let limit = rx - pad;
    // Tools: "K 이름", or just the key letter when the window is too narrow for the names.
    let full: i32 = TOOLS
        .iter()
        .map(|(_, k, n)| {
            SelectObject(dc, fk);
            let kw = text_w(dc, &k.to_string());
            SelectObject(dc, f);
            text_w(dc, n) + kw + (18.0 * u) as i32
        })
        .sum();
    let rest = (8 * 30 + 3 * 30 + 90) as f32 * u; // colours, sizes, tail toggle
    let compact = x + full + rest as i32 > limit;
    for (t, key, name) in TOOLS {
        SelectObject(dc, fk);
        let kw = text_w(dc, &key.to_string());
        SelectObject(dc, f);
        let w = if compact { kw + (16.0 * u) as i32 } else { text_w(dc, name) + kw + (16.0 * u) as i32 };
        let r = RECT { left: x, top, right: x + w, bottom: bot };
        if e.tool == t {
            fill(dc, &r, 0x4A4A4A);
            fill(dc, &RECT { left: r.left, top: r.bottom - (2.0 * u) as i32, right: r.right, bottom: r.bottom }, 0x3DA5FF);
        }
        SelectObject(dc, fk);
        if compact {
            draw_text(dc, &key.to_string(), &r, if e.tool == t { 0xFFFFFF } else { 0xC8C8C8 }, DT_CENTER);
        } else {
            let inner = RECT { left: r.left + (6.0 * u) as i32, ..r };
            draw_text(dc, &key.to_string(), &inner, 0x8A8A8A, DT_LEFT);
            SelectObject(dc, f);
            let inner = RECT { left: inner.left + kw + (4.0 * u) as i32, ..r };
            draw_text(dc, name, &inner, 0xF0F0F0, DT_LEFT);
        }
        SelectObject(dc, f);
        e.layout.push((r, Item::Tool(t)));
        x += w + (2.0 * u) as i32;
    }
    x += (10.0 * u) as i32;
    let cur = if e.tool == Tool::Highlight { e.hl_color } else { e.color };
    let sw = (bot - top) - (6.0 * u) as i32;
    for c in COLORS.iter().copied().chain((e.tool == Tool::Highlight).then_some(HL_DEFAULT)) {
        if x + sw > limit {
            break;
        }
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
        if r.right > limit {
            break;
        }
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
    if let Some(on) = tail_state(e) {
        x += (8.0 * u) as i32;
        let label = if on { "꼬리 켬" } else { "꼬리 끔" };
        let w = text_w(dc, label) + (20.0 * u) as i32;
        let r = RECT { left: x, top, right: x + w, bottom: bot };
        if r.right > limit {
            return finish_toolbar(dc, of, f, fk);
        }
        fill(dc, &r, if on { 0x4A4A4A } else { 0x333333 });
        if on {
            fill(dc, &RECT { left: r.left, top: r.bottom - (2.0 * u) as i32, right: r.right, bottom: r.bottom }, 0x3DA5FF);
        }
        draw_text(dc, label, &r, if on { 0xF0F0F0 } else { 0x8A8A8A }, DT_CENTER);
        e.layout.push((r, Item::Tail));
        x += w;
    }
    let _ = x;
    finish_toolbar(dc, of, f, fk);
}

unsafe fn finish_toolbar(dc: HDC, of: HGDIOBJ, f: HFONT, fk: HFONT) {
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
    let (w, h) = if e.pt_units { ((w as f32 / e.scale.0).round() as u32, (h as f32 / e.scale.0).round() as u32) } else { (w, h) };
    let unit = if e.pt_units { "pt" } else { "px" };
    let mut left = format!("{w} × {h} {unit}   {}%", (e.zoom * 100.0).round());
    if let Some(c) = e.area {
        let (aw, ah) = area_size(e, c);
        left += &format!("   선택 {aw} × {ah}");
    }
    if let Some((x, y)) = e.hover.filter(|&(x, y)| x >= 0.0 && y >= 0.0 && x < e.base.w as f32 && y < e.base.h as f32) {
        left += &format!("   ({}, {})", x as u32, y as u32);
    }
    if !e.status.is_empty() {
        left += "   ";
        left += &e.status;
    }
    let lr = RECT { left: (10.0 * u) as i32, ..r };
    draw_text(dc, &left, &lr, 0xC8C8C8, DT_LEFT);
    let hint = match ctx(e) {
        Ctx::Text => "입력 중 · Enter 끝내기 · Shift+Enter 줄바꿈",
        Ctx::Area => "R 사각형 · O 원 · B 흐림 · S 강조 · M 돋보기 · Del 지우기 · 화살표 이동 · Ctrl+화살표 크기 · [ ] 줄이기/늘리기 · C 평균색 · A 맞춤 · Enter 자르기 · Ctrl+C 복사 · Ctrl+O 텍스트 · Esc 해제",
        Ctx::Object => "Tab 다음 · 화살표 이동 · Ctrl+D 복제 · Ctrl+[ ] 크기 · Ctrl+C/V 복사·붙여넣기 · Del 삭제 · Esc 선택 해제",
        _ => "V 드래그 = 영역 선택 · Tab 색상 · Shift+Tab 가장 어두운 색 · 화살표/1/2 누르고 재기 · Ctrl+O 텍스트 · Ctrl+Z 되돌리기 · Ctrl+1 맞춤 · Ctrl+0 원래 크기",
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
        Tool::Spotlight => Shape::Spotlight,
        Tool::Magnifier => Shape::Magnifier(p),
        Tool::Select | Tool::Text | Tool::Counter => return None,
    })
}

fn set_tool(e: &mut Ed, t: Tool) {
    commit_live(e);
    e.tool = t;
    e.sel = None;
    e.status.clear();
}

/// Tail toggle state shown in the toolbar: the text being edited / selected, else the default
/// for new text (only while the text tool is active).
fn tail_state(e: &Ed) -> Option<bool> {
    let text = |o: &Obj| matches!(o.shape, Shape::Text(_)).then_some(o.tail);
    if e.editing_text {
        return e.live.as_ref().and_then(text);
    }
    if let Some(o) = e.sel.and_then(|i| e.objs.get(i)) {
        return text(o);
    }
    (e.tool == Tool::Text).then_some(e.tail)
}

fn toggle_tail(e: &mut Ed) {
    let Some(on) = tail_state(e).map(|t| !t) else { return };
    e.tail = on;
    let s = e.scale;
    let set = |o: &mut Obj| {
        o.tail = on;
        // A box made by a plain click has its tip inside: give it one below the box.
        if let Some(b) = o.bubble(s).filter(|b| on && b.tail.is_none()) {
            let px = s.font_px(o.size) as f32;
            o.b = (b.x0 + px, b.y1 + px * 1.5);
        }
    };
    if e.editing_text {
        if let Some(o) = e.live.as_mut() {
            set(o);
        }
        draw_live(e);
    } else if let Some(i) = e.sel {
        snapshot(e);
        set(&mut e.objs[i]);
        rebuild(e);
    }
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
        Item::Tail => toggle_tail(e),
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
            let after = click_item(e, item);
            // Hover no longer repaints the toolbar, so show the new tool/colour/size now.
            InvalidateRect(e.hwnd, null(), 0);
            return after;
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
    // Tail tip of a speech bubble: drag it (back into the box removes the tail).
    let tip_tol = 8.0 / e.zoom;
    if let Some(i) = (0..e.objs.len()).rev().find(|&i| {
        let o = &e.objs[i];
        o.bubble(e.scale).is_some_and(|b| b.tail.is_some()) && (o.b.0 - p.0).hypot(o.b.1 - p.1) <= tip_tol
    }) {
        lift(e, i);
        e.drag = Drag::Tip;
        SetCapture(e.hwnd);
        InvalidateRect(e.hwnd, null(), 0);
        return After::Nothing;
    }
    // Existing object under the cursor: select & move (text with the text tool: edit).
    let grabs = |o: &Obj| {
        let same = matches!(
            (&o.shape, e.tool),
            (Shape::Blur, Tool::Blur) | (Shape::Erase, Tool::Erase) | (Shape::Highlight, Tool::Highlight)
        );
        if same || e.tool == Tool::Select { o.hit(e.scale, p.0, p.1, tol) } else { o.hit_edge(e.scale, p.0, p.1, tol) }
    };
    // Select tool: a drag inside the area selection moves it.
    if e.tool == Tool::Select {
        if let Some(c) = e.area.filter(|c| p.0 >= c.0.min(c.2) && p.0 <= c.0.max(c.2) && p.1 >= c.1.min(c.3) && p.1 <= c.1.max(c.3)) {
            e.area = Some(norm(c));
            e.drag = Drag::AreaMove { last: p };
            SetCapture(e.hwnd);
            return After::Nothing;
        }
    }
    e.area = None;
    if let Some(i) = (0..e.objs.len()).rev().find(|&i| grabs(&e.objs[i])) {
        let is_text = matches!(e.objs[i].shape, Shape::Text(_));
        lift(e, i);
        if is_text && e.tool == Tool::Text {
            start_text(e);
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
        Tool::Select => {
            let (w, h) = (e.base.w as f32, e.base.h as f32);
            let q = (p.0.clamp(0.0, w), p.1.clamp(0.0, h));
            e.drag = Drag::Area { start: q };
            e.area = Some((q.0, q.1, q.0, q.1));
            SetCapture(e.hwnd);
        }
        Tool::Text => {
            snapshot(e);
            let px = e.scale.font_px(size);
            let a = (p.0, p.1 - px as f32 * 0.6);
            e.live = Some(Obj { shape: Shape::Text(String::new()), a, b: a, color, size, tail: e.tail });
            e.live_at = None;
            start_text(e);
            draw_live(e);
            // Click = plain text box; drag = speech bubble pointing at where the drag started.
            e.drag = Drag::TextTail { start: p };
            SetCapture(e.hwnd);
        }
        Tool::Counter => {
            snapshot(e);
            let n = e.objs.iter().filter_map(|o| if let Shape::Counter(n) = o.shape { Some(n) } else { None }).max().unwrap_or(0) + 1;
            e.objs.push(Obj { shape: Shape::Counter(n), a: p, b: p, color, size, tail: true });
            rebuild(e);
        }
        _ => {
            if let Some(shape) = new_shape(e, p) {
                snapshot(e);
                e.live = Some(Obj { shape, a: p, b: p, color, size, tail: true });
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
                if let Shape::Magnifier(c) = &mut o.shape {
                    *c = annot::place_lens((o.a.0, o.a.1, o.b.0, o.b.1), e.base.w, e.base.h);
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
        Drag::TextTail { start } => {
            let px = e.scale.font_px(e.live.as_ref().map_or(e.size, |o| o.size)) as f32;
            if let Some(o) = e.live.as_mut() {
                o.a = (p.0, p.1 - px * 0.6);
                o.b = start;
            }
            draw_live(e);
        }
        Drag::Tip => {
            if let Some(o) = e.live.as_mut() {
                o.b = p;
            }
            draw_live(e);
        }
        Drag::Area { start } => {
            let b = constrain(e, start, p);
            let (w, h) = (e.base.w as f32, e.base.h as f32);
            e.area = Some((start.0, start.1, b.0.clamp(0.0, w), b.1.clamp(0.0, h)));
        }
        Drag::AreaMove { last } => {
            if let Some(c) = e.area {
                let (w, h) = (e.base.w as f32, e.base.h as f32);
                let dx = (p.0 - last.0).clamp(-c.0, w - c.2);
                let dy = (p.1 - last.1).clamp(-c.1, h - c.3);
                e.area = Some((c.0 + dx, c.1 + dy, c.2 + dx, c.3 + dy));
                e.drag = Drag::AreaMove { last: (last.0 + dx, last.1 + dy) };
            }
        }
    }
    if e.drag == Drag::None && e.ruler.is_none() {
        // Hover only changes the coordinates in the status bar.
        let mut rc: RECT = zeroed();
        GetClientRect(e.hwnd, &mut rc);
        let (_, sb) = bars(e);
        rc.top = rc.bottom - sb;
        InvalidateRect(e.hwnd, &rc, 0);
    } else {
        InvalidateRect(e.hwnd, null(), 0);
    }
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
        Drag::Tip => {
            // Tip back inside the box: no tail.
            if let Some(o) = e.live.as_mut() {
                if o.bubble(e.scale).is_some_and(|b| b.tail.is_none()) {
                    o.b = o.a;
                }
            }
            commit_live(e);
        }
        Drag::TextTail { .. } => {} // keep typing into the new text box
        Drag::Area { .. } => {
            // A click (no real drag) clears the selection.
            e.area = e.area.filter(|&c| clip_rect(c, e.base.w, e.base.h).is_some()).map(norm);
        }
        _ => {}
    }
    InvalidateRect(e.hwnd, null(), 0);
}

fn norm(c: (f32, f32, f32, f32)) -> (f32, f32, f32, f32) {
    (c.0.min(c.2), c.1.min(c.3), c.0.max(c.2), c.1.max(c.3))
}

fn hex(p: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", p[0], p[1], p[2])
}

unsafe fn copy_hex(e: &mut Ed, p: [u8; 3], what: &str) {
    let h = hex(p);
    e.status = if crate::clip::write_text(e.hwnd, &h) { format!("{what} {h} 복사함") } else { "복사 실패".into() };
}

fn pixel(e: &Ed, x: usize, y: usize) -> [u8; 3] {
    let i = (y * e.base.w as usize + x) * 4;
    [e.work.px[i], e.work.px[i + 1], e.work.px[i + 2]]
}

fn hover_px(e: &Ed) -> Option<(usize, usize)> {
    let (x, y) = e.hover?;
    (x >= 0.0 && y >= 0.0 && x < e.base.w as f32 && y < e.base.h as f32).then_some((x as usize, y as usize))
}

/// Tab: colour under the cursor. Shift+Tab: darkest pixel of the 20×20 box around it (Shottr
/// samples cursor−10 … cursor+10, which picks text colour over anti-aliased edges).
unsafe fn copy_color(e: &mut Ed, darkest: bool) {
    let Some((x, y)) = hover_px(e) else { return };
    if !darkest {
        let p = pixel(e, x, y);
        return copy_hex(e, p, "색상");
    }
    let p = keymap::darkest(&e.work.px, e.base.w as usize, e.base.h as usize, x, y);
    copy_hex(e, p, "가장 어두운 색");
}

fn area_px(e: &Ed) -> Option<(u32, u32, u32, u32)> {
    e.area.and_then(|c| clip_rect(c, e.base.w, e.base.h))
}

/// C with an area: its average colour.
unsafe fn copy_avg(e: &mut Ed) {
    let Some((x0, y0, x1, y1)) = area_px(e) else { return };
    let mut sum = [0u64; 3];
    for y in y0..y1 {
        for x in x0..x1 {
            let p = pixel(e, x as usize, y as usize);
            (0..3).for_each(|k| sum[k] += p[k] as u64);
        }
    }
    let n = ((x1 - x0) * (y1 - y0)) as u64;
    let p = [0, 1, 2].map(|k| ((sum[k] + n / 2) / n) as u8);
    copy_hex(e, p, "평균색");
}

/// A with an area: shrink it to the content (edges that are one flat colour are dropped).
fn auto_adjust(e: &mut Ed) {
    let Some((mut x0, mut y0, mut x1, mut y1)) = area_px(e) else { return };
    let bg = pixel(e, x0 as usize, y0 as usize);
    let flat = |e: &Ed, xs: std::ops::Range<u32>, ys: std::ops::Range<u32>| {
        ys.clone().all(|y| xs.clone().all(|x| pixel(e, x as usize, y as usize).iter().zip(bg).all(|(a, b)| a.abs_diff(b) <= 16)))
    };
    while y1 - y0 > 2 && flat(e, x0..x1, y0..y0 + 1) {
        y0 += 1;
    }
    while y1 - y0 > 2 && flat(e, x0..x1, y1 - 1..y1) {
        y1 -= 1;
    }
    while x1 - x0 > 2 && flat(e, x0..x0 + 1, y0..y1) {
        x0 += 1;
    }
    while x1 - x0 > 2 && flat(e, x1 - 1..x1, y0..y1) {
        x1 -= 1;
    }
    if (x1 - x0, y1 - y0) == (2, 2) || (y1 - y0 <= 2 && flat(e, x0..x1, y0..y1)) {
        e.status = "내용이 없는 영역".into();
        return; // all one colour: keep the selection
    }
    e.area = Some((x0 as f32, y0 as f32, x1 as f32, y1 as f32));
}

/// Pixels of the edited picture inside `r`, as RGBA.
fn region(e: &Ed, (x0, y0, x1, y1): (u32, u32, u32, u32)) -> Rgba {
    let w = (x1 - x0) as usize;
    let mut px = Vec::with_capacity(w * (y1 - y0) as usize * 4);
    for y in y0..y1 {
        let o = (y as usize * e.base.w as usize + x0 as usize) * 4;
        px.extend_from_slice(&e.work.px[o..o + w * 4]);
    }
    Rgba { w: w as u32, h: y1 - y0, px }
}

/// Ctrl+C with an area: copy just that part (the editor stays open).
unsafe fn copy_area(e: &mut Ed) {
    let Some(r) = area_px(e) else { return };
    let img = region(e, r);
    let ok = crate::optimize::encode_png(&img).ok().is_some_and(|png| {
        let dib = crate::optimize::encode_dib(&img);
        let out = crate::clip::Output { id: "editor-area", png: &png, dib: &dib, path: std::path::Path::new(""), with_path: false };
        crate::clip::write(e.hwnd, &out, None)
    });
    e.status = if ok { format!("선택 영역 {} × {} 복사함", img.w, img.h) } else { "복사 실패".into() };
}

/// Ctrl+O: text recognition of the area (or the whole picture) on a worker thread.
unsafe fn start_ocr(e: &mut Ed) {
    let r = area_px(e).unwrap_or((0, 0, e.base.w, e.base.h));
    let img = region(e, r);
    let mut bgra = img.px;
    bgra.chunks_exact_mut(4).for_each(|p| p.swap(0, 2));
    let (w, h, dpi) = (img.w as i32, img.h as i32, (96.0 * e.scale.0) as u32);
    let hwnd = e.hwnd as usize;
    e.status = "텍스트 인식 중…".into();
    std::thread::spawn(move || {
        let r = std::panic::catch_unwind(|| crate::ocr::recognize(w, h, &bgra, dpi)).unwrap_or_else(|_| Err("내부 오류(panic)".into()));
        *OCR_OUT.lock().unwrap() = Some(r);
        unsafe { PostMessageW(hwnd as HWND, WM_EDITOR_OCR, 0, 0) };
    });
}

unsafe fn ocr_done(e: &mut Ed) {
    let Some(r) = OCR_OUT.lock().unwrap().take() else { return };
    e.status = match r {
        Ok(t) if t.trim().is_empty() => "인식된 텍스트가 없습니다".into(),
        Ok(t) if crate::clip::write_text(e.hwnd, &t) => format!("텍스트 {}줄 {}자 복사함", t.lines().count(), t.chars().count()),
        Ok(_) => "복사 실패".into(),
        Err(err) => format!("텍스트 인식 실패: {err}"),
    };
}

/// Turns the area selection into an object (Shottr: R / O / B / S / M / Delete on a selection).
fn area_into(e: &mut Ed, t: Tool) {
    let Some(c) = e.area.take().map(norm) else { return };
    let shape = match t {
        Tool::Rect => Shape::Rect,
        Tool::Oval => Shape::Oval,
        Tool::Blur => Shape::Blur,
        Tool::Erase => Shape::Erase,
        Tool::Spotlight => Shape::Spotlight,
        Tool::Magnifier => Shape::Magnifier(annot::place_lens(c, e.base.w, e.base.h)),
        _ => return,
    };
    let o = Obj { shape, a: (c.0, c.1), b: (c.2, c.3), color: e.color, size: e.size, tail: true };
    if o.degenerate() {
        return;
    }
    snapshot(e);
    e.objs.push(o);
    e.sel = None;
    rebuild(e);
}

unsafe fn zoom_to(e: &mut Ed, r: (f32, f32, f32, f32)) {
    let r = norm(r);
    let (rw, rh) = ((r.2 - r.0).max(1.0), (r.3 - r.1).max(1.0));
    let c = canvas(e);
    let (cw, ch) = ((c.right - c.left) as f32, (c.bottom - c.top) as f32);
    e.zoom = (cw / rw).min(ch / rh).mul_add(0.9, 0.0).clamp(0.05, 16.0);
    e.ox = c.left as f32 + cw / 2.0 - (r.0 + rw / 2.0) * e.zoom;
    e.oy = c.top as f32 + ch / 2.0 - (r.1 + rh / 2.0) * e.zoom;
    e.fit = false;
    clamp_view(e, false);
}

fn ctx(e: &Ed) -> Ctx {
    if e.editing_text {
        Ctx::Text
    } else if matches!(e.drag, Drag::Draw | Drag::TextTail { .. }) {
        Ctx::Drawing
    } else if e.area.is_some() {
        Ctx::Area
    } else if e.sel.is_some() {
        Ctx::Object
    } else {
        Ctx::None
    }
}

/// Esc during a move / area drag / pan: put things back.
unsafe fn cancel_drag(e: &mut Ed) {
    let drag = std::mem::replace(&mut e.drag, Drag::None);
    ReleaseCapture();
    match drag {
        Drag::Area { .. } => e.area = None,
        Drag::Move { .. } | Drag::Tip => commit_live(e),
        _ => {}
    }
}

unsafe fn on_key(e: &mut Ed, vk: u32) -> After {
    let m = Mods {
        shift: GetKeyState(VK_SHIFT as i32) < 0,
        ctrl: GetKeyState(VK_CONTROL as i32) < 0,
        alt: GetKeyState(VK_MENU as i32) < 0,
    };
    let key = Key::from_vk(vk);
    if key == Key::Esc && !matches!(e.drag, Drag::None | Drag::Draw | Drag::TextTail { .. }) {
        cancel_drag(e);
        InvalidateRect(e.hwnd, null(), 0);
        return After::Nothing;
    }
    let act = keymap::action(key, m, ctx(e));
    let after = run(e, act);
    InvalidateRect(e.hwnd, null(), 0);
    after
}

unsafe fn run(e: &mut Ed, act: Act) -> After {
    let unit = if e.pt_units { e.scale.0 } else { 1.0 };
    let (w, h) = (e.base.w as f32, e.base.h as f32);
    match act {
        Act::Tool(t) => set_tool(e, t),
        Act::AreaInto(t) => area_into(e, t),
        Act::AreaErase => area_into(e, Tool::Erase),
        Act::AreaMove(..) | Act::AreaResize(..) | Act::AreaGrow(_) => {
            if let Some(c) = e.area {
                e.area = Some(keymap::area_apply(c, act, unit, w, h));
            }
        }
        Act::AreaAvgColor => copy_avg(e),
        Act::AreaAutoAdjust => auto_adjust(e),
        Act::AreaCrop => {
            if let Some(c) = e.area.take().filter(|&c| clip_rect(c, e.base.w, e.base.h).is_some()) {
                snapshot(e);
                e.crop = Some(norm(c));
            }
        }
        Act::AreaClear => e.area = None,
        Act::AreaCopy => copy_area(e),
        Act::AreaOcr | Act::Ocr => start_ocr(e),
        Act::ColorPixel => copy_color(e, false),
        Act::ColorDarkest => copy_color(e, true),
        Act::ObjNext => {
            if !e.objs.is_empty() {
                e.sel = Some(e.sel.map_or(0, |i| (i + 1) % e.objs.len()));
            }
        }
        Act::ObjDelete => {
            if let Some(i) = e.sel.take() {
                snapshot(e);
                e.objs.remove(i);
                rebuild(e);
            }
        }
        Act::ObjDuplicate => {
            if let Some(mut o) = e.sel.and_then(|i| e.objs.get(i)).cloned() {
                snapshot(e);
                o.translate(10.0, 10.0);
                e.objs.push(o);
                e.sel = Some(e.objs.len() - 1);
                rebuild(e);
            }
        }
        Act::ObjSize(d) => {
            if let Some(i) = e.sel {
                let s = (e.objs[i].size as i8 + d).clamp(0, 2) as u8;
                if s != e.objs[i].size {
                    snapshot(e);
                    e.objs[i].size = s;
                    e.size = s;
                    rebuild(e);
                }
            }
        }
        Act::ObjCopy => {
            if let Some(o) = e.sel.and_then(|i| e.objs.get(i)).cloned() {
                OBJ_CLIP.with(|c| *c.borrow_mut() = Some(o));
                e.status = "개체 복사함 (Ctrl+V 붙여넣기)".into();
            }
        }
        Act::ObjPaste => {
            let Some(mut o) = OBJ_CLIP.with(|c| c.borrow().clone()) else { return After::Nothing };
            o.translate(10.0, 10.0);
            OBJ_CLIP.with(|c| *c.borrow_mut() = Some(o.clone())); // next paste lands further on
            commit_live(e);
            snapshot(e);
            e.objs.push(o);
            e.sel = Some(e.objs.len() - 1);
            e.area = None;
            rebuild(e);
        }
        Act::ObjNudge(dx, dy) => {
            if let Some(i) = e.sel {
                snapshot(e);
                e.objs[i].translate(dx as f32 * unit, dy as f32 * unit);
                rebuild(e);
            }
        }
        Act::ObjDeselect => e.sel = None,
        Act::CopyImage | Act::Finish => return After::Finish,
        Act::DrawFinish => {
            if e.drag == Drag::Draw {
                on_up(e);
            }
        }
        Act::TextCommit => commit_live(e),
        Act::TextInput | Act::Nothing => {}
        Act::Undo => undo(e, false),
        Act::Redo => undo(e, true),
        Act::Save | Act::SaveAs => return After::SaveAs,
        Act::Units => {
            e.pt_units = !e.pt_units;
            e.status = if e.pt_units { "단위: pt (화면 배율 반영)".into() } else { "단위: px".into() };
        }
        Act::Pin | Act::AddCapture | Act::Upload | Act::Rasterize | Act::Backdrop => {
            e.status = "ShotClip에서 아직 지원하지 않는 Shottr 기능".into();
        }
        Act::ZoomIn => zoom_step(e, true),
        Act::ZoomOut => zoom_step(e, false),
        Act::ZoomFit => fit_view(e),
        Act::Zoom100 => zoom_at_center(e, 1.0),
        Act::ZoomSelection => {
            let r = e.area.or_else(|| e.sel.and_then(|i| e.objs.get(i)).map(|o| o.bounds(e.scale)));
            if let Some(r) = r {
                zoom_to(e, r);
            }
        }
        Act::CornerTopLeft | Act::CornerBottomRight => {
            if let Some(p) = hover_px(e).map(|(x, y)| (x as f32, y as f32)) {
                let c = e.area.map(norm).unwrap_or((p.0, p.1, p.0, p.1));
                e.area = Some(if act == Act::CornerTopLeft { (p.0, p.1, c.2.max(p.0), c.3.max(p.1)) } else { (c.0.min(p.0), c.1.min(p.1), p.0 + 1.0, p.1 + 1.0) });
                e.sel = None;
            }
        }
        Act::Ruler(horiz) => e.ruler = Some(horiz),
        Act::Pan => e.space = true,
    }
    After::Nothing
}

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

unsafe fn ime_string(h: HIMC, kind: IME_COMPOSITION_STRING) -> String {
    let n = ImmGetCompositionStringW(h, kind, null_mut(), 0);
    if n <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; n as usize / 2];
    ImmGetCompositionStringW(h, kind, buf.as_mut_ptr().cast(), n as u32);
    String::from_utf16_lossy(&buf)
}

unsafe fn on_ime(e: &mut Ed, lp: LPARAM) {
    let h = ImmGetContext(e.hwnd);
    if h.is_null() {
        return;
    }
    let flags = lp as u32;
    let result = if flags & GCS_RESULTSTR != 0 { ime_string(h, GCS_RESULTSTR) } else { String::new() };
    e.comp = if flags & GCS_COMPSTR != 0 { ime_string(h, GCS_COMPSTR) } else { String::new() };
    ImmReleaseContext(e.hwnd, h);
    if !e.editing_text {
        e.comp.clear();
        return;
    }
    if let Some(Obj { shape: Shape::Text(t), .. }) = e.live.as_mut() {
        t.push_str(&result);
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
                    start_text(e);
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
            // Alt+letter (Shottr: Opt+tool key) comes as a system key; Alt+F4 etc. stay default.
            WM_SYSKEYDOWN if (0x41..=0x5A).contains(&(wp as u32)) => on_key(e, wp as u32),
            WM_KEYUP => {
                if wp == 0x20 {
                    e.space = false;
                }
                if matches!(wp, 0x25..=0x28 | 0x31 | 0x32 | 0x61 | 0x62) && e.ruler.take().is_some() {
                    InvalidateRect(hwnd, null(), 0);
                }
                After::Nothing
            }
            WM_EDITOR_OCR => {
                ocr_done(e);
                InvalidateRect(hwnd, null(), 0);
                After::Nothing
            }
            WM_CHAR => {
                on_char(e, wp as u16);
                After::Nothing
            }
            // Draw the composition inline (no IME window at the screen corner); the finished
            // syllable is taken from GCS_RESULTSTR, so no WM_IME_CHAR/WM_CHAR follows.
            WM_IME_STARTCOMPOSITION => After::Nothing,
            WM_IME_COMPOSITION => {
                on_ime(e, lp);
                After::Nothing
            }
            WM_IME_ENDCOMPOSITION => {
                if !e.comp.is_empty() {
                    e.comp.clear();
                    draw_live(e);
                    InvalidateRect(hwnd, null(), 0);
                }
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
