//! The island's "wallpaper" color: the pleasant dominant color of the current
//! wallpaper, found on a short-lived thread (decoding an image is slow) and
//! recomputed only when the wallpaper changes (`WM_SETTINGCHANGE` /
//! `SPI_SETDESKWALLPAPER`, which a slideshow sends for every new picture).

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use windows::Win32::Foundation::{GENERIC_READ, HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppBGRA, IWICImagingFactory,
    WICBitmapDitherTypeNone, WICBitmapInterpolationModeFant, WICBitmapPaletteTypeCustom,
    WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::UI::WindowsAndMessaging::{
    PostMessageW, SPI_GETDESKWALLPAPER, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
    WM_APP,
};
use windows::core::HSTRING;

/// Posted to the bar when the color is ready: lParam = `0x00BBGGRR`.
pub const WM_APP_WALLPAPER: u32 = WM_APP + 11;
/// `SPI_SETDESKWALLPAPER`, the `wParam` of the `WM_SETTINGCHANGE` for a new wallpaper.
pub const SPI_SETDESKWALLPAPER: usize = 0x0014;

static BUSY: AtomicBool = AtomicBool::new(false);
/// (path, modified time) of the picture the last result was computed from.
static LAST: Mutex<Option<(PathBuf, Option<SystemTime>)>> = Mutex::new(None);

/// Path of the current wallpaper picture (empty / `None` for a plain color).
fn current_path() -> Option<PathBuf> {
    let mut buffer = [0u16; 520];
    unsafe {
        SystemParametersInfoW(
            SPI_GETDESKWALLPAPER,
            buffer.len() as u32,
            Some(buffer.as_mut_ptr().cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .ok()?;
    }
    let len = buffer.iter().position(|&c| c == 0)?;
    (len > 0).then(|| PathBuf::from(String::from_utf16_lossy(&buffer[..len])))
}

/// Finds the wallpaper color on a thread and posts `WM_APP_WALLPAPER` to `bar`.
/// With `force` it is computed even if the picture is the one already analysed.
pub fn request(bar: HWND, force: bool) {
    if BUSY.swap(true, Ordering::AcqRel) {
        return;
    }
    let target = bar.0 as usize;
    let spawned = std::thread::Builder::new()
        .name("nitrobar-wallpaper".into())
        .stack_size(256 * 1024)
        .spawn(move || {
            if let Some(path) = current_path() {
                let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
                let key = (path.clone(), modified);
                let same = LAST.lock().is_ok_and(|last| last.as_ref() == Some(&key));
                if (force || !same)
                    && let Some((r, g, b)) = analyze(&path)
                {
                    if let Ok(mut last) = LAST.lock() {
                        *last = Some(key);
                    }
                    let rgb = u32::from(r) | u32::from(g) << 8 | u32::from(b) << 16;
                    unsafe {
                        let _ = PostMessageW(
                            Some(HWND(target as *mut _)),
                            WM_APP_WALLPAPER,
                            WPARAM(0),
                            LPARAM(rgb as isize),
                        );
                    }
                }
            }
            BUSY.store(false, Ordering::Release);
        });
    if spawned.is_err() {
        BUSY.store(false, Ordering::Release);
    }
}

/// Decodes `path`, shrinks it to a thumbnail and returns its dominant color.
fn analyze(path: &std::path::Path) -> Option<(u8, u8, u8)> {
    const THUMB: u32 = 48;
    unsafe {
        // Own COM apartment for this thread.
        let com = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        let result = (|| {
            let factory: IWICImagingFactory =
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
            let decoder = factory
                .CreateDecoderFromFilename(
                    &HSTRING::from(path.as_os_str()),
                    None,
                    GENERIC_READ,
                    WICDecodeMetadataCacheOnDemand,
                )
                .ok()?;
            let frame = decoder.GetFrame(0).ok()?;
            let (mut w, mut h) = (0u32, 0u32);
            frame.GetSize(&raw mut w, &raw mut h).ok()?;
            if w == 0 || h == 0 {
                return None;
            }
            let k = (THUMB as f32 / w.max(h) as f32).min(1.0);
            let (tw, th) = (
                ((w as f32 * k).round() as u32).max(1),
                ((h as f32 * k).round() as u32).max(1),
            );
            let scaler = factory.CreateBitmapScaler().ok()?;
            scaler
                .Initialize(&frame, tw, th, WICBitmapInterpolationModeFant)
                .ok()?;
            let converter = factory.CreateFormatConverter().ok()?;
            converter
                .Initialize(
                    &scaler,
                    &GUID_WICPixelFormat32bppBGRA,
                    WICBitmapDitherTypeNone,
                    None,
                    0.0,
                    WICBitmapPaletteTypeCustom,
                )
                .ok()?;
            let stride = tw * 4;
            let mut pixels = vec![0u8; (stride * th) as usize];
            converter
                .CopyPixels(std::ptr::null(), stride, &mut pixels)
                .ok()?;
            Some(dominant_color(&pixels))
        })();
        if com {
            CoUninitialize();
        }
        result
    }
}

/// RGB -> (hue 0..360, saturation 0..1, value 0..1).
pub fn to_hsv(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d == 0.0 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / d).rem_euclid(6.0))
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (h, if max == 0.0 { 0.0 } else { d / max }, max)
}

pub fn from_hsv(h: f32, s: f32, v: f32) -> (f32, f32, f32) {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match (h.rem_euclid(360.0) / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (r + m, g + m, b + m)
}

/// A pleasant dominant color of BGRA `pixels`: the hue bucket with the most
/// colorful, bright-enough pixels (grey and near-black pixels hardly count), then
/// brought into a range that works as a glass tint (not too dull, not too dark).
/// A wallpaper with no color at all gives a soft blue-grey.
pub fn dominant_color(pixels: &[u8]) -> (u8, u8, u8) {
    const BINS: usize = 24;
    // Per hue bin: weight, and the weighted sum of s and v.
    let mut weight = [0.0f32; BINS];
    let mut sat = [0.0f32; BINS];
    let mut val = [0.0f32; BINS];
    for px in pixels.as_chunks::<4>().0 {
        let (r, g, b) = (
            f32::from(px[2]) / 255.0,
            f32::from(px[1]) / 255.0,
            f32::from(px[0]) / 255.0,
        );
        let (h, s, v) = to_hsv(r, g, b);
        // Dull or dark pixels barely vote.
        let w = (s * s) * v.min(0.9) * if v < 0.18 { 0.05 } else { 1.0 };
        if w <= 0.0 {
            continue;
        }
        let bin = ((h / 360.0 * BINS as f32) as usize).min(BINS - 1);
        weight[bin] += w;
        sat[bin] += w * s;
        val[bin] += w * v;
    }
    // Neighbouring bins vote together, so a hue split across two bins still wins.
    let score =
        |i: usize| weight[i] + 0.5 * (weight[(i + 1) % BINS] + weight[(i + BINS - 1) % BINS]);
    let best = (0..BINS).max_by(|&a, &b| score(a).total_cmp(&score(b)));
    let Some(best) = best.filter(|&i| weight[i] > 1.0e-4) else {
        return (0x5a, 0x6b, 0x85);
    };
    let h = (best as f32 + 0.5) / BINS as f32 * 360.0;
    let s = (sat[best] / weight[best]).clamp(0.40, 0.85);
    let v = (val[best] / weight[best]).clamp(0.38, 0.80);
    let (r, g, b) = from_hsv(h, s, v);
    (
        (r * 255.0).round() as u8,
        (g * 255.0).round() as u8,
        (b * 255.0).round() as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(colors: &[((u8, u8, u8), usize)]) -> Vec<u8> {
        colors
            .iter()
            .flat_map(|&((r, g, b), n)| std::iter::repeat_n([b, g, r, 255], n).flatten())
            .collect()
    }

    #[test]
    fn picks_the_colorful_hue_over_big_grey_areas() {
        // Mostly grey / black, a little saturated blue.
        let px = image(&[((90, 90, 90), 600), ((5, 5, 8), 300), ((30, 90, 220), 100)]);
        let (r, g, b) = dominant_color(&px);
        assert!(b > r && b > g, "blue wins: {r} {g} {b}");
    }

    #[test]
    fn result_is_never_too_dull_or_too_dark() {
        let px = image(&[((20, 6, 6), 500), ((60, 30, 30), 500)]);
        let (r, g, b) = dominant_color(&px);
        let (_, s, v) = to_hsv(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
        assert!(s >= 0.38 && v >= 0.36, "s={s} v={v}");
    }

    #[test]
    fn a_grey_wallpaper_gets_a_neutral_default() {
        let px = image(&[((128, 128, 128), 100)]);
        assert_eq!(dominant_color(&px), (0x5a, 0x6b, 0x85));
        assert_eq!(dominant_color(&[]), (0x5a, 0x6b, 0x85));
    }

    #[test]
    fn hsv_round_trips() {
        for &(r, g, b) in &[(0.8, 0.2, 0.1), (0.1, 0.7, 0.3), (0.2, 0.3, 0.9)] {
            let (h, s, v) = to_hsv(r, g, b);
            let (r2, g2, b2) = from_hsv(h, s, v);
            assert!((r - r2).abs() < 1e-3 && (g - g2).abs() < 1e-3 && (b - b2).abs() < 1e-3);
        }
    }
}
