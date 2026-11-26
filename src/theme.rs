//! System theme (light/dark + accent color), the window backdrop and dark menus.

use std::ffi::c_void;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::Dwm::{
    DWM_SYSTEMBACKDROP_TYPE, DWM_WINDOW_CORNER_PREFERENCE, DWMSBT_NONE, DWMWA_SYSTEMBACKDROP_TYPE,
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND,
    DwmExtendFrameIntoClientArea, DwmSetWindowAttribute,
};
use windows::Win32::System::LibraryLoader::{
    GetModuleHandleW, GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, RRF_RT_REG_BINARY, RRF_RT_REG_DWORD, RegGetValueW,
};
use windows::Win32::UI::Controls::MARGINS;
use windows::core::{BOOL, PCSTR, PCWSTR, s, w};

use crate::config::Background;

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
    /// Collapsed island pill: lightly tinted glass with a hairline border.
    pub island: D2D1_COLOR_F,
    pub island_border: D2D1_COLOR_F,
}

#[derive(Clone, Copy)]
pub struct Theme {
    pub light: bool,
    /// Backdrop in effect (`Solid` until applied, or if it could not be).
    pub backdrop: Background,
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
    /// `force_light` overrides the mode (config `theme = "dark" / "light"`).
    pub fn from_system(force_light: Option<bool>) -> Self {
        let light = force_light
            .unwrap_or_else(|| reg_dword(PERSONALIZE, w!("SystemUsesLightTheme")) == Some(1));
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
            backdrop: Background::Solid,
            accent,
        }
    }

    pub fn palette(&self) -> Palette {
        if self.light {
            Palette {
                background: match self.backdrop {
                    Background::Clear => rgba(0xf3, 0xf3, 0xf3, 0.0),
                    Background::Blur => rgba(0xf3, 0xf3, 0xf3, 0.25),
                    Background::Acrylic => rgba(0xf3, 0xf3, 0xf3, 0.6),
                    Background::Solid => rgba(0xee, 0xee, 0xee, 1.0),
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
                island: rgba(0xf3, 0xf3, 0xf3, 0.7),
                island_border: rgba(0x00, 0x00, 0x00, 0.18),
            }
        } else {
            Palette {
                background: match self.backdrop {
                    Background::Clear => rgba(0x1c, 0x1c, 0x1c, 0.0),
                    Background::Blur => rgba(0x1c, 0x1c, 0x1c, 0.25),
                    Background::Acrylic => rgba(0x1c, 0x1c, 0x1c, 0.6),
                    Background::Solid => rgba(0x1c, 0x1c, 0x1c, 1.0),
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
                island: rgba(0x20, 0x20, 0x20, 0.6),
                island_border: rgba(0xff, 0xff, 0xff, 0.22),
            }
        }
    }
}

/// Undocumented user32 `SetWindowCompositionAttribute` accent policy (used by
/// TranslucentTB, Windows Terminal and others since Windows 10).
#[repr(C)]
struct AccentPolicy {
    state: u32,
    flags: u32,
    /// 0xAABBGGRR
    gradient_color: u32,
    animation_id: u32,
}

#[repr(C)]
struct CompositionData {
    attribute: u32,
    data: *mut c_void,
    size: usize,
}

const WCA_ACCENT_POLICY: u32 = 19;
const ACCENT_DISABLED: u32 = 0;
const ACCENT_ENABLE_TRANSPARENTGRADIENT: u32 = 2;
const ACCENT_ENABLE_ACRYLICBLURBEHIND: u32 = 4;
/// Use `gradient_color` as the tint.
const ACCENT_FLAG_GRADIENT_COLOR: u32 = 2;

fn abgr(c: &D2D1_COLOR_F) -> u32 {
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    (byte(c.a) << 24) | (byte(c.b) << 16) | (byte(c.g) << 8) | byte(c.r)
}

fn set_accent(hwnd: HWND, state: u32, tint: &D2D1_COLOR_F) -> bool {
    type SetWindowCompositionAttribute =
        unsafe extern "system" fn(HWND, *mut CompositionData) -> BOOL;
    unsafe {
        let Ok(user32) = GetModuleHandleW(w!("user32.dll")) else {
            return false;
        };
        let Some(proc) = GetProcAddress(user32, s!("SetWindowCompositionAttribute")) else {
            return false;
        };
        let set: SetWindowCompositionAttribute = std::mem::transmute(proc);
        let mut policy = AccentPolicy {
            state,
            flags: ACCENT_FLAG_GRADIENT_COLOR,
            gradient_color: abgr(tint),
            animation_id: 0,
        };
        let mut data = CompositionData {
            attribute: WCA_ACCENT_POLICY,
            data: (&raw mut policy).cast::<c_void>(),
            size: size_of::<AccentPolicy>(),
        };
        set(hwnd, &raw mut data).as_bool()
    }
}

/// Dark mode, square corners and the backdrop. Returns the backdrop in effect.
///
/// Every translucent mode extends the DWM frame over the whole window, so pixels
/// we leave transparent are composed by DWM: a transparent-gradient accent shows
/// what is behind (`Clear`); the acrylic accent blurs it first. Unlike the
/// `DWMWA_SYSTEMBACKDROP_TYPE` backdrops, accents do not depend on the window
/// being active, so the bar needs no "fake active" trick and never falls back to
/// an opaque fill when a shell flyout takes the foreground.
pub fn apply_backdrop(
    hwnd: HWND,
    light: bool,
    wanted: Background,
    tint: &D2D1_COLOR_F,
) -> Background {
    unsafe {
        let dark = BOOL::from(!light);
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
        // Never the activation-dependent system backdrop.
        let none: DWM_SYSTEMBACKDROP_TYPE = DWMSBT_NONE;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            (&raw const none).cast::<c_void>(),
            size_of::<DWM_SYSTEMBACKDROP_TYPE>() as u32,
        );
        let extend = if wanted == Background::Solid { 0 } else { -1 };
        let margins = MARGINS {
            cxLeftWidth: extend,
            cxRightWidth: extend,
            cyTopHeight: extend,
            cyBottomHeight: extend,
        };
        if DwmExtendFrameIntoClientArea(hwnd, &raw const margins).is_err() {
            return Background::Solid;
        }
    }
    // ACCENT_ENABLE_BLURBEHIND (3) renders black for ordinary windows on current
    // Windows 11 builds, so "blur" is the acrylic accent with an (almost) clear
    // tint; the light tint is drawn by us. Acrylic is tinted by DWM itself.
    // Clear also keeps an accent (a fully transparent gradient, like TranslucentTB's
    // "clear"): going back to ACCENT_DISABLED after a blur accent leaves the
    // transparent pixels black until the window is recreated.
    let accent = match wanted {
        Background::Blur | Background::Acrylic => ACCENT_ENABLE_ACRYLICBLURBEHIND,
        Background::Clear => ACCENT_ENABLE_TRANSPARENTGRADIENT,
        Background::Solid => ACCENT_DISABLED,
    };
    let mut accent_tint = *tint;
    match wanted {
        Background::Acrylic => {}
        // Alpha 0 is treated as "no tint given" by some builds: keep 1/255.
        Background::Blur => accent_tint.a = 1.0 / 255.0,
        Background::Clear | Background::Solid => accent_tint.a = 0.0,
    }
    let applied = set_accent(hwnd, accent, &accent_tint);
    match wanted {
        // Without the accent, blur / acrylic would be fully clear: report that.
        Background::Blur | Background::Acrylic if !applied => Background::Clear,
        other => other,
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
