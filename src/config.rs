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

/// How the bar occupies the screen edge.
#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum BarMode {
    /// Always shown, reserves its strip of the screen (an AppBar).
    Full,
    /// A small pill that expands into the full bar on hover; reserves no space.
    #[default]
    Island,
}

/// How the island looks.
#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum IslandStyle {
    /// Frosted glass: a blur of what is behind it with a light tint and a hairline
    /// border (falls back to a translucent pill without blur if the blur is unavailable).
    #[default]
    Glass,
    /// Solid black, like the Dynamic Island.
    Black,
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
    pub mode: BarMode,
    pub island_style: IslandStyle,
    /// Pill size while at rest, DIPs (clamped to 40..=600 / 4..=80), and its gap to
    /// the screen edge (0..=60).
    pub island_width: f32,
    pub island_height: f32,
    pub island_gap: f32,
    /// Hover time before expanding / after leaving before collapsing (ms).
    pub expand_delay_ms: u32,
    pub collapse_delay_ms: u32,
    pub island_show_clock: bool,
    /// Minimizing a window pours it into the island; restoring grows it back out.
    pub window_animation: bool,
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
            mode: BarMode::Island,
            island_style: IslandStyle::Glass,
            island_width: 200.0,
            island_height: 40.0,
            island_gap: 10.0,
            expand_delay_ms: 30,
            collapse_delay_ms: 600,
            island_show_clock: true,
            window_animation: true,
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
        config.bar.island_width = clamp_or(config.bar.island_width, 40.0, 600.0, 200.0);
        config.bar.island_height = clamp_or(config.bar.island_height, 4.0, 80.0, 40.0);
        config.bar.island_gap = clamp_or(config.bar.island_gap, 0.0, 60.0, 10.0);
        config.bar.expand_delay_ms = config.bar.expand_delay_ms.min(2000);
        config.bar.collapse_delay_ms = config.bar.collapse_delay_ms.min(10_000);
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

# Chế độ hiển thị:
#   "island": thu gọn thành một viên nhỏ ở mép màn hình, rê chuột vào (hoặc chạm mép, hoặc
#             nhấn phím Win) thì bung ra; không chiếm chỗ, taskbar gốc được ẩn.
#   "full":   thanh luôn hiện và giữ chỗ như taskbar thường.
mode = "island"
island_style = "glass"       # "glass" (kính mờ, blur phần phía sau) hoặc "black" (đen đặc)
island_width = 200           # cỡ viên lúc nghỉ (DIP, 40..600)
island_height = 40           # (DIP, 4..80)
island_gap = 10              # khoảng cách tới mép màn hình (DIP, 0..60)
expand_delay_ms = 30         # rê chuột vào bao lâu thì bung ra (0 = ngay lập tức)
collapse_delay_ms = 600      # rời chuột bao lâu thì thu lại
island_show_clock = true     # hiện giờ ở bên phải viên (bên trái là icon app đang active)

# Thu nhỏ cửa sổ: cửa sổ co lại, tối dần rồi "chui" vào island (viên nảy lên, hiện icon app);
# bấm nút trên thanh để khôi phục thì cửa sổ bung ra từ đó. false = hiệu ứng của Windows.
window_animation = true

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

/// The island lines the first island version wrote (a 120x8 pill, 100 ms delay...).
const LEGACY_ISLAND: [(&str, &str); 4] = [
    (
        "island_width = 120           # cỡ viên khi thu gọn (DIP, 40..600)",
        "island_width = 200           # cỡ viên lúc nghỉ (DIP, 40..600)",
    ),
    (
        "island_height = 8            # (DIP, 4..40)",
        "island_height = 40           # (DIP, 4..80)
island_gap = 10              # khoảng cách tới mép màn hình (DIP, 0..60)",
    ),
    (
        "expand_delay_ms = 100        # rê chuột vào bao lâu thì bung ra",
        "expand_delay_ms = 30         # rê chuột vào bao lâu thì bung ra (0 = ngay lập tức)",
    ),
    (
        "island_show_clock = false    # hiện giờ nhỏ trong viên",
        "island_show_clock = true     # hiện giờ ở bên phải viên (bên trái là icon app đang active)",
    ),
];

/// A config file written by an earlier version still pins that version's island
/// numbers (a file value always beats the default), so the new design would never
/// show. Lines that are still exactly what that version wrote (the user never
/// touched them) are brought up to date; edited lines are left alone.
fn migrate_legacy(text: &str) -> Option<String> {
    let mut out = text.to_string();
    for (old, new) in LEGACY_ISLAND {
        out = out.replace(old, new);
    }
    (out != text).then_some(out)
}

/// Reads the config file, creating it with defaults if missing.
pub fn load() -> Result<Config, String> {
    let Some(path) = path() else {
        return Ok(Config::default());
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => match migrate_legacy(&text) {
            Some(new) => {
                let _ = std::fs::write(&path, &new);
                Config::parse(&new)
            }
            None => Config::parse(&text),
        },
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
    fn legacy_island_lines_are_migrated_but_edits_are_kept() {
        let legacy = "[bar]
mode = \"island\"
island_width = 120           # cỡ viên khi thu gọn (DIP, 40..600)
island_height = 8            # (DIP, 4..40)
expand_delay_ms = 100        # rê chuột vào bao lâu thì bung ra
island_show_clock = false    # hiện giờ nhỏ trong viên
";
        let migrated = migrate_legacy(legacy).unwrap();
        let config = Config::parse(&migrated).unwrap();
        assert_eq!(config.bar, Config::default().bar);
        assert!(migrate_legacy(&migrated).is_none(), "idempotent");
        // The user changed the width: that line stays.
        let edited = legacy.replace("island_width = 120 ", "island_width = 150 ");
        let config = Config::parse(&migrate_legacy(&edited).unwrap()).unwrap();
        assert_eq!(config.bar.island_width, 150.0);
        assert_eq!(config.bar.island_height, 40.0);
        assert!(migrate_legacy(DEFAULT_CONFIG).is_none());
    }

    #[test]
    fn island_style_defaults_to_glass() {
        assert_eq!(Config::default().bar.island_style, IslandStyle::Glass);
        let black = Config::parse(
            "[bar]
island_style = \"black\"
",
        )
        .unwrap();
        assert_eq!(black.bar.island_style, IslandStyle::Black);
        assert!(
            Config::parse(
                "[bar]
island_style = \"neon\"
"
            )
            .is_err()
        );
    }

    #[test]
    fn partial_file_uses_defaults() {
        let config = Config::parse("[bar]\nposition = \"top\"\n").unwrap();
        assert_eq!(config.bar.position, Position::Top);
        assert_eq!(config.bar.height, 48.0);
        assert_eq!(config.bar.background, Background::Clear);
        assert!(config.clock.show_date);
        assert_eq!(config.bar.mode, BarMode::Island);
        let blur = Config::parse("[bar]\nbackground = \"blur\"\n").unwrap();
        assert_eq!(blur.bar.background, Background::Blur);
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        let config = Config::parse("[bar]\nheight = 500\nicon_size = 1\n").unwrap();
        assert_eq!(config.bar.height, 96.0);
        assert_eq!(config.bar.icon_size, 16.0);
        let island = Config::parse("[bar]\nisland_width = 5\ncollapse_delay_ms = 99999\n").unwrap();
        assert_eq!(island.bar.island_width, 40.0);
        assert_eq!(island.bar.collapse_delay_ms, 10_000);
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
