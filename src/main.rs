//! ShrinkClip: Clop-style clipboard screenshot optimiser for Windows.
//!
//! - Watches the clipboard; when a plain image arrives (e.g. Win+Shift+S), it quantises it
//!   like Clop (pngquant engine), saves it to %LOCALAPPDATA%\ShrinkClip\images and puts
//!   PNG + CF_DIB (+ file + path text) back on the clipboard.
//! - For 30 s after a result: Ctrl+Shift+- downscales (100→75→50→40…%), Ctrl+Shift+Z restores
//!   the original. Like Clop, the keys are only grabbed while a result is "live".
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod clip;
mod optimize;
mod store;

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
        atomic::{AtomicU32, Ordering},
        mpsc, Mutex,
    },
    thread,
};
use windows_sys::Win32::{
    Foundation::*,
    System::{DataExchange::AddClipboardFormatListener, LibraryLoader::GetModuleHandleW, Threading::{CreateMutexW, GetCurrentProcess, SetProcessWorkingSetSize}},
    UI::{Input::KeyboardAndMouse::*, Shell::*, WindowsAndMessaging::*},
};

const WM_TRAY: u32 = WM_APP + 1;
const WM_DONE: u32 = WM_APP + 2;
const TIMER_DEBOUNCE: usize = 1;
const TIMER_HOTKEYS: usize = 2;
const TIMER_CLEANUP: usize = 3;
const HK_DOWNSCALE: i32 = 1;
const HK_RESTORE: i32 = 2;
const HOTKEY_WINDOW_MS: u32 = 30_000;
const DEBOUNCE_MS: u32 = 150;
const VK_MINUS: u32 = 0xBD;
const VK_Z: u32 = 0x5A;

static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
static RESULTS: Mutex<VecDeque<Result<Done, String>>> = Mutex::new(VecDeque::new());

enum Job {
    Optimise { seq: u32, src: clip::Source },
    Scale { id: String, original: PathBuf, factor: f64 },
    Restore { id: String, original: PathBuf },
}

#[derive(PartialEq)]
enum Kind {
    Optimised,
    Scaled,
    Restored,
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
}

struct Last {
    id: String,
    original: PathBuf,
    factor: f64,
}

struct App {
    jobs: mpsc::Sender<Job>,
    paused: bool,
    copy_path: bool,
    hotkeys: bool,
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
        Job::Scale { id, original, factor } => {
            let orig_png = fs::read(&original)?;
            let img = opt::resize(&opt::decode_png(&orig_png)?, factor)?;
            let baseline = opt::encode_png(&img)?;
            let pct = (factor * 100.0).round() as u32;
            let path = store::images_dir().join(format!("screenshot_{id}_{pct}pct.png"));
            compress(Kind::Scaled, None, id, original, factor, img, baseline, orig_png.len(), path)
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
            })
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
    if paused { "ShrinkClip (일시정지)" } else { "ShrinkClip: 클립보드 이미지 최적화 중" }
}

unsafe fn tray_add(hwnd: HWND) {
    let paused = with_app(|a| a.paused).unwrap_or(false);
    let mut n = nid(hwnd);
    n.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    n.uCallbackMessage = WM_TRAY;
    n.hIcon = LoadIconW(null_mut(), IDI_APPLICATION);
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
    let mut n = nid(hwnd);
    n.uFlags = NIF_INFO;
    copy_w(&mut n.szInfoTitle, title);
    copy_w(&mut n.szInfo, body);
    n.dwInfoFlags = NIIF_INFO | NIIF_NOSOUND;
    Shell_NotifyIconW(NIM_MODIFY, &n);
}

unsafe fn show_menu(hwnd: HWND) {
    let Some((paused, copy_path)) = with_app(|a| (a.paused, a.copy_path)) else { return };
    let auto = store::autostart_enabled();
    let m = CreatePopupMenu();
    let item = |id: usize, text: &str, checked: bool| {
        let flags = MF_STRING | if checked { MF_CHECKED } else { MF_UNCHECKED };
        AppendMenuW(m, flags, id, wide(text).as_ptr());
    };
    item(1, "일시정지", paused);
    item(2, "파일 경로도 복사 (Clop 기본값)", copy_path);
    item(3, "Windows 시작 시 실행", auto);
    AppendMenuW(m, MF_SEPARATOR, 0, null());
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
        3 => store::set_autostart(!auto),
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
    let Some((id, original, factor, tx)) = with_app(|a| {
        a.last.as_ref().map(|l| (l.id.clone(), l.original.clone(), l.factor, a.jobs.clone()))
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
        (Job::Scale { id, original, factor: f }, f)
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
        Err(e) => return notify(hwnd, "ShrinkClip 오류", &e),
    };
    // Downscale/restore results only apply if the clipboard still holds this result.
    if d.seq.is_none() && clip::marker_id(hwnd).as_deref() != Some(d.id.as_str()) {
        return;
    }
    let copy_path = with_app(|a| a.copy_path).unwrap_or(true);
    let out = clip::Output { id: &d.id, png: &d.png, dib: &d.dib, path: &d.path, with_path: copy_path };
    if !clip::write(hwnd, &out, d.seq) {
        return; // clipboard changed meanwhile; drop the stale result
    }
    if d.kind == Kind::Optimised {
        with_app(|a| a.last = Some(Last { id: d.id.clone(), original: d.original.clone(), factor: d.factor }));
    }
    set_hotkeys(hwnd, true);

    let after = d.png.len();
    let saved = if after < d.before { (d.before - after) * 100 / d.before } else { 0 };
    let title = match d.kind {
        Kind::Optimised => format!("스크린샷 최적화 · {}×{}", d.w, d.h),
        Kind::Scaled => format!("{}% 축소 · {}×{}", (d.factor * 100.0).round(), d.w, d.h),
        Kind::Restored => format!("원본 복원 · {}×{}", d.w, d.h),
    };
    let size = if d.kind == Kind::Restored {
        opt::human(after)
    } else if d.kept_original && d.kind == Kind::Optimised {
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
            _ => {}
        },
        WM_HOTKEY => on_hotkey(hwnd, wp as i32),
        WM_DONE => on_done(hwnd),
        WM_TRAY => {
            let ev = (lp as u32) & 0xFFFF;
            if ev == WM_RBUTTONUP || ev == WM_LBUTTONUP {
                show_menu(hwnd);
            }
        }
        WM_DESTROY => {
            set_hotkeys(hwnd, false);
            Shell_NotifyIconW(NIM_DELETE, &nid(hwnd));
            PostQuitMessage(0);
        }
        m if m != 0 && m == TASKBAR_CREATED.load(Ordering::Relaxed) => tray_add(hwnd), // Explorer restarted
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    0
}

fn main() {
    unsafe {
        CreateMutexW(null(), 0, wide(r"Local\ShrinkClip.SingleInstance").as_ptr());
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return;
        }
        let hinst = GetModuleHandleW(null());
        let class = wide("ShrinkClipWnd");
        let mut wc: WNDCLASSW = zeroed();
        wc.lpfnWndProc = Some(wndproc);
        wc.hInstance = hinst;
        wc.lpszClassName = class.as_ptr();
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            wide("ShrinkClip").as_ptr(),
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
                hotkeys: false,
                last: None,
            })
        });
        TASKBAR_CREATED.store(RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()), Ordering::Relaxed);

        tray_add(hwnd);
        AddClipboardFormatListener(hwnd);
        thread::spawn(store::cleanup);
        SetTimer(hwnd, TIMER_CLEANUP, 6 * 3600 * 1000, None);

        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
