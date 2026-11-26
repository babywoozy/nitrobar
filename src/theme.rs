//! System theme (light/dark + accent color), the acrylic backdrop and dark menus.

use std::ffi::c_void;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::Dwm::{
    DWM_SYSTEMBACKDROP_TYPE, DWM_WINDOW_CORNER_PREFERENCE, DWMSBT_TRANSIENTWINDOW,
    DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_DONOTROUND, DwmExtendFrameIntoClientArea, DwmSetWindowAttribute,
};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, RRF_RT_REG_BINARY, RRF_RT_REG_DWORD, RegGetValueW,
};
use windows::Win32::UI::Controls::MARGINS;
use windows::core::{BOOL, PCSTR, PCWSTR, w};

const fn rgba(r: u8, g: u8, b: u8, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a,
    }
}

/// Every color the bar draws with.
#[derive(Clone, Copy)]
pub struct Palette {
    /// Fill behind everything. Translucent when the acrylic backdrop is active.
    pub background: D2D1_COLOR_F,
    pub text: D2D1_COLOR_F,
    pub hover: D2D1_COLOR_F,
    pub pressed: D2D1_COLOR_F,
    pub active: D2D1_COLOR_F,
    pub active_hover: D2D1_COLOR_F,
    pub indicator: D2D1_COLOR_F,
    pub accent: D2D1_COLOR_F,
    pub attention: D2D1_COLOR_F,
    pub attention_bg: D2D1_COLOR_F,
    pub placeholder: D2D1_COLOR_F,
}

#[derive(Clone, Copy)]
pub struct Theme {
    pub light: bool,
    /// Whether DWM accepted the acrylic backdrop for our window.
    pub acrylic: bool,
    pub accent: D2D1_COLOR_F,
}

const PERSONALIZE: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
const ACCENT: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Accent");

fn reg_dword(subkey: PCWSTR, value: PCWSTR) -> Option<u32> {
    let mut data = 0u32;
    let mut size = size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey,
            value,
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut data).cast::<c_void>()),
            Some(&raw mut size),
        )
    };
    status.is_ok().then_some(data)
}

/// `AccentPalette` holds 8 RGBA colors: Light3, Light2, Light1, Base, Dark1, Dark2, Dark3, unused.
fn accent_palette() -> Option<[u8; 32]> {
    let mut data = [0u8; 32];
    let mut size = data.len() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            ACCENT,
            w!("AccentPalette"),
            RRF_RT_REG_BINARY,
            None,
            Some(data.as_mut_ptr().cast::<c_void>()),
            Some(&raw mut size),
        )
    };
    (status.is_ok() && size >= 32).then_some(data)
}

impl Theme {
    /// Reads the current Windows mode ("SystemUsesLightTheme") and accent color.
    pub fn from_system() -> Self {
        let light = reg_dword(PERSONALIZE, w!("SystemUsesLightTheme")) == Some(1);
        // Same shades the Windows 11 taskbar uses for its indicator.
        let index = if light { 4 } else { 1 };
        let accent = accent_palette().map_or(
            if light {
                rgba(0x00, 0x5f, 0xb8, 1.0)
            } else {
                rgba(0x4c, 0xc2, 0xff, 1.0)
            },
            |p| rgba(p[index * 4], p[index * 4 + 1], p[index * 4 + 2], 1.0),
        );
        Self {
            light,
            acrylic: false,
            accent,
        }
    }

    pub fn palette(&self) -> Palette {
        if self.light {
            Palette {
                background: if self.acrylic {
                    rgba(0xf3, 0xf3, 0xf3, 0.55)
                } else {
                    rgba(0xee, 0xee, 0xee, 1.0)
                },
                text: rgba(0x00, 0x00, 0x00, 0.9),
                hover: rgba(0xff, 0xff, 0xff, 0.6),
                pressed: rgba(0xff, 0xff, 0xff, 0.4),
                active: rgba(0xff, 0xff, 0xff, 0.7),
                active_hover: rgba(0xff, 0xff, 0xff, 0.85),
                indicator: rgba(0x00, 0x00, 0x00, 0.45),
                accent: self.accent,
                attention: rgba(0xc4, 0x2b, 0x1c, 1.0),
                attention_bg: rgba(0xff, 0x8c, 0x00, 0.3),
                placeholder: rgba(0x00, 0x00, 0x00, 0.2),
            }
        } else {
            Palette {
                background: if self.acrylic {
                    rgba(0x1c, 0x1c, 0x1c, 0.55)
                } else {
                    rgba(0x1c, 0x1c, 0x1c, 1.0)
                },
                text: rgba(0xff, 0xff, 0xff, 1.0),
                hover: rgba(0xff, 0xff, 0xff, 0.08),
                pressed: rgba(0xff, 0xff, 0xff, 0.05),
                active: rgba(0xff, 0xff, 0xff, 0.06),
                active_hover: rgba(0xff, 0xff, 0xff, 0.11),
                indicator: rgba(0xff, 0xff, 0xff, 0.55),
                accent: self.accent,
                attention: rgba(0xff, 0x8c, 0x00, 1.0),
                attention_bg: rgba(0xff, 0x8c, 0x00, 0.28),
                placeholder: rgba(0xff, 0xff, 0xff, 0.25),
            }
        }
    }
}

/// Dark title-bar mode, square corners and the acrylic system backdrop.
/// Returns false where unsupported (Windows 10, Windows 11 before 22H2): draw opaque then.
pub fn apply_backdrop(hwnd: HWND, theme: &Theme) -> bool {
    unsafe {
        let dark = BOOL::from(!theme.light);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            (&raw const dark).cast::<c_void>(),
            size_of::<BOOL>() as u32,
        );
        let corners: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_DONOTROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            (&raw const corners).cast::<c_void>(),
            size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
        );
        // The whole client area becomes "frame", so DWM composes the backdrop under
        // every pixel we leave transparent.
        let margins = MARGINS {
            cxLeftWidth: -1,
            cxRightWidth: -1,
            cyTopHeight: -1,
            cyBottomHeight: -1,
        };
        if DwmExtendFrameIntoClientArea(hwnd, &raw const margins).is_err() {
            return false;
        }
        let backdrop: DWM_SYSTEMBACKDROP_TYPE = DWMSBT_TRANSIENTWINDOW;
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            (&raw const backdrop).cast::<c_void>(),
            size_of::<DWM_SYSTEMBACKDROP_TYPE>() as u32,
        )
        .is_ok()
    }
}

/// Makes Win32 popup menus follow the theme.
///
/// Uses uxtheme's undocumented `SetPreferredAppMode` (ordinal 135) and
/// `FlushMenuThemes` (ordinal 136), as Explorer, Notepad++ and others do. If the
/// ordinals ever disappear, menus simply stay light.
pub fn apply_menu_theme(light: bool) {
    type SetPreferredAppMode = unsafe extern "system" fn(i32) -> i32;
    type FlushMenuThemes = unsafe extern "system" fn();
    const FORCE_DARK: i32 = 2;
    const FORCE_LIGHT: i32 = 3;
    unsafe {
        let Ok(uxtheme) = LoadLibraryExW(w!("uxtheme.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32)
        else {
            return;
        };
        if let Some(set_mode) = GetProcAddress(uxtheme, PCSTR(135 as *const u8)) {
            let set_mode: SetPreferredAppMode = std::mem::transmute(set_mode);
            set_mode(if light { FORCE_LIGHT } else { FORCE_DARK });
        }
        if let Some(flush) = GetProcAddress(uxtheme, PCSTR(136 as *const u8)) {
            let flush: FlushMenuThemes = std::mem::transmute(flush);
            flush();
        }
    }
}
