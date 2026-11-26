//! User configuration: `%APPDATA%\nitrobar\config.toml`.
//!
//! The file is created with commented defaults on first run, and re-applied live
//! whenever it is saved (directory change notification in the message loop).
//! A broken file never takes the bar down: the previous config stays active and
//! the error is shown in the right-click menu.

use std::path::PathBuf;

use serde::Deserialize;
use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;

#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum Position {
    #[default]
    Bottom,
    Top,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum Alignment {
    #[default]
    Center,
    Left,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    #[default]
    Auto,
    Dark,
    Light,
}

/// What is behind the bar's own drawing.
#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum Background {
    /// Fully transparent: the wallpaper / windows behind show through, unblurred.
    #[default]
    Clear,
    /// Blurred, lightly tinted.
    Blur,
    /// Blur + noise + tint, like Windows 11 flyouts.
    Acrylic,
    /// Opaque theme color.
    Solid,
}

#[derive(Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct BarConfig {
    pub position: Position,
    /// DIPs, clamped to 32..=96.
    pub height: f32,
    /// DIPs, clamped to 16..=48.
    pub icon_size: f32,
    pub alignment: Alignment,
    pub hide_native_taskbar: bool,
    pub theme: ThemeMode,
    pub background: Background,
}

impl Default for BarConfig {
    fn default() -> Self {
        Self {
            position: Position::Bottom,
            height: 48.0,
            icon_size: 24.0,
            alignment: Alignment::Center,
            hide_native_taskbar: false,
            theme: ThemeMode::Auto,
            background: Background::Clear,
        }
    }
}

/// Optional overrides, "#RRGGBB" or "#RRGGBBAA". Empty = follow the Windows theme.
#[derive(Deserialize, Clone, PartialEq, Debug, Default)]
#[serde(default)]
pub struct ColorConfig {
    pub background: String,
    pub text: String,
    pub accent: String,
}

#[derive(Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct ClockConfig {
    pub show_date: bool,
}

impl Default for ClockConfig {
    fn default() -> Self {
        Self { show_date: true }
    }
}

#[derive(Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct StatusConfig {
    pub show_battery_percent: bool,
}

impl Default for StatusConfig {
    fn default() -> Self {
        Self {
            show_battery_percent: true,
        }
    }
}

#[derive(Deserialize, Clone, PartialEq, Debug, Default)]
#[serde(default)]
pub struct Config {
    pub bar: BarConfig,
    pub colors: ColorConfig,
    pub clock: ClockConfig,
    pub status: StatusConfig,
}

impl Config {
    /// Parses TOML and clamps numbers into safe ranges.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut config: Config = toml::from_str(text).map_err(|e| e.to_string())?;
        config.bar.height = clamp_or(config.bar.height, 32.0, 96.0, 48.0);
        config.bar.icon_size = clamp_or(config.bar.icon_size, 16.0, 48.0, 24.0);
        Ok(config)
    }
}

fn clamp_or(value: f32, min: f32, max: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        fallback
    }
}

/// Written on first run. Must parse to `Config::default()` (see tests).
pub const DEFAULT_CONFIG: &str = r##"# nitrobar - cấu hình
# Lưu file là thanh tự áp dụng ngay, không cần khởi động lại.
# Nếu file lỗi, thanh giữ cấu hình cũ và báo lỗi trong menu chuột phải.

[bar]
position = "bottom"          # "bottom" hoặc "top"
height = 48                  # chiều cao thanh (DIP, 32..96)
icon_size = 24               # cỡ icon app (DIP, 16..48)
alignment = "center"         # "center" (giống Windows 11) hoặc "left"
hide_native_taskbar = false  # ẩn taskbar gốc của Windows khi nitrobar chạy
theme = "auto"               # "auto" (theo Windows), "dark" hoặc "light"
background = "clear"         # "clear" (trong suốt hẳn), "blur" (mờ), "acrylic" hoặc "solid" (đặc)

[colors]
# Để trống = theo theme Windows. Định dạng "#RRGGBB" hoặc "#RRGGBBAA".
background = ""
text = ""
accent = ""                  # màu vạch của app đang mở

[clock]
show_date = true

[status]
show_battery_percent = true
"##;

/// `%APPDATA%\nitrobar`
pub fn dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|base| PathBuf::from(base).join("nitrobar"))
}

pub fn path() -> Option<PathBuf> {
    dir().map(|d| d.join("config.toml"))
}

/// Reads the config file, creating it with defaults if missing.
pub fn load() -> Result<Config, String> {
    let Some(path) = path() else {
        return Ok(Config::default());
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => Config::parse(&text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&path, DEFAULT_CONFIG);
            Ok(Config::default())
        }
        Err(err) => Err(err.to_string()),
    }
}

/// "#RRGGBB" / "#RRGGBBAA" -> color. Empty or invalid -> `None`.
pub fn parse_color(text: &str) -> Option<D2D1_COLOR_F> {
    let hex = text.trim().strip_prefix('#')?;
    if !(hex.len() == 6 || hex.len() == 8) || !hex.is_ascii() {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    let alpha = if hex.len() == 8 { byte(6)? } else { 255 };
    Some(D2D1_COLOR_F {
        r: f32::from(byte(0)?) / 255.0,
        g: f32::from(byte(2)?) / 255.0,
        b: f32::from(byte(4)?) / 255.0,
        a: f32::from(alpha) / 255.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_file_matches_defaults() {
        assert_eq!(Config::parse(DEFAULT_CONFIG), Ok(Config::default()));
    }

    #[test]
    fn partial_file_uses_defaults() {
        let config = Config::parse("[bar]\nposition = \"top\"\n").unwrap();
        assert_eq!(config.bar.position, Position::Top);
        assert_eq!(config.bar.height, 48.0);
        assert_eq!(config.bar.background, Background::Clear);
        assert!(config.clock.show_date);
        let blur = Config::parse("[bar]\nbackground = \"blur\"\n").unwrap();
        assert_eq!(blur.bar.background, Background::Blur);
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        let config = Config::parse("[bar]\nheight = 500\nicon_size = 1\n").unwrap();
        assert_eq!(config.bar.height, 96.0);
        assert_eq!(config.bar.icon_size, 16.0);
    }

    #[test]
    fn invalid_file_is_an_error() {
        assert!(Config::parse("[bar]\nposition = \"middle\"\n").is_err());
        assert!(Config::parse("not toml").is_err());
        assert!(Config::parse("[bar]\nbackground = \"glass\"\n").is_err());
    }

    #[test]
    fn colors_parse() {
        let c = parse_color("#ff0080").unwrap();
        assert_eq!((c.r, c.g, c.a), (1.0, 0.0, 1.0));
        assert!((c.b - 128.0 / 255.0).abs() < 1e-6);
        assert_eq!(parse_color("#00000080").unwrap().a, 128.0 / 255.0);
        assert!(parse_color("").is_none());
        assert!(parse_color("red").is_none());
        assert!(parse_color("#12345").is_none());
    }
}
