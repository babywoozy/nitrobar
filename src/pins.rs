//! Pinned apps = the native taskbar's pins (live, in Windows' order) + apps pinned
//! in nitrobar, minus native pins the user unpinned in nitrobar.
//!
//! nitrobar never edits Windows' own pin list (undocumented format); its changes
//! live in `%APPDATA%\nitrobar\pins.toml`, written by the bar itself (kept apart
//! from config.toml so saving pins never touches the user's comments).

use serde::{Deserialize, Serialize};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, w};

use crate::config;
use crate::icons::{Icon, IconLoader};
use crate::tasks::AppId;
use crate::winpins::{self, WinPin};

#[derive(Serialize, Deserialize, Clone, Default)]
struct PinEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    aumid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    path: Option<String>,
}

impl PinEntry {
    fn from_app(app: &AppId) -> Self {
        Self {
            aumid: app.aumid.clone(),
            path: app.exe.clone(),
        }
    }

    fn app(&self) -> AppId {
        AppId {
            aumid: self.aumid.clone(),
            exe: self.path.clone(),
        }
    }
}

#[derive(Serialize, Deserialize, Default)]
struct PinFile {
    /// Apps pinned in nitrobar (shown after the native pins).
    /// Empty lists are skipped: `hide = []` after `[[pin]]` tables is not valid
    /// TOML output and makes the serializer fail.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pin: Vec<PinEntry>,
    /// Native taskbar pins the user unpinned in nitrobar.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    hide: Vec<PinEntry>,
}

impl PinFile {
    fn to_toml(&self) -> Option<String> {
        toml::to_string(self).ok()
    }
}

pub struct Pinned {
    pub app: AppId,
    /// Shortcut the app was pinned through (native pins): launched and used for the icon.
    pub link: Option<String>,
    pub icon: Option<Icon>,
    icon_attempted: bool,
}

#[derive(Default)]
pub struct Pins {
    pub items: Vec<Pinned>,
    native: Vec<WinPin>,
    own: Vec<AppId>,
    hidden: Vec<AppId>,
}

fn file() -> Option<std::path::PathBuf> {
    config::dir().map(|d| d.join("pins.toml"))
}

fn has_identity(app: &AppId) -> bool {
    app.aumid.is_some() || app.exe.is_some()
}

impl Pins {
    pub fn load() -> Self {
        let data = file()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| toml::from_str::<PinFile>(&text).ok())
            .unwrap_or_default();
        let mut pins = Self {
            own: data
                .pin
                .iter()
                .map(PinEntry::app)
                .filter(has_identity)
                .collect(),
            hidden: data
                .hide
                .iter()
                .map(PinEntry::app)
                .filter(has_identity)
                .collect(),
            native: winpins::read(),
            ..Self::default()
        };
        pins.rebuild();
        pins
    }

    /// Re-reads the native pin list (Taskband key changed). Returns true if it changed.
    pub fn reload_native(&mut self) -> bool {
        let native = winpins::read();
        if native == self.native {
            return false;
        }
        self.native = native;
        self.rebuild();
        true
    }

    /// Recomputes `items`, keeping already-loaded icons.
    fn rebuild(&mut self) {
        let mut old = std::mem::take(&mut self.items);
        let native = self
            .native
            .iter()
            .filter(|p| !self.hidden.iter().any(|h| h.same_app(&p.app)))
            .map(|p| (p.app.clone(), p.link.clone()));
        let own = self.own.iter().map(|app| (app.clone(), None));
        for (app, link) in native.chain(own) {
            let duplicate = self
                .items
                .iter()
                .any(|p| has_identity(&app) && p.app.same_app(&app));
            if duplicate {
                continue;
            }
            let reused = old
                .iter()
                .position(|p| p.app == app && p.link == link)
                .map(|i| old.swap_remove(i));
            self.items.push(reused.unwrap_or(Pinned {
                app,
                link,
                icon: None,
                icon_attempted: false,
            }));
        }
    }

    fn save(&self) {
        let data = PinFile {
            pin: self.own.iter().map(PinEntry::from_app).collect(),
            hide: self.hidden.iter().map(PinEntry::from_app).collect(),
        };
        let (Some(path), Some(text)) = (file(), data.to_toml()) else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let header = "# Ghim của nitrobar, do nitrobar tự ghi (chuột phải > Ghim / Bỏ ghim).\n\
                      # App ghim trên taskbar gốc của Windows được đọc trực tiếp, không lưu ở đây.\n\
                      # [[pin]]  = app ghim thêm trong nitrobar\n\
                      # [[hide]] = app ghim của Windows mà bạn đã bỏ ghim trong nitrobar\n\n";
        let _ = std::fs::write(path, format!("{header}{text}"));
    }

    /// Index of the pin for this app, if any.
    pub fn find(&self, app: &AppId) -> Option<usize> {
        self.items.iter().position(|p| p.app.same_app(app))
    }

    pub fn pin(&mut self, app: AppId) {
        if !has_identity(&app) || self.find(&app).is_some() {
            return;
        }
        let was_hidden = self.hidden.len();
        self.hidden.retain(|h| !h.same_app(&app));
        let is_native = self.native.iter().any(|p| p.app.same_app(&app));
        if was_hidden == self.hidden.len() || !is_native {
            self.own.push(app);
        }
        self.rebuild();
        self.save();
    }

    pub fn unpin(&mut self, app: &AppId) {
        self.own.retain(|a| !a.same_app(app));
        if self.native.iter().any(|p| p.app.same_app(app)) && has_identity(app) {
            self.hidden.push(app.clone());
        }
        self.rebuild();
        self.save();
    }

    /// Loads icons that are missing (once per pin), e.g. at startup or after a DPI change.
    pub fn load_icons(&mut self, icons: &IconLoader, px: u32, force: bool) {
        for pin in &mut self.items {
            if force || (pin.icon.is_none() && !pin.icon_attempted) {
                pin.icon_attempted = true;
                pin.icon = icons.load_app(&pin.app, pin.link.as_deref(), px);
            }
        }
    }
}

fn shell_open(target: &str) -> bool {
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &HSTRING::from(target),
            None,
            None,
            SW_SHOWNORMAL,
        )
    };
    // ShellExecute returns a value > 32 on success.
    result.0 as usize > 32
}

/// Starts a pinned app through its shortcut (keeps arguments / working directory),
/// else packaged / AUMID apps through `shell:AppsFolder`, else the exe.
pub fn launch_pin(pin: &Pinned) {
    if let Some(link) = &pin.link
        && shell_open(link)
    {
        return;
    }
    launch(&pin.app);
}

/// Starts an app: packaged / AUMID apps through `shell:AppsFolder`, else the exe.
pub fn launch(app: &AppId) {
    if let Some(aumid) = &app.aumid
        && shell_open(&format!("shell:AppsFolder\\{aumid}"))
    {
        return;
    }
    if let Some(exe) = &app.exe {
        shell_open(exe);
    }
}

/// Opens a file with its associated app, falling back to Notepad.
pub fn open_file(path: &std::path::Path) {
    let path = HSTRING::from(path.as_os_str());
    unsafe {
        let result = ShellExecuteW(None, w!("open"), &path, None, None, SW_SHOWNORMAL);
        if result.0 as usize <= 32 {
            ShellExecuteW(
                None,
                w!("open"),
                w!("notepad.exe"),
                &path,
                None,
                SW_SHOWNORMAL,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(aumid: Option<&str>, exe: Option<&str>) -> AppId {
        AppId {
            aumid: aumid.map(str::to_string),
            exe: exe.map(str::to_string),
        }
    }

    fn pins_with_native(native: Vec<AppId>) -> Pins {
        let mut pins = Pins {
            native: native
                .into_iter()
                .map(|app| WinPin { app, link: None })
                .collect(),
            ..Pins::default()
        };
        pins.rebuild();
        pins
    }

    #[test]
    fn native_order_then_own_without_duplicates() {
        let explorer = app(Some("Microsoft.Windows.Explorer"), None);
        let store = app(Some("Claude_pzs8sxrjxfjjc!Claude"), None);
        let mut pins = pins_with_native(vec![explorer.clone(), store.clone()]);
        pins.own = vec![app(None, Some("C:\\x\\a.exe")), store.clone()];
        pins.rebuild();
        let order: Vec<_> = pins.items.iter().map(|p| p.app.clone()).collect();
        assert_eq!(
            order,
            vec![explorer, store, app(None, Some("C:\\x\\a.exe"))]
        );
    }

    #[test]
    fn hiding_a_native_pin_and_pinning_it_back() {
        let store = app(Some("Claude_pzs8sxrjxfjjc!Claude"), None);
        let mut pins = pins_with_native(vec![store.clone()]);
        pins.hidden.push(store.clone());
        pins.rebuild();
        assert!(pins.items.is_empty());
        // Pinning again just un-hides it (it stays in Windows' position).
        pins.hidden.retain(|h| !h.same_app(&store));
        pins.rebuild();
        assert_eq!(pins.items.len(), 1);
        assert!(pins.own.is_empty());
    }

    #[test]
    fn pin_file_round_trip() {
        for (pin, hide) in [(1, 0), (0, 1), (2, 1), (0, 0)] {
            let entry = PinEntry::from_app(&app(Some("A!b"), Some("C:\\a.exe")));
            let data = PinFile {
                pin: vec![entry.clone(); pin],
                hide: vec![entry; hide],
            };
            let text = data.to_toml().expect("serializes");
            let back: PinFile = toml::from_str(&text).expect("parses");
            assert_eq!((back.pin.len(), back.hide.len()), (pin, hide));
        }
    }

    #[test]
    fn matching_running_apps() {
        let chrome = app(None, Some("C:\\Chrome\\chrome.exe"));
        let profile = app(Some("Chrome.Profile1"), Some("C:\\Chrome\\chrome.exe"));
        let other_profile = app(Some("Chrome.Profile2"), Some("C:\\Chrome\\CHROME.exe"));
        assert!(chrome.same_app(&profile));
        assert!(!profile.same_app(&other_profile));
        assert!(app(Some("MSEdge"), None).same_app(&app(Some("msedge"), Some("C:\\e.exe"))));
    }
}
