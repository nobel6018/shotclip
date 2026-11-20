//! Win32 clipboard read/write.

use std::{path::Path, ptr, sync::OnceLock, thread, time::Duration};
use windows_sys::Win32::{
    Foundation::{GlobalFree, HWND},
    System::{DataExchange::*, Memory::*},
};

pub const CF_METAFILEPICT: u32 = 3;
pub const CF_DIB: u32 = 8;
pub const CF_UNICODETEXT: u32 = 13;
pub const CF_ENHMETAFILE: u32 = 14;
pub const CF_HDROP: u32 = 15;

/// Formats that mean "this is not just a picture" (Office objects, spreadsheets, rich text,
/// password managers…). Windows counterpart of Clop's NOT_IMAGE_TYPES.
const BLOCKED_NAMES: &[&str] = &[
    "Object Descriptor",
    "Link Source Descriptor",
    "Embed Source",
    "Link Source",
    "Native",
    "OwnerLink",
    "ObjectLink",
    "Biff5",
    "Biff8",
    "Biff12",
    "XML Spreadsheet",
    "Rich Text Format",
    "image/svg+xml",
    "Kingsoft Data Descriptor",
    "ExcludeClipboardContentFromMonitorProcessing",
];

pub struct Formats {
    pub png: u32,
    pub marker: u32,
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

pub fn formats() -> &'static Formats {
    static F: OnceLock<Formats> = OnceLock::new();
    F.get_or_init(|| unsafe {
        Formats {
            png: RegisterClipboardFormatW(wide("PNG").as_ptr()),
            marker: RegisterClipboardFormatW(wide("ShrinkClip.Optimised").as_ptr()),
        }
    })
}

struct Open;
impl Drop for Open {
    fn drop(&mut self) {
        unsafe { CloseClipboard() };
    }
}
fn open(hwnd: HWND) -> Option<Open> {
    for _ in 0..20 {
        if unsafe { OpenClipboard(hwnd) } != 0 {
            return Some(Open);
        }
        thread::sleep(Duration::from_millis(25)); // another app is holding it
    }
    None
}

unsafe fn get_bytes(fmt: u32) -> Option<Vec<u8>> {
    let h = GetClipboardData(fmt);
    if h.is_null() {
        return None;
    }
    let p = GlobalLock(h) as *const u8;
    if p.is_null() {
        return None;
    }
    let v = std::slice::from_raw_parts(p, GlobalSize(h)).to_vec();
    GlobalUnlock(h);
    Some(v)
}

unsafe fn set_bytes(fmt: u32, data: &[u8]) {
    let h = GlobalAlloc(GMEM_MOVEABLE, data.len().max(1));
    if h.is_null() {
        return;
    }
    let p = GlobalLock(h) as *mut u8;
    if p.is_null() {
        GlobalFree(h);
        return;
    }
    ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
    GlobalUnlock(h);
    if SetClipboardData(fmt, h).is_null() {
        GlobalFree(h);
    }
}

pub fn has_marker() -> bool {
    unsafe { IsClipboardFormatAvailable(formats().marker) != 0 }
}

/// The id stored in our marker format, if the clipboard currently holds our output.
pub fn marker_id(hwnd: HWND) -> Option<String> {
    if !has_marker() {
        return None;
    }
    let _g = open(hwnd)?;
    let b = unsafe { get_bytes(formats().marker)? };
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8(b[..end].to_vec()).ok()
}

pub enum Source {
    Png(Vec<u8>),
    Dib(Vec<u8>),
}

/// Reads a plain image from the clipboard, or None if it isn't one we should touch.
pub fn read_image(hwnd: HWND) -> Option<(u32, Source)> {
    let f = formats();
    unsafe {
        if has_marker() {
            return None;
        }
        let has_png = IsClipboardFormatAvailable(f.png) != 0;
        if !has_png && IsClipboardFormatAvailable(CF_DIB) == 0 {
            return None;
        }
        let seq = GetClipboardSequenceNumber();
        let _g = open(hwnd)?;

        let mut fmt = 0;
        loop {
            fmt = EnumClipboardFormats(fmt);
            if fmt == 0 {
                break;
            }
            if matches!(fmt, CF_HDROP | CF_UNICODETEXT | CF_ENHMETAFILE | CF_METAFILEPICT) {
                return None;
            }
            let mut buf = [0u16; 128];
            let n = GetClipboardFormatNameW(fmt, buf.as_mut_ptr(), buf.len() as i32);
            if n > 0 {
                let name = String::from_utf16_lossy(&buf[..n as usize]);
                if name.starts_with("Art::") || BLOCKED_NAMES.contains(&name.as_str()) {
                    return None;
                }
            }
        }

        let src = if has_png { get_bytes(f.png).map(Source::Png) } else { None }
            .or_else(|| get_bytes(CF_DIB).map(Source::Dib))?;
        Some((seq, src))
    }
}

pub struct Output<'a> {
    pub id: &'a str,
    pub png: &'a [u8],
    pub dib: &'a [u8],
    pub path: &'a Path,
    pub with_path: bool,
}

/// Writes PNG + CF_DIB (+ CF_HDROP + path text, like Clop's `copyImageFilePath`) + marker.
/// If `expected_seq` is given, aborts when the clipboard changed since it was read.
pub fn write(hwnd: HWND, out: &Output, expected_seq: Option<u32>) -> bool {
    let Some(_g) = open(hwnd) else { return false };
    unsafe {
        if expected_seq.is_some_and(|s| s != GetClipboardSequenceNumber()) {
            return false;
        }
        if EmptyClipboard() == 0 {
            return false;
        }
        let f = formats();
        set_bytes(f.png, out.png);
        set_bytes(CF_DIB, out.dib);
        if out.with_path {
            let path: Vec<u16> = out.path.as_os_str().to_string_lossy().encode_utf16().collect();
            // DROPFILES { pFiles: u32, pt: POINT, fNC: BOOL, fWide: BOOL } + "path\0\0"
            let mut drop = Vec::with_capacity(20 + (path.len() + 2) * 2);
            drop.extend_from_slice(&20u32.to_le_bytes());
            drop.extend_from_slice(&[0u8; 12]);
            drop.extend_from_slice(&1u32.to_le_bytes());
            path.iter().chain(&[0, 0]).for_each(|c| drop.extend_from_slice(&c.to_le_bytes()));
            set_bytes(CF_HDROP, &drop);

            let text: Vec<u8> = path.iter().chain(&[0]).flat_map(|c| c.to_le_bytes()).collect();
            set_bytes(CF_UNICODETEXT, &text);
        }
        let mut id = out.id.as_bytes().to_vec();
        id.push(0);
        set_bytes(f.marker, &id);
    }
    true
}
