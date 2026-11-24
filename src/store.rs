//! Work directory (Clop: ~/Library/Caches/Clop) and settings (HKCU\Software\ShotClip).

use crate::clip::wide;
use std::{
    fs,
    path::PathBuf,
    time::{Duration, SystemTime},
};
use windows_sys::Win32::System::{Registry::*, SystemInformation::GetLocalTime};

/// Clop's default `workdirCleanupInterval` is every 3 days.
pub const MAX_AGE: Duration = Duration::from_secs(3 * 24 * 3600);

pub fn workdir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("ShotClip")
}

fn sub(name: &str) -> PathBuf {
    let d = workdir().join(name);
    let _ = fs::create_dir_all(&d);
    d
}
pub fn images_dir() -> PathBuf {
    sub("images")
}
pub fn originals_dir() -> PathBuf {
    sub("originals")
}

pub fn timestamp() -> String {
    unsafe {
        let mut t = std::mem::zeroed();
        GetLocalTime(&mut t);
        format!(
            "{:04}{:02}{:02}_{:02}{:02}{:02}_{:03}",
            t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
        )
    }
}

pub fn cleanup() {
    let now = SystemTime::now();
    for dir in [images_dir(), originals_dir()] {
        let Ok(rd) = fs::read_dir(dir) else { continue };
        for e in rd.flatten() {
            let old = e
                .metadata()
                .and_then(|m| m.modified())
                .map(|t| now.duration_since(t).unwrap_or_default() > MAX_AGE)
                .unwrap_or(false);
            if old {
                let _ = fs::remove_file(e.path());
            }
        }
    }
}

const KEY: &str = r"Software\ShotClip";
/// Pre-0.2 name; settings and the autostart entry are migrated from it.
const OLD_KEY: &str = r"Software\ShrinkClip";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_NAME: &str = "ShotClip";
const OLD_RUN_NAME: &str = "ShrinkClip";

pub fn get_bool(name: &str, default: bool) -> bool {
    read_bool(KEY, name).or_else(|| read_bool(OLD_KEY, name)).unwrap_or(default)
}

/// Moves a ShrinkClip-era autostart entry over to this exe.
pub fn migrate_autostart() {
    let old = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            wide(RUN_KEY).as_ptr(),
            wide(OLD_RUN_NAME).as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        ) == 0
    };
    if old {
        unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, wide(RUN_KEY).as_ptr(), wide(OLD_RUN_NAME).as_ptr()) };
        set_autostart(true);
    }
}

fn read_bool(key: &str, name: &str) -> Option<bool> {
    let mut v: u32 = 0;
    let mut size = 4u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            wide(key).as_ptr(),
            wide(name).as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            &mut v as *mut u32 as *mut _,
            &mut size,
        )
    };
    (r == 0).then_some(v != 0)
}

pub fn set_bool(name: &str, value: bool) {
    let v = value as u32;
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            wide(KEY).as_ptr(),
            wide(name).as_ptr(),
            REG_DWORD,
            &v as *const u32 as *const _,
            4,
        );
    }
}

pub fn autostart_enabled() -> bool {
    let mut size = 0u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            wide(RUN_KEY).as_ptr(),
            wide(RUN_NAME).as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        ) == 0
    }
}

pub fn set_autostart(on: bool) {
    unsafe {
        if on {
            let exe = std::env::current_exe().unwrap_or_default();
            let cmd = wide(&format!("\"{}\"", exe.display()));
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                wide(RUN_KEY).as_ptr(),
                wide(RUN_NAME).as_ptr(),
                REG_SZ,
                cmd.as_ptr() as *const _,
                (cmd.len() * 2) as u32,
            );
        } else {
            RegDeleteKeyValueW(HKEY_CURRENT_USER, wide(RUN_KEY).as_ptr(), wide(RUN_NAME).as_ptr());
        }
    }
}
