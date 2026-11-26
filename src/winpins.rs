//! The native taskbar's pinned apps, in the user's order.
//!
//! Windows 11 keeps the pin list in
//! `HKCU\...\Explorer\Taskband\Favorites`: a byte 0, then per pin a `u32` size, an
//! absolute PIDL of that size, and one separator byte (0xFF after the last pin).
//! Desktop apps point at shortcuts in `...\Quick Launch\User Pinned\TaskBar`;
//! Store / packaged apps point straight into `shell:AppsFolder` (no .lnk exists).
//! The blob format is undocumented, so if it is missing or unreadable we fall back
//! to the .lnk files in that folder (alphabetical; order unknown).

use std::ffi::c_void;

use windows::Win32::Foundation::{HANDLE, PROPERTYKEY};
use windows::Win32::Storage::EnhancedStorage::{PKEY_AppUserModel_ID, PKEY_Link_TargetParsingPath};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_NOTIFY, REG_NOTIFY_CHANGE_LAST_SET, RRF_RT_REG_BINARY,
    RegCloseKey, RegGetValueW, RegNotifyChangeKeyValue, RegOpenKeyExW,
};
use windows::Win32::System::Threading::CreateEventW;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    IShellItem2, SHCreateItemFromIDList, SIGDN, SIGDN_DESKTOPABSOLUTEPARSING, SIGDN_FILESYSPATH,
};
use windows::core::{PCWSTR, PWSTR, w};

use crate::tasks::AppId;

const TASKBAND: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Taskband");
/// Parsing-name prefix of items in `shell:AppsFolder` (FOLDERID_AppsFolder).
const APPS_FOLDER_PREFIX: &str = "::{4234D49B-0245-4DF3-B780-3893943456E1}\\";

/// One native pin: the app identity plus the shortcut it was pinned through, if any.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct WinPin {
    pub app: AppId,
    pub link: Option<String>,
}

/// Reads the native pin list (best effort; empty if nothing can be read).
pub fn read() -> Vec<WinPin> {
    let from_blob: Vec<WinPin> = favorites_blob()
        .map(|blob| {
            split_pidls(&blob)
                .iter()
                .filter_map(|p| pin_from_pidl(p))
                .collect()
        })
        .unwrap_or_default();
    if !from_blob.is_empty() {
        return from_blob;
    }
    pinned_folder_links()
}

fn favorites_blob() -> Option<Vec<u8>> {
    let mut size = 0u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            TASKBAND,
            w!("Favorites"),
            RRF_RT_REG_BINARY,
            None,
            None,
            Some(&raw mut size),
        )
        .ok()
        .ok()?;
    }
    let mut data = vec![0u8; size as usize];
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            TASKBAND,
            w!("Favorites"),
            RRF_RT_REG_BINARY,
            None,
            Some(data.as_mut_ptr().cast::<c_void>()),
            Some(&raw mut size),
        )
        .ok()
        .ok()?;
    }
    data.truncate(size as usize);
    Some(data)
}

/// Splits the Favorites blob into PIDLs, validating every item length.
fn split_pidls(blob: &[u8]) -> Vec<&[u8]> {
    let mut pidls = Vec::new();
    let mut off = 1;
    while let Some(size) = blob.get(off..off + 4) {
        let size = u32::from_le_bytes([size[0], size[1], size[2], size[3]]) as usize;
        let start = off + 4;
        let Some(pidl) = blob.get(start..start + size) else {
            break;
        };
        if is_valid_pidl(pidl) {
            pidls.push(pidl);
        }
        off = start + size;
        // Separator: 0 = another pin follows, 0xFF = end of list.
        if blob.get(off).is_none_or(|&b| b != 0) {
            break;
        }
        off += 1;
    }
    pidls
}

/// A PIDL is a chain of `u16`-length-prefixed items ending in a zero length.
fn is_valid_pidl(pidl: &[u8]) -> bool {
    let mut p = 0;
    while let Some(cb) = pidl.get(p..p + 2) {
        let cb = u16::from_le_bytes([cb[0], cb[1]]) as usize;
        if cb == 0 {
            return p > 0;
        }
        if cb < 2 {
            return false;
        }
        p += cb;
    }
    false
}

fn take_string(text: PWSTR) -> Option<String> {
    let result = unsafe { text.to_string() }.ok();
    unsafe { CoTaskMemFree(Some(text.0 as *const c_void)) };
    result.filter(|s| !s.is_empty())
}

fn item_string(item: &IShellItem2, key: &PROPERTYKEY) -> Option<String> {
    take_string(unsafe { item.GetString(key) }.ok()?)
}

fn item_name(item: &IShellItem2, sigdn: SIGDN) -> Option<String> {
    take_string(unsafe { item.GetDisplayName(sigdn) }.ok()?)
}

fn pin_from_pidl(pidl: &[u8]) -> Option<WinPin> {
    // ITEMIDLIST is byte-packed; the shell copies what it needs.
    let item: IShellItem2 =
        unsafe { SHCreateItemFromIDList(pidl.as_ptr().cast::<ITEMIDLIST>()) }.ok()?;
    let link = item_name(&item, SIGDN_FILESYSPATH)
        .filter(|path| path.to_ascii_lowercase().ends_with(".lnk"));
    let aumid = item_string(&item, &PKEY_AppUserModel_ID).or_else(|| {
        item_name(&item, SIGDN_DESKTOPABSOLUTEPARSING).and_then(|name| {
            name.get(..APPS_FOLDER_PREFIX.len())
                .filter(|p| p.eq_ignore_ascii_case(APPS_FOLDER_PREFIX))
                .map(|_| name[APPS_FOLDER_PREFIX.len()..].to_string())
        })
    });
    let exe = link
        .as_ref()
        .and_then(|_| item_string(&item, &PKEY_Link_TargetParsingPath))
        .filter(|target| looks_like_path(target));
    let pin = WinPin {
        app: AppId { aumid, exe },
        link,
    };
    (pin.app.aumid.is_some() || pin.app.exe.is_some() || pin.link.is_some()).then_some(pin)
}

fn looks_like_path(text: &str) -> bool {
    let b = text.as_bytes();
    (b.len() > 3 && b[1] == b':' && b[2] == b'\\') || text.starts_with("\\\\")
}

/// Fallback: the shortcuts in the pinned folder, alphabetically.
fn pinned_folder_links() -> Vec<WinPin> {
    let Some(appdata) = std::env::var_os("APPDATA") else {
        return Vec::new();
    };
    let dir = std::path::Path::new(&appdata)
        .join("Microsoft\\Internet Explorer\\Quick Launch\\User Pinned\\TaskBar");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<_> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("lnk")))
        .collect();
    paths.sort();
    paths
        .iter()
        .filter_map(|path| {
            let wide: Vec<u16> = path
                .as_os_str()
                .to_string_lossy()
                .encode_utf16()
                .chain(Some(0))
                .collect();
            let item: IShellItem2 = unsafe {
                windows::Win32::UI::Shell::SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None)
            }
            .ok()?;
            let exe = item_string(&item, &PKEY_Link_TargetParsingPath)
                .filter(|target| looks_like_path(target));
            Some(WinPin {
                app: AppId {
                    aumid: item_string(&item, &PKEY_AppUserModel_ID),
                    exe,
                },
                link: Some(path.to_string_lossy().into_owned()),
            })
        })
        .collect()
}

/// Signals an event whenever the Taskband key changes (pin / unpin / reorder in
/// the native taskbar). Waited on by the message loop; call `rearm` after each signal.
pub struct Watcher {
    key: HKEY,
    pub event: HANDLE,
}

impl Watcher {
    pub fn new() -> Option<Self> {
        let mut key = HKEY::default();
        unsafe {
            RegOpenKeyExW(HKEY_CURRENT_USER, TASKBAND, None, KEY_NOTIFY, &raw mut key)
                .ok()
                .ok()?;
        }
        let Ok(event) = (unsafe { CreateEventW(None, false, false, PCWSTR::null()) }) else {
            unsafe {
                let _ = RegCloseKey(key);
            }
            return None;
        };
        let watcher = Self { key, event };
        watcher.rearm();
        Some(watcher)
    }

    pub fn rearm(&self) {
        unsafe {
            let _ = RegNotifyChangeKeyValue(
                self.key,
                false,
                REG_NOTIFY_CHANGE_LAST_SET,
                Some(self.event),
                true,
            );
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.key);
            let _ = windows::Win32::Foundation::CloseHandle(self.event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_favorites_blob() {
        // header, [size=4][item cb=2? no: one 2-byte item + terminator], sep 0, second, sep 0xFF
        let mut blob = vec![0u8];
        for _ in 0..2 {
            blob.extend_from_slice(&6u32.to_le_bytes());
            blob.extend_from_slice(&[4, 0, 0xAA, 0xBB, 0, 0]);
            blob.push(0);
        }
        *blob.last_mut().unwrap() = 0xFF;
        assert_eq!(split_pidls(&blob).len(), 2);
        // Truncated / corrupt data never panics.
        assert!(split_pidls(&blob[..9]).is_empty());
        assert!(split_pidls(&[0, 0xFF, 0xFF, 0xFF, 0xFF]).is_empty());
    }
}
