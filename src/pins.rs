//! Pinned apps: `%APPDATA%\nitrobar\pins.toml`, written by the bar itself
//! (kept apart from config.toml so saving pins never touches the user's comments).

use serde::{Deserialize, Serialize};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, w};

use crate::config;
use crate::icons::{Icon, IconLoader};
use crate::tasks::AppId;

#[derive(Serialize, Deserialize, Clone, Default)]
struct PinEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    aumid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    path: Option<String>,
}

#[derive(Serialize, Deserialize, Default)]
struct PinFile {
    #[serde(default)]
    pin: Vec<PinEntry>,
}

pub struct Pinned {
    pub app: AppId,
    pub key: String,
    pub icon: Option<Icon>,
    icon_attempted: bool,
}

#[derive(Default)]
pub struct Pins {
    pub items: Vec<Pinned>,
}

fn file() -> Option<std::path::PathBuf> {
    config::dir().map(|d| d.join("pins.toml"))
}

impl Pins {
    pub fn load() -> Self {
        let entries = file()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| toml::from_str::<PinFile>(&text).ok())
            .map(|f| f.pin)
            .unwrap_or_default();
        let mut pins = Self::default();
        for entry in entries {
            pins.push(AppId {
                aumid: entry.aumid,
                exe: entry.path,
            });
        }
        pins
    }

    fn save(&self) {
        let data = PinFile {
            pin: self
                .items
                .iter()
                .map(|p| PinEntry {
                    aumid: p.app.aumid.clone(),
                    path: p.app.exe.clone(),
                })
                .collect(),
        };
        let (Some(path), Ok(text)) = (file(), toml::to_string(&data)) else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let header =
            "# Ứng dụng được ghim. nitrobar tự ghi file này (chuột phải > Ghim / Bỏ ghim).\n\n";
        let _ = std::fs::write(path, format!("{header}{text}"));
    }

    fn push(&mut self, app: AppId) {
        if app.aumid.is_none() && app.exe.is_none() {
            return;
        }
        let key = app.key();
        if !self.contains(&key) {
            self.items.push(Pinned {
                app,
                key,
                icon: None,
                icon_attempted: false,
            });
        }
    }

    pub fn contains(&self, key: &str) -> bool {
        self.items.iter().any(|p| p.key == key)
    }

    pub fn pin(&mut self, app: AppId) {
        self.push(app);
        self.save();
    }

    pub fn unpin(&mut self, key: &str) {
        self.items.retain(|p| p.key != key);
        self.save();
    }

    /// Loads icons that are missing (once per pin), e.g. at startup or after a DPI change.
    pub fn load_icons(&mut self, icons: &IconLoader, px: u32, force: bool) {
        for pin in &mut self.items {
            if force || (pin.icon.is_none() && !pin.icon_attempted) {
                pin.icon_attempted = true;
                pin.icon = icons.load_app(&pin.app, px);
            }
        }
    }
}

/// Starts an app: packaged / AUMID apps through `shell:AppsFolder`, else the exe.
pub fn launch(app: &AppId) {
    let run = |target: &str| unsafe {
        let result = ShellExecuteW(
            None,
            w!("open"),
            &HSTRING::from(target),
            None,
            None,
            SW_SHOWNORMAL,
        );
        // ShellExecute returns a value > 32 on success.
        result.0 as usize > 32
    };
    if let Some(aumid) = &app.aumid
        && run(&format!("shell:AppsFolder\\{aumid}"))
    {
        return;
    }
    if let Some(exe) = &app.exe {
        run(exe);
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
