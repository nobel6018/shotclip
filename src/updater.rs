//! In-app update, same model as sori's Tauri updater (docs/update.md there):
//!
//! 1. Reads `releases/latest/download/latest.json` (`version`, `platforms.windows-x86_64.{url,signature}`)
//!    at start-up, every 12 h and from the tray menu.
//! 2. A newer version is only announced (notification + tray menu item); nothing installs by itself.
//! 3. Clicking installs: downloads the exe, verifies its minisign signature against the public key
//!    built in here, swaps the running exe (rename to .old, write the new one), starts the new exe
//!    with `--after-update <pid>` and quits. The new instance waits for us to exit, deletes the .old
//!    file and says which version it now is.
//!
//! The secret key lives outside the repo (~/.shotclip/updater.key); see scripts/release.sh.

use crate::clip::wide;
use std::{fs, path::PathBuf, ptr::null_mut};
use windows_sys::Win32::{
    Foundation::CloseHandle,
    System::{
        Com::Urlmon::URLDownloadToFileW,
        Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE},
    },
};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const PUBKEY: &str = "RWQG+9qzTUMmUR6U7hO4p9sTXHTeka/tBf8Rb65Lwjbg8WJkrJI58hyC";
const LATEST: &str = "https://github.com/nobel6018/shotclip/releases/latest/download/latest.json";
const PLATFORM: &str = "windows-x86_64";
pub const AFTER_UPDATE_ARG: &str = "--after-update";

#[derive(Clone)]
pub struct Update {
    pub version: String,
    url: String,
    signature: String,
}

/// Appends a line to %LOCALAPPDATA%\ShotClip\update\update.log (kept under ~64 KB).
pub fn log(msg: &str) {
    use std::io::Write;
    let dir = crate::store::workdir().join("update");
    let _ = fs::create_dir_all(&dir);
    let path = dir.join("update.log");
    if fs::metadata(&path).is_ok_and(|m| m.len() > 64 * 1024) {
        let _ = fs::remove_file(&path);
    }
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{} [{VERSION}] {msg}", crate::store::timestamp());
    }
}

fn parse_version(v: &str) -> Option<(u32, u32, u32)> {
    let mut it = v.trim().trim_start_matches('v').split(['.', '-']).map(|p| p.parse::<u32>());
    Some((it.next()?.ok()?, it.next()?.ok()?, it.next().unwrap_or(Ok(0)).ok()?))
}

fn newer(remote: &str, local: &str) -> bool {
    matches!((parse_version(remote), parse_version(local)), (Some(r), Some(l)) if r > l)
}

/// HTTPS GET through the OS stack (proxy settings, TLS, GitHub's redirects) into memory.
fn download(url: &str) -> Result<Vec<u8>, String> {
    let dir = crate::store::workdir().join("update");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!("dl_{}.tmp", std::process::id()));
    // A unique query defeats the WinINet cache (latest.json changes under the same URL).
    let nocache = if url.starts_with("file:") { url.to_string() } else { format!("{url}{}t={}", if url.contains('?') { '&' } else { '?' }, crate::store::timestamp()) };
    let hr = unsafe { URLDownloadToFileW(null_mut(), wide(&nocache).as_ptr(), wide(&tmp.to_string_lossy()).as_ptr(), 0, null_mut()) };
    let r = if hr == 0 { fs::read(&tmp).map_err(|e| e.to_string()) } else { Err(format!("다운로드 실패 (0x{:08X})", hr as u32)) };
    let _ = fs::remove_file(&tmp);
    r
}

/// The newer release, if any.
pub fn check() -> Result<Option<Update>, String> {
    let r = check_inner();
    match &r {
        Ok(Some(u)) => log(&format!("check: {} available", u.version)),
        Ok(None) => log("check: up to date"),
        Err(e) => log(&format!("check failed: {e}")),
    }
    r
}

fn check_inner() -> Result<Option<Update>, String> {
    // SHOTCLIP_UPDATE_URL points at a test manifest (file:// works); signatures are still checked.
    let url = std::env::var("SHOTCLIP_UPDATE_URL").unwrap_or_else(|_| LATEST.to_string());
    let body = download(&url)?;
    let json = body.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&body); // tolerate a UTF-8 BOM
    let j: serde_json::Value = serde_json::from_slice(json).map_err(|e| format!("latest.json: {e}"))?;
    let version = j["version"].as_str().ok_or("latest.json: version 없음")?.to_string();
    if !newer(&version, VERSION) {
        return Ok(None);
    }
    let p = &j["platforms"][PLATFORM];
    let (Some(url), Some(signature)) = (p["url"].as_str(), p["signature"].as_str()) else {
        return Err(format!("latest.json에 {PLATFORM} 항목이 없습니다"));
    };
    Ok(Some(Update { version, url: url.into(), signature: signature.into() }))
}

fn verify(data: &[u8], signature: &str) -> Result<(), String> {
    let pk = minisign_verify::PublicKey::from_base64(PUBKEY).map_err(|e| e.to_string())?;
    let sig = minisign_verify::Signature::decode(signature).map_err(|e| format!("서명 형식: {e}"))?;
    pk.verify(data, &sig, false).map_err(|_| "서명이 맞지 않습니다 (변조되었거나 다른 키로 서명됨)".to_string())
}

fn old_path(exe: &std::path::Path) -> PathBuf {
    exe.with_extension("exe.old")
}

/// Downloads, verifies and swaps in the new exe, then starts it. The caller quits on Ok.
pub fn install(u: &Update) -> Result<(), String> {
    log(&format!("install {} from {}", u.version, u.url));
    let r = install_inner(u);
    if let Err(e) = &r {
        log(&format!("install failed: {e}"));
    }
    r
}

fn install_inner(u: &Update) -> Result<(), String> {
    let data = download(&u.url)?;
    verify(&data, &u.signature)?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let old = old_path(&exe);
    let _ = fs::remove_file(&old);
    // A running exe can be renamed (not overwritten), which frees its name for the new one.
    fs::rename(&exe, &old).map_err(|e| format!("교체 실패: {e}"))?;
    if let Err(e) = fs::write(&exe, &data) {
        let _ = fs::rename(&old, &exe);
        return Err(format!("교체 실패: {e}"));
    }
    if let Err(e) = std::process::Command::new(&exe).args([AFTER_UPDATE_ARG, &std::process::id().to_string()]).spawn() {
        let _ = fs::remove_file(&exe);
        let _ = fs::rename(&old, &exe);
        return Err(format!("새 버전 실행 실패: {e}"));
    }
    Ok(())
}

/// Called first thing in main: with `--after-update <pid>`, waits for the old process to exit
/// and removes its exe. Returns whether this start follows an update.
pub fn after_update() -> bool {
    let r = after_update_inner();
    if r {
        log("started after update");
    }
    r
}

fn after_update_inner() -> bool {
    let args: Vec<String> = std::env::args().collect();
    let Some(i) = args.iter().position(|a| a == AFTER_UPDATE_ARG) else {
        if let Ok(exe) = std::env::current_exe() {
            let _ = fs::remove_file(old_path(&exe)); // leftover from an interrupted update
        }
        return false;
    };
    if let Some(pid) = args.get(i + 1).and_then(|p| p.parse::<u32>().ok()) {
        unsafe {
            let h = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
            if !h.is_null() {
                WaitForSingleObject(h, 15_000);
                CloseHandle(h);
            }
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        for _ in 0..20 {
            if fs::remove_file(old_path(&exe)).is_ok() || !old_path(&exe).exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(newer("0.3.1", "0.3.0"));
        assert!(newer("v1.0.0", "0.9.9"));
        assert!(newer("0.10.0", "0.9.0"));
        assert!(!newer("0.3.0", "0.3.0"));
        assert!(!newer("0.2.9", "0.3.0"));
        assert!(!newer("garbage", "0.3.0"));
    }

    /// SHOTCLIP_SIG_FILE=<file> (signed with ~/.shotclip/updater.key, <file>.minisig next to it)
    #[test]
    #[ignore]
    fn verifies_release_signature() {
        let f = std::env::var("SHOTCLIP_SIG_FILE").unwrap();
        let data = fs::read(&f).unwrap();
        let sig = fs::read_to_string(format!("{f}.minisig")).unwrap();
        verify(&data, &sig).unwrap();
        let mut bad = data.clone();
        bad[0] ^= 1;
        assert!(verify(&bad, &sig).is_err());
    }
}
