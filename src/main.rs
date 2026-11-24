//! ShotClip: screenshot tool for Windows.
//!
//! - Alt+Shift+3 / Alt+Shift+4: macOS-style capture (see capture.rs). Results go through the
//!   same optimiser, onto the clipboard and (optionally) to the Desktop like macOS, then the
//!   post-capture editor opens (editor.rs, like Shottr); edits are re-exported on close.
//! - Alt+Shift+5 scrolling capture, 6 OCR, 7 colour picker, 8 pixel ruler (Shottr-style tools,
//!   see overlay.rs / scroll.rs / ocr.rs).
//! - In-app updates from GitHub Releases (updater.rs).
//! - Watches the clipboard; when a plain image arrives (e.g. Win+Shift+S), it quantises it
//!   like Clop (pngquant engine), saves it to %LOCALAPPDATA%\ShotClip\images and puts
//!   PNG + CF_DIB (+ file + path text) back on the clipboard.
//! - For 30 s after a result: Ctrl+Shift+- downscales (100→75→50→40…%), Ctrl+Shift+Z restores
//!   the original. Like Clop, the keys are only grabbed while a result is "live".
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod annot;
mod capture;
mod clip;
mod editor;
mod ocr;
mod optimize;
mod overlay;
mod scroll;
mod store;
mod updater;

use clip::wide;
use optimize as opt;
use std::{
    cell::RefCell,
    collections::VecDeque,
    fs,
    mem::{size_of, zeroed},
    path::PathBuf,
    ptr::{null, null_mut},
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc, Mutex,
    },
    thread,
};
use windows_sys::Win32::{
    Foundation::*,
    System::{DataExchange::AddClipboardFormatListener, LibraryLoader::GetModuleHandleW, Threading::{CreateMutexW, GetCurrentProcess, SetProcessWorkingSetSize}},
    UI::{HiDpi::*, Input::KeyboardAndMouse::*, Shell::*, WindowsAndMessaging::*},
};

const WM_TRAY: u32 = WM_APP + 1;
const WM_DONE: u32 = WM_APP + 2;
const WM_OCR: u32 = WM_APP + 3;
/// wParam 1 = checked from the menu (report "up to date" / errors too).
const WM_UPDATE_CHECKED: u32 = WM_APP + 4;
const WM_UPDATE_INSTALLED: u32 = WM_APP + 5;
const TIMER_UPDATE: usize = 4;
const UPDATE_EVERY_MS: u32 = 12 * 3600 * 1000;
const TIMER_DEBOUNCE: usize = 1;
const TIMER_HOTKEYS: usize = 2;
const TIMER_CLEANUP: usize = 3;
const HK_DOWNSCALE: i32 = 1;
const HK_RESTORE: i32 = 2;
const HOTKEY_WINDOW_MS: u32 = 30_000;
const DEBOUNCE_MS: u32 = 150;
const VK_MINUS: u32 = 0xBD;
const VK_Z: u32 = 0x5A;
const HK_CAPTURE_FULL: i32 = 10;
const HK_CAPTURE_AREA: i32 = 11;
const HK_SCROLL: i32 = 12;
const HK_OCR: i32 = 13;
const HK_COLOR: i32 = 14;
const HK_RULER: i32 = 15;

static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
static RESULTS: Mutex<VecDeque<Result<Done, String>>> = Mutex::new(VecDeque::new());
static OCR_RESULTS: Mutex<VecDeque<Result<String, String>>> = Mutex::new(VecDeque::new());
static UPDATE_RESULT: Mutex<Option<Result<Option<updater::Update>, String>>> = Mutex::new(None);
static INSTALL_RESULT: Mutex<Option<Result<(), String>>> = Mutex::new(None);
/// The balloon on screen is the "new version" one (clicking it installs).
static UPDATE_BALLOON: AtomicBool = AtomicBool::new(false);

enum Job {
    Optimise { seq: u32, src: clip::Source },
    /// `limit`: size of the optimised full-size result (what the downscale must beat).
    Scale { id: String, original: PathBuf, factor: f64, limit: usize },
    Restore { id: String, original: PathBuf },
    Capture { img: opt::Rgba, desktop: Option<PathBuf>, clip: bool },
}

#[derive(PartialEq)]
enum Kind {
    Optimised,
    Scaled,
    Restored,
    Captured,
    /// Downscale would only grow the file: the clipboard keeps the current image.
    Kept,
}

struct Done {
    kind: Kind,
    seq: Option<u32>,
    id: String,
    original: PathBuf,
    factor: f64,
    png: Vec<u8>,
    dib: Vec<u8>,
    path: PathBuf,
    before: usize,
    w: u32,
    h: u32,
    kept_original: bool,
    /// Put the result on the clipboard (false for extra monitors of a full-screen capture).
    clip: bool,
    desktop: Option<PathBuf>,
}

struct Last {
    id: String,
    original: PathBuf,
    factor: f64,
    /// Bytes of the optimised full-size result.
    size: usize,
}

struct App {
    jobs: mpsc::Sender<Job>,
    paused: bool,
    copy_path: bool,
    save_desktop: bool,
    open_editor: bool,
    hotkeys: bool,
    update: Option<updater::Update>,
    installing: bool,
    last: Option<Last>,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Short borrows only: never call anything that may re-enter the window procedure inside `f`.
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok()?.as_mut().map(f))
}

// ---------------------------------------------------------------- worker

fn worker(rx: mpsc::Receiver<Job>, hwnd: usize) {
    for job in rx {
        let r = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(job))) {
            Ok(Ok(d)) => Ok(d),
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err("내부 오류(panic)".to_string()),
        };
        RESULTS.lock().unwrap().push_back(r);
        unsafe { PostMessageW(hwnd as HWND, WM_DONE, 0, 0) };
    }
}

fn run(job: Job) -> opt::Result<Done> {
    match job {
        Job::Optimise { seq, src } => {
            let id = store::timestamp();
            let (img, orig_png) = match src {
                clip::Source::Png(b) => (opt::decode_png(&b)?, b),
                clip::Source::Dib(b) => {
                    let img = opt::decode_dib(&b)?;
                    let png = opt::encode_png(&img)?;
                    (img, png)
                }
            };
            let original = store::originals_dir().join(format!("screenshot_{id}.png"));
            fs::write(&original, &orig_png)?;
            let path = store::images_dir().join(format!("screenshot_{id}.png"));
            let before = orig_png.len();
            compress(Kind::Optimised, Some(seq), id, original, 1.0, img, orig_png, before, path)
        }
        Job::Scale { id, original, factor, limit } => {
            let orig_png = fs::read(&original)?;
            let src = opt::decode_png(&orig_png)?;
            let img = opt::resize(&src, factor)?;
            let baseline = opt::encode_png(&img)?;
            let pct = (factor * 100.0).round() as u32;
            let path = store::images_dir().join(format!("screenshot_{id}_{pct}pct.png"));
            let mut d = compress(Kind::Scaled, None, id, original, factor, img, baseline, orig_png.len(), path)?;
            // Clop (downscaledPNGUnderOriginal): an optimiser should never grow a file. A downscaled
            // flat screenshot can, because resampling adds anti-aliased colours; re-quantise toward
            // the original's colour count, or keep what is on the clipboard now.
            if d.png.len() > limit {
                let small = opt::resize(&src, factor)?;
                match opt::shrink_below(&small, &src, limit) {
                    Some((png, pix)) => {
                        fs::write(&d.path, &png)?;
                        d.dib = opt::encode_dib(&pix);
                        d.png = png;
                        d.kept_original = false;
                    }
                    None => d.kind = Kind::Kept,
                }
            }
            Ok(d)
        }
        Job::Restore { id, original } => {
            let png = fs::read(&original)?;
            let img = opt::decode_png(&png)?;
            Ok(Done {
                kind: Kind::Restored,
                seq: None,
                id,
                path: original.clone(),
                original,
                factor: 1.0,
                before: png.len(),
                dib: opt::encode_dib(&img),
                w: img.w,
                h: img.h,
                png,
                kept_original: true,
                clip: true,
                desktop: None,
            })
        }
        Job::Capture { img, desktop, clip } => {
            let id = store::timestamp();
            let orig_png = opt::encode_png(&img)?;
            let original = store::originals_dir().join(format!("screenshot_{id}.png"));
            fs::write(&original, &orig_png)?;
            let path = store::images_dir().join(format!("screenshot_{id}.png"));
            let before = orig_png.len();
            let mut d = compress(Kind::Captured, None, id, original, 1.0, img, orig_png, before, path)?;
            if let Some(dst) = &desktop {
                fs::write(dst, &d.png)?;
            }
            d.clip = clip;
            d.desktop = desktop;
            Ok(d)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn compress(
    kind: Kind,
    seq: Option<u32>,
    id: String,
    original: PathBuf,
    factor: f64,
    img: opt::Rgba,
    baseline: Vec<u8>,
    before: usize,
    path: PathBuf,
) -> opt::Result<Done> {
    let (q, qimg) = opt::quantize_png(&img)?;
    // Clop: if the optimised PNG is bigger, keep the original.
    let (png, pix, kept_original) =
        if q.len() < baseline.len() { (q, qimg, false) } else { (baseline, img, true) };
    fs::write(&path, &png)?;
    Ok(Done {
        kind,
        seq,
        id,
        original,
        factor,
        before,
        dib: opt::encode_dib(&pix),
        w: pix.w,
        h: pix.h,
        png,
        path,
        kept_original,
        clip: true,
        desktop: None,
    })
}

// ---------------------------------------------------------------- tray

fn copy_w(dst: &mut [u16], s: &str) {
    let v: Vec<u16> = s.encode_utf16().take(dst.len() - 1).collect();
    dst[..v.len()].copy_from_slice(&v);
    dst[v.len()] = 0;
}

unsafe fn nid(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut n: NOTIFYICONDATAW = zeroed();
    n.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    n.hWnd = hwnd;
    n.uID = 1;
    n
}

fn tip(paused: bool) -> &'static str {
    if paused { "ShotClip (최적화 일시정지) · Alt+Shift+3~8" } else { "ShotClip · Alt+Shift+3 전체 4 영역 5 스크롤 6 OCR 7 색상 8 자" }
}

/// Icon resource 1 (embedded by build.rs) at the given size, falling back to the stock icon.
unsafe fn app_icon(size: i32) -> HICON {
    let h = LoadImageW(GetModuleHandleW(null()), 1 as _, IMAGE_ICON, size, size, LR_DEFAULTCOLOR);
    if h.is_null() { LoadIconW(null_mut(), IDI_APPLICATION) } else { h as HICON }
}

unsafe fn tray_add(hwnd: HWND) {
    let paused = with_app(|a| a.paused).unwrap_or(false);
    let mut n = nid(hwnd);
    n.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    n.uCallbackMessage = WM_TRAY;
    n.hIcon = app_icon(GetSystemMetrics(SM_CXSMICON));
    copy_w(&mut n.szTip, tip(paused));
    Shell_NotifyIconW(NIM_ADD, &n);
}

unsafe fn tray_tip(hwnd: HWND, paused: bool) {
    let mut n = nid(hwnd);
    n.uFlags = NIF_TIP;
    copy_w(&mut n.szTip, tip(paused));
    Shell_NotifyIconW(NIM_MODIFY, &n);
}

unsafe fn notify(hwnd: HWND, title: &str, body: &str) {
    UPDATE_BALLOON.store(false, Ordering::Relaxed);
    let mut n = nid(hwnd);
    n.uFlags = NIF_INFO;
    copy_w(&mut n.szInfoTitle, title);
    copy_w(&mut n.szInfo, body);
    n.dwInfoFlags = NIIF_INFO | NIIF_NOSOUND;
    Shell_NotifyIconW(NIM_MODIFY, &n);
}

unsafe fn show_menu(hwnd: HWND) {
    let Some((paused, copy_path, save_desktop, open_editor)) =
        with_app(|a| (a.paused, a.copy_path, a.save_desktop, a.open_editor))
    else {
        return;
    };
    let auto = store::autostart_enabled();
    let m = CreatePopupMenu();
    let item = |id: usize, text: &str, checked: bool| {
        let flags = MF_STRING | if checked { MF_CHECKED } else { MF_UNCHECKED };
        AppendMenuW(m, flags, id, wide(text).as_ptr());
    };
    item(1, "일시정지", paused);
    item(2, "파일 경로도 복사 (Clop 기본값)", copy_path);
    item(6, "캡처를 바탕화면에도 저장 (Alt+Shift+3/4)", save_desktop);
    item(8, "캡처 후 편집기 열기", open_editor);
    item(3, "Windows 시작 시 실행", auto);
    AppendMenuW(m, MF_SEPARATOR, 0, null());
    match with_app(|a| a.update.as_ref().map(|u| u.version.clone())).flatten() {
        Some(v) => item(7, &format!("업데이트하고 다시 켜기 ({} → {v})", updater::VERSION), false),
        None => item(7, &format!("업데이트 확인 (현재 {})", updater::VERSION), false),
    }
    item(4, "작업 폴더 열기", false);
    item(5, "종료", false);

    let mut pt: POINT = zeroed();
    GetCursorPos(&mut pt);
    SetForegroundWindow(hwnd);
    let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, pt.x, pt.y, 0, hwnd, null());
    DestroyMenu(m);

    match cmd {
        1 => {
            let p = with_app(|a| {
                a.paused = !a.paused;
                a.paused
            })
            .unwrap_or(false);
            store::set_bool("Paused", p);
            tray_tip(hwnd, p);
        }
        2 => {
            let c = with_app(|a| {
                a.copy_path = !a.copy_path;
                a.copy_path
            })
            .unwrap_or(true);
            store::set_bool("CopyPath", c);
        }
        6 => {
            with_app(|a| a.save_desktop = !save_desktop);
            store::set_bool("SaveToDesktop", !save_desktop);
        }
        8 => {
            with_app(|a| a.open_editor = !open_editor);
            store::set_bool("OpenEditor", !open_editor);
        }
        3 => store::set_autostart(!auto),
        7 => {
            if with_app(|a| a.update.is_some()).unwrap_or(false) {
                start_install(hwnd);
            } else {
                start_check(hwnd, true);
            }
        }
        4 => {
            let dir = wide(&store::workdir().to_string_lossy());
            ShellExecuteW(hwnd, wide("open").as_ptr(), dir.as_ptr(), null(), null(), SW_SHOWNORMAL);
        }
        5 => {
            DestroyWindow(hwnd);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------- capture

/// Called by capture.rs with the captured image(s); `clip_idx` is the one for the clipboard.
pub unsafe fn on_capture(_hwnd: HWND, imgs: Vec<opt::Rgba>, clip_idx: usize) {
    let Some((tx, save, open_editor)) = with_app(|a| (a.jobs.clone(), a.save_desktop, a.open_editor)) else { return };
    let (dir, base, many) = (capture::desktop_dir(), capture::base_name(), imgs.len() > 1);
    let mut edit = None;
    for (i, img) in imgs.into_iter().enumerate() {
        let name = if many { format!("{base} ({})", i + 1) } else { base.clone() };
        let desktop = save.then(|| capture::unique(&dir, &name));
        if open_editor && i == clip_idx {
            let copy = opt::Rgba { w: img.w, h: img.h, px: img.px.clone() };
            edit = Some((copy, name, desktop.clone()));
        }
        let _ = tx.send(Job::Capture { img, desktop, clip: i == clip_idx });
    }
    // Like Shottr: the original is already on its way to the clipboard/Desktop; the editor
    // re-exports to the same file only if something was changed.
    if let Some((img, name, desktop)) = edit {
        // Ctrl+Shift+Z / Ctrl+Shift+- of an earlier result must not stay grabbed globally while
        // the editor (with its own Ctrl+Shift+Z = redo) is open.
        set_hotkeys(_hwnd, false);
        let tx = tx.clone();
        editor::open(
            img,
            &name,
            false,
            Box::new(move |img| {
                let _ = tx.send(Job::Capture { img, desktop: desktop.clone(), clip: true });
            }),
        );
    }
}

/// Text result (colour, measurement): clipboard + notification.
pub unsafe fn on_text(hwnd: HWND, title: &str, text: &str) {
    set_hotkeys(hwnd, false);
    if clip::write_text(hwnd, text) {
        notify(hwnd, title, "클립보드에 복사했습니다");
    }
}

/// Runs OCR on a worker thread (WinRT calls block); the result comes back as WM_OCR.
pub(crate) unsafe fn on_ocr(hwnd: HWND, img: capture::Img, dpi: u32) {
    let h = hwnd as usize;
    thread::spawn(move || {
        let r = std::panic::catch_unwind(|| ocr::recognize(img.w, img.h, &img.px, dpi))
            .unwrap_or_else(|_| Err("내부 오류(panic)".into()));
        OCR_RESULTS.lock().unwrap().push_back(r);
        unsafe { PostMessageW(h as HWND, WM_OCR, 0, 0) };
    });
}

unsafe fn on_ocr_done(hwnd: HWND) {
    let Some(r) = OCR_RESULTS.lock().unwrap().pop_front() else { return };
    match r {
        Ok(t) if t.trim().is_empty() => notify(hwnd, "텍스트 인식", "인식된 텍스트가 없습니다"),
        Ok(t) => {
            set_hotkeys(hwnd, false);
            if clip::write_text(hwnd, &t) {
                let preview: String = t.chars().take(120).collect();
                let n = t.lines().count();
                notify(hwnd, &format!("텍스트 복사 · {n}줄 {}자", t.chars().count()), &preview);
            }
        }
        Err(e) => notify(hwnd, "텍스트 인식 실패", &e),
    }
}

/// Alt+Shift+3..8. While a scroll capture runs, any of them ends it (5 saves, others cancel).
unsafe fn on_tool(hwnd: HWND, id: i32) {
    if scroll::active() {
        scroll::finish(id == HK_SCROLL);
        return;
    }
    if id != HK_CAPTURE_FULL {
        // The focused app saw Alt down/up with our key swallowed; see capture_full.
        keybd_event(0xE8, 0, 0, 0);
        keybd_event(0xE8, 0, KEYEVENTF_KEYUP, 0);
    }
    match id {
        HK_CAPTURE_FULL => capture::capture_full(hwnd),
        HK_CAPTURE_AREA => overlay::start(hwnd, overlay::Mode::Area),
        HK_SCROLL => overlay::start(hwnd, overlay::Mode::Scroll),
        HK_OCR => overlay::start(hwnd, overlay::Mode::Ocr),
        HK_COLOR => overlay::start(hwnd, overlay::Mode::Color),
        _ => overlay::start(hwnd, overlay::Mode::Ruler),
    }
}

// ---------------------------------------------------------------- update

unsafe fn start_check(hwnd: HWND, manual: bool) {
    let h = hwnd as usize;
    thread::spawn(move || {
        *UPDATE_RESULT.lock().unwrap() = Some(updater::check());
        unsafe { PostMessageW(h as HWND, WM_UPDATE_CHECKED, manual as usize, 0) };
    });
}

unsafe fn on_update_checked(hwnd: HWND, manual: bool) {
    let Some(r) = UPDATE_RESULT.lock().unwrap().take() else { return };
    match r {
        Ok(Some(u)) => {
            let v = u.version.clone();
            let known = with_app(|a| a.update.replace(u).is_some_and(|old| old.version == v)).unwrap_or(false);
            if manual || !known {
                notify(hwnd, &format!("ShotClip {v} 새 버전"), "이 알림을 누르거나 트레이 메뉴에서 \"업데이트하고 다시 켜기\"");
                UPDATE_BALLOON.store(true, Ordering::Relaxed);
            }
        }
        Ok(None) if manual => notify(hwnd, "ShotClip", &format!("최신 버전({})입니다", updater::VERSION)),
        Err(e) if manual => notify(hwnd, "업데이트 확인 실패", &e),
        _ => {}
    }
}

/// Like sori: refuses while something is in progress instead of interrupting it.
unsafe fn start_install(hwnd: HWND) {
    let Some(Some(u)) = with_app(|a| if a.installing { None } else { a.update.clone() }) else { return };
    if overlay::is_open() || scroll::active() || editor::is_open() {
        return notify(hwnd, "업데이트 대기", "진행 중인 캡처를 끝낸 뒤 다시 눌러 주세요");
    }
    with_app(|a| a.installing = true);
    notify(hwnd, &format!("ShotClip {} 받는 중", u.version), "끝나면 자동으로 다시 켜집니다");
    let h = hwnd as usize;
    thread::spawn(move || {
        *INSTALL_RESULT.lock().unwrap() = Some(updater::install(&u));
        unsafe { PostMessageW(h as HWND, WM_UPDATE_INSTALLED, 0, 0) };
    });
}

unsafe fn on_update_installed(hwnd: HWND) {
    with_app(|a| a.installing = false);
    match INSTALL_RESULT.lock().unwrap().take() {
        Some(Ok(())) => {
            DestroyWindow(hwnd); // the new exe is already waiting for us to exit
        }
        Some(Err(e)) => notify(hwnd, "업데이트하지 못했습니다", &e),
        None => {}
    }
}

// ---------------------------------------------------------------- handlers

unsafe fn set_hotkeys(hwnd: HWND, on: bool) {
    let cur = with_app(|a| a.hotkeys).unwrap_or(false);
    if on {
        if !cur {
            let mods = MOD_CONTROL | MOD_SHIFT | MOD_NOREPEAT;
            RegisterHotKey(hwnd, HK_DOWNSCALE, mods, VK_MINUS);
            RegisterHotKey(hwnd, HK_RESTORE, mods, VK_Z);
        }
        SetTimer(hwnd, TIMER_HOTKEYS, HOTKEY_WINDOW_MS, None); // (re)start the 30 s window
    } else if cur {
        UnregisterHotKey(hwnd, HK_DOWNSCALE);
        UnregisterHotKey(hwnd, HK_RESTORE);
        KillTimer(hwnd, TIMER_HOTKEYS);
    }
    with_app(|a| a.hotkeys = on);
}

unsafe fn on_clipboard(hwnd: HWND) {
    if clip::has_marker() {
        return; // our own write
    }
    set_hotkeys(hwnd, false); // the clipboard no longer holds our result
    if !with_app(|a| a.paused).unwrap_or(true) {
        // Some apps set the clipboard in several steps; wait until it settles.
        SetTimer(hwnd, TIMER_DEBOUNCE, DEBOUNCE_MS, None);
    }
}

unsafe fn on_debounce(hwnd: HWND) {
    KillTimer(hwnd, TIMER_DEBOUNCE);
    if let Some((seq, src)) = clip::read_image(hwnd) {
        with_app(|a| a.jobs.send(Job::Optimise { seq, src }));
    }
}

unsafe fn on_hotkey(hwnd: HWND, which: i32) {
    let Some((id, original, factor, limit, tx)) = with_app(|a| {
        a.last.as_ref().map(|l| (l.id.clone(), l.original.clone(), l.factor, l.size, a.jobs.clone()))
    })
    .flatten() else {
        return;
    };
    if clip::marker_id(hwnd).as_deref() != Some(id.as_str()) {
        set_hotkeys(hwnd, false);
        return;
    }
    let (job, new_factor) = if which == HK_DOWNSCALE {
        let f = opt::next_factor(factor);
        if f >= factor {
            return; // already at the 10 % floor
        }
        (Job::Scale { id, original, factor: f, limit }, f)
    } else {
        (Job::Restore { id, original }, 1.0)
    };
    // Update now so quick repeated presses keep stepping down.
    with_app(|a| {
        if let Some(l) = a.last.as_mut() {
            l.factor = new_factor;
        }
    });
    let _ = tx.send(job);
    set_hotkeys(hwnd, true);
}

unsafe fn on_done(hwnd: HWND) {
    let Some(r) = RESULTS.lock().unwrap().pop_front() else { return };
    let d = match r {
        Ok(d) => d,
        Err(e) => return notify(hwnd, "ShotClip 오류", &e),
    };
    if !d.clip {
        return; // extra monitor of a full-screen capture: file only
    }
    if d.kind == Kind::Kept {
        return notify(hwnd, "축소하지 않음", "줄이면 오히려 파일이 커져서 지금 이미지를 그대로 둡니다");
    }
    // Downscale/restore results only apply if the clipboard still holds this result.
    let fresh = matches!(d.kind, Kind::Optimised | Kind::Captured);
    if !fresh && clip::marker_id(hwnd).as_deref() != Some(d.id.as_str()) {
        return;
    }
    let copy_path = with_app(|a| a.copy_path).unwrap_or(true);
    let path = d.desktop.as_deref().unwrap_or(&d.path);
    let out = clip::Output { id: &d.id, png: &d.png, dib: &d.dib, path, with_path: copy_path };
    if !clip::write(hwnd, &out, d.seq) {
        return; // clipboard changed meanwhile; drop the stale result
    }
    if fresh {
        let size = d.png.len();
        with_app(|a| a.last = Some(Last { id: d.id.clone(), original: d.original.clone(), factor: d.factor, size }));
    }
    // While the editor is open its own Ctrl+Shift+Z (redo) must not be grabbed globally.
    if !editor::is_open() {
        set_hotkeys(hwnd, true);
    }

    let after = d.png.len();
    let saved = if after < d.before { (d.before - after) * 100 / d.before } else { 0 };
    let title = match d.kind {
        Kind::Optimised => format!("스크린샷 최적화 · {}×{}", d.w, d.h),
        Kind::Scaled => format!("{}% 축소 · {}×{}", (d.factor * 100.0).round(), d.w, d.h),
        Kind::Restored => format!("원본 복원 · {}×{}", d.w, d.h),
        Kind::Kept => String::new(),
        Kind::Captured => match &d.desktop {
            Some(p) => format!("{} · {}×{}", p.file_stem().unwrap_or_default().to_string_lossy(), d.w, d.h),
            None => format!("스크린샷 · {}×{}", d.w, d.h),
        },
    };
    let size = if d.kind == Kind::Restored {
        opt::human(after)
    } else if d.kept_original && fresh {
        format!("{} (이미 최적 상태라 원본 유지)", opt::human(after))
    } else {
        format!("{} → {} (−{saved}%)", opt::human(d.before), opt::human(after))
    };
    notify(hwnd, &title, &format!("{size}\nCtrl+Shift+- 축소 · Ctrl+Shift+Z 원본"));
    // Give the decode/quantise buffers back to the OS so the idle footprint stays small.
    SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_CLIPBOARDUPDATE => on_clipboard(hwnd),
        WM_TIMER => match wp {
            TIMER_DEBOUNCE => on_debounce(hwnd),
            TIMER_HOTKEYS => set_hotkeys(hwnd, false),
            TIMER_CLEANUP => drop(thread::spawn(store::cleanup)),
            scroll::TIMER_ID => scroll::tick(),
            TIMER_UPDATE => start_check(hwnd, false),
            _ => {}
        },
        WM_HOTKEY => match wp as i32 {
            id @ HK_CAPTURE_FULL..=HK_RULER => on_tool(hwnd, id),
            id @ (scroll::HK_DONE | scroll::HK_CANCEL | scroll::HK_AUTO) => scroll::on_hotkey(id),
            which => on_hotkey(hwnd, which),
        },
        WM_DONE => on_done(hwnd),
        WM_OCR => on_ocr_done(hwnd),
        WM_UPDATE_CHECKED => on_update_checked(hwnd, wp == 1),
        WM_UPDATE_INSTALLED => on_update_installed(hwnd),
        WM_TRAY => {
            let ev = (lp as u32) & 0xFFFF;
            if ev == WM_RBUTTONUP || ev == WM_LBUTTONUP {
                show_menu(hwnd);
            } else if ev == NIN_BALLOONUSERCLICK && UPDATE_BALLOON.swap(false, Ordering::Relaxed) {
                start_install(hwnd);
            }
        }
        WM_DESTROY => {
            set_hotkeys(hwnd, false);
            scroll::finish(false);
            for id in HK_CAPTURE_FULL..=HK_RULER {
                UnregisterHotKey(hwnd, id);
            }
            Shell_NotifyIconW(NIM_DELETE, &nid(hwnd));
            PostQuitMessage(0);
        }
        m if m != 0 && m == TASKBAR_CREATED.load(Ordering::Relaxed) => tray_add(hwnd), // Explorer restarted
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    0
}

/// `shotclip --edit <png>`: open the editor on a file (writes "<name> 편집.png" next to it).
unsafe fn edit_file(path: &str) {
    SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    editor::register(GetModuleHandleW(null()));
    let src = PathBuf::from(path);
    let Ok(img) = fs::read(&src).map_err(|e| e.to_string()).and_then(|b| opt::decode_png(&b).map_err(|e| e.to_string())) else {
        MessageBoxW(null_mut(), wide(&format!("PNG를 열 수 없습니다: {path}")).as_ptr(), wide("ShotClip").as_ptr(), MB_ICONERROR);
        return;
    };
    let stem = src.file_stem().unwrap_or_default().to_string_lossy().to_string();
    let out = src.with_file_name(format!("{stem} 편집.png"));
    editor::open(
        img,
        &stem,
        true,
        Box::new(move |img| {
            if let Ok(png) = opt::encode_png(&img) {
                let _ = fs::write(&out, png);
            }
        }),
    );
    let mut msg: MSG = zeroed();
    while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 3 && args[1] == "--edit" {
        unsafe { edit_file(&args[2]) };
        return;
    }
    let updated = updater::after_update(); // waits for the previous version to exit
    unsafe {
        CreateMutexW(null(), 0, wide(r"Local\ShotClip.SingleInstance").as_ptr());
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return;
        }
        // Physical pixels everywhere, so captures are full resolution on scaled displays.
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let hinst = GetModuleHandleW(null());
        let mut oc: WNDCLASSW = zeroed();
        oc.lpfnWndProc = Some(overlay::overlay_proc);
        oc.hInstance = hinst;
        let oc_name = wide(overlay::OVERLAY_CLASS);
        oc.lpszClassName = oc_name.as_ptr();
        RegisterClassW(&oc);
        let mut fc: WNDCLASSW = zeroed();
        fc.lpfnWndProc = Some(scroll::frame_proc);
        fc.hInstance = hinst;
        let fc_name = wide(scroll::FRAME_CLASS);
        fc.lpszClassName = fc_name.as_ptr();
        RegisterClassW(&fc);
        editor::register(hinst);
        let class = wide("ShotClipWnd");
        let mut wc: WNDCLASSW = zeroed();
        wc.lpfnWndProc = Some(wndproc);
        wc.hInstance = hinst;
        wc.hIcon = app_icon(GetSystemMetrics(SM_CXICON));
        wc.lpszClassName = class.as_ptr();
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            wide("ShotClip").as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            hinst,
            null(),
        );
        if hwnd.is_null() {
            return;
        }

        let (tx, rx) = mpsc::channel();
        let h = hwnd as usize;
        thread::spawn(move || worker(rx, h));
        APP.with(|a| {
            *a.borrow_mut() = Some(App {
                jobs: tx,
                paused: store::get_bool("Paused", false),
                copy_path: store::get_bool("CopyPath", true),
                save_desktop: store::get_bool("SaveToDesktop", true),
                open_editor: store::get_bool("OpenEditor", true),
                hotkeys: false,
                update: None,
                installing: false,
                last: None,
            })
        });
        TASKBAR_CREATED.store(RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()), Ordering::Relaxed);

        tray_add(hwnd);
        AddClipboardFormatListener(hwnd);
        store::migrate_autostart();
        let mods = MOD_ALT | MOD_SHIFT | MOD_NOREPEAT;
        let failed: Vec<String> = (HK_CAPTURE_FULL..=HK_RULER)
            .zip('3'..='8')
            .filter(|(id, key)| RegisterHotKey(hwnd, *id, mods, *key as u32) == 0)
            .map(|(_, key)| key.to_string())
            .collect();
        if !failed.is_empty() {
            let keys = failed.join(", ");
            notify(hwnd, "ShotClip", &format!("Alt+Shift+{keys} 단축키를 등록하지 못했습니다 (다른 앱이 사용 중)."));
        }
        thread::spawn(store::cleanup);
        SetTimer(hwnd, TIMER_CLEANUP, 6 * 3600 * 1000, None);
        if updated {
            notify(hwnd, "ShotClip", &format!("{} 버전으로 업데이트했습니다", updater::VERSION));
        }
        start_check(hwnd, false);
        SetTimer(hwnd, TIMER_UPDATE, UPDATE_EVERY_MS, None);

        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
