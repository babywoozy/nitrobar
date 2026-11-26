//! What is playing: the system media session (the one Windows' own media flyout
//! shows), its title, artist and cover art, plus the equalizer bars.
//!
//! Everything WinRT runs on one worker thread that sleeps until a session event
//! arrives (no polling). It publishes a snapshot and posts `WM_APP_MEDIA`.
//! The bars follow the loudness of the default output (`IAudioMeterInformation`);
//! they are sampled only while music is playing and the pill is visible.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

use windows::Foundation::TypedEventHandler;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
};
use windows::Storage::Streams::IRandomAccessStreamReference;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppBGRA, IWICImagingFactory,
    WICBitmapDitherTypeNone, WICBitmapInterpolationModeFant, WICBitmapPaletteTypeCustom,
    WICDecodeMetadataCacheOnDemand, WICRect,
};
use windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation;
use windows::Win32::Media::Audio::{IMMDeviceEnumerator, MMDeviceEnumerator, eConsole, eRender};
use windows::Win32::System::Com::{
    CLSCTX_ALL, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
    IStream,
};
use windows::Win32::System::WinRT::CreateStreamOverRandomAccessStream;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::wallpaper;

/// Posted to the bar when the snapshot changed.
pub const WM_APP_MEDIA: u32 = WM_APP + 12;

/// Cover art size in pixels the UI wants (set from the DPI and pill height).
static ART_PX: AtomicU32 = AtomicU32::new(30);
static CURRENT: Mutex<Option<Arc<Track>>> = Mutex::new(None);

pub struct Track {
    pub title: Vec<u16>,
    pub artist: Vec<u16>,
    pub playing: bool,
    /// Round, premultiplied BGRA cover, `art_px` square.
    pub art: Option<Vec<u8>>,
    pub art_px: u32,
    /// Dominant color of the cover.
    pub accent: Option<(u8, u8, u8)>,
}

/// The latest snapshot (`None`: no media session).
pub fn current() -> Option<Arc<Track>> {
    CURRENT.lock().ok().and_then(|c| c.clone())
}

/// Handle to the worker; sending makes it read the session again.
pub struct Media {
    wake: Sender<()>,
}

impl Media {
    /// Starts the worker; it reports to `bar`.
    pub fn start(bar: HWND) -> Option<Self> {
        let (wake, events) = channel::<()>();
        let from_events = wake.clone();
        let target = bar.0 as usize;
        std::thread::Builder::new()
            .name("nitrobar-media".into())
            .spawn(move || {
                unsafe {
                    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                }
                let Some(manager) = Manager::RequestAsync().ok().and_then(|op| op.join().ok())
                else {
                    return;
                };
                let on_change = {
                    let tx = from_events.clone();
                    TypedEventHandler::new(move |_, _| {
                        let _ = tx.send(());
                        Ok(())
                    })
                };
                let _ = manager.CurrentSessionChanged(&on_change);
                loop {
                    let session = manager.GetCurrentSession().ok();
                    // (Re-)subscribe to whatever session is current now.
                    // Dropped (unsubscribed) at the end of this round.
                    let _watched = session.as_ref().map(|s| Watched::new(s, &from_events));
                    let track = session.as_ref().and_then(read);
                    if let Ok(mut current) = CURRENT.lock() {
                        *current = track.map(Arc::new);
                    }
                    unsafe {
                        let _ = PostMessageW(
                            Some(HWND(target as *mut _)),
                            WM_APP_MEDIA,
                            WPARAM(0),
                            LPARAM(0),
                        );
                    }
                    if events.recv().is_err() {
                        break;
                    }
                    // Several events usually arrive together.
                    std::thread::sleep(std::time::Duration::from_millis(60));
                    while events.try_recv().is_ok() {}
                }
            })
            .ok()?;
        Some(Self { wake })
    }

    /// The wanted cover size changed (DPI, pill height): decode it again.
    pub fn set_art_size(&self, px: u32) {
        if px > 0 && ART_PX.swap(px, Ordering::AcqRel) != px {
            let _ = self.wake.send(());
        }
    }
}

/// Event subscriptions on one session, removed on drop.
struct Watched {
    session: Session,
    properties: Option<i64>,
    playback: Option<i64>,
}

impl Watched {
    fn new(session: &Session, wake: &Sender<()>) -> Self {
        let tx = wake.clone();
        let properties = session
            .MediaPropertiesChanged(&TypedEventHandler::new(move |_, _| {
                let _ = tx.send(());
                Ok(())
            }))
            .ok();
        let tx = wake.clone();
        let playback = session
            .PlaybackInfoChanged(&TypedEventHandler::new(move |_, _| {
                let _ = tx.send(());
                Ok(())
            }))
            .ok();
        Self {
            session: session.clone(),
            properties,
            playback,
        }
    }
}

impl Drop for Watched {
    fn drop(&mut self) {
        if let Some(token) = self.properties {
            let _ = self.session.RemoveMediaPropertiesChanged(token);
        }
        if let Some(token) = self.playback {
            let _ = self.session.RemovePlaybackInfoChanged(token);
        }
    }
}

fn read(session: &Session) -> Option<Track> {
    let playing = session
        .GetPlaybackInfo()
        .and_then(|i| i.PlaybackStatus())
        .is_ok_and(|s| s == PlaybackStatus::Playing);
    let properties = session.TryGetMediaPropertiesAsync().ok()?.join().ok()?;
    let title: Vec<u16> = properties.Title().ok()?.to_vec();
    if title.is_empty() {
        return None;
    }
    let artist = properties.Artist().map(|a| a.to_vec()).unwrap_or_default();
    let art_px = ART_PX.load(Ordering::Acquire);
    let cover = properties
        .Thumbnail()
        .ok()
        .and_then(|t| decode_cover(&t, art_px));
    let (art, accent) = match cover {
        Some(mut pixels) => {
            let accent = wallpaper::dominant_color(&pixels);
            round_mask(&mut pixels, art_px);
            (Some(pixels), Some(accent))
        }
        None => (None, None),
    };
    Some(Track {
        title,
        artist,
        playing,
        art,
        art_px,
        accent,
    })
}

/// The cover, scaled to fill a `size` square (center crop), straight BGRA.
fn decode_cover(reference: &IRandomAccessStreamReference, size: u32) -> Option<Vec<u8>> {
    let stream = reference.OpenReadAsync().ok()?.join().ok()?;
    unsafe {
        let stream: IStream = CreateStreamOverRandomAccessStream(&stream).ok()?;
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let decoder = factory
            .CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)
            .ok()?;
        let frame = decoder.GetFrame(0).ok()?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&raw mut w, &raw mut h).ok()?;
        if w == 0 || h == 0 || size == 0 {
            return None;
        }
        let k = size as f32 / w.min(h) as f32;
        let (sw, sh) = (
            ((w as f32 * k).round() as u32).max(size),
            ((h as f32 * k).round() as u32).max(size),
        );
        let scaler = factory.CreateBitmapScaler().ok()?;
        scaler
            .Initialize(&frame, sw, sh, WICBitmapInterpolationModeFant)
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
        let crop = WICRect {
            X: ((sw - size) / 2) as i32,
            Y: ((sh - size) / 2) as i32,
            Width: size as i32,
            Height: size as i32,
        };
        let stride = size * 4;
        let mut pixels = vec![0u8; (stride * size) as usize];
        converter
            .CopyPixels(&raw const crop, stride, &mut pixels)
            .ok()?;
        Some(pixels)
    }
}

/// Cuts a `size` square BGRA image into an anti-aliased circle, premultiplied.
fn round_mask(pixels: &mut [u8], size: u32) {
    let r = size as f32 / 2.0;
    for (i, px) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let (x, y) = ((i as u32 % size) as f32, (i as u32 / size) as f32);
        let d = ((x + 0.5 - r).powi(2) + (y + 0.5 - r).powi(2)).sqrt();
        let coverage = (r - d + 0.5).clamp(0.0, 1.0);
        let alpha = coverage * f32::from(px[3]) / 255.0;
        for c in &mut px[..3] {
            *c = (f32::from(*c) * alpha).round() as u8;
        }
        px[3] = (alpha * 255.0).round() as u8;
    }
}

// ----- equalizer bars -----

/// Loudness of the default output device, 0..1.
pub struct Meter(IAudioMeterInformation);

impl Meter {
    pub fn new() -> Option<Self> {
        unsafe {
            let devices: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
            let device = devices.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
            device.Activate(CLSCTX_ALL, None).ok().map(Self)
        }
    }

    pub fn peak(&self) -> f32 {
        unsafe { self.0.GetPeakValue() }.unwrap_or(0.0)
    }
}

pub const BARS: usize = 4;

/// Four bars driven by one loudness value: each follows it with its own rhythm,
/// rises fast and falls slower. An automatic gain keeps them lively at any volume.
#[derive(Clone, Copy)]
pub struct Bars {
    pub levels: [f32; BARS],
    reference: f32,
    time: f32,
}

impl Default for Bars {
    fn default() -> Self {
        Self {
            levels: [0.0; BARS],
            reference: 0.05,
            time: 0.0,
        }
    }
}

impl Bars {
    /// Advances by `dt` seconds with the current `peak`.
    pub fn step(&mut self, peak: f32, dt: f32) {
        const RHYTHM: [(f32, f32); BARS] = [(9.3, 0.0), (13.1, 1.7), (7.7, 3.1), (11.9, 4.4)];
        self.time += dt;
        self.reference = (self.reference * (-dt / 3.0).exp()).max(peak).max(0.02);
        let loud = (peak / self.reference).clamp(0.0, 1.0);
        for (level, (speed, phase)) in self.levels.iter_mut().zip(RHYTHM) {
            let wave = (self.time * speed + phase).sin().abs();
            let target = if peak < 0.001 {
                0.0
            } else {
                loud * (0.25 + 0.75 * wave)
            };
            let tau = if target > *level { 0.035 } else { 0.11 };
            *level += (target - *level) * (1.0 - (-dt / tau).exp());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bars_dance_with_sound_and_fall_silent() {
        let mut bars = Bars::default();
        let mut seen = [0.0f32; BARS];
        for _ in 0..60 {
            bars.step(0.4, 1.0 / 30.0);
            for (s, l) in seen.iter_mut().zip(bars.levels) {
                *s = s.max(l);
            }
        }
        assert!(seen.iter().all(|&l| l > 0.5 && l <= 1.0), "{seen:?}");
        assert!(bars.levels.windows(2).any(|w| (w[0] - w[1]).abs() > 0.02));
        for _ in 0..60 {
            bars.step(0.0, 1.0 / 30.0);
        }
        assert!(bars.levels.iter().all(|&l| l < 0.01));
    }

    #[test]
    fn quiet_music_still_fills_the_bars() {
        let mut bars = Bars::default();
        let mut best = 0.0f32;
        for _ in 0..200 {
            bars.step(0.03, 1.0 / 30.0);
            best = best.max(bars.levels[0]);
        }
        assert!(best > 0.6, "automatic gain: {best}");
    }

    #[test]
    fn cover_is_cut_into_a_circle() {
        let mut px = vec![200u8; 10 * 10 * 4];
        px.iter_mut().skip(3).step_by(4).for_each(|a| *a = 255);
        round_mask(&mut px, 10);
        assert_eq!(px[3], 0, "corner is transparent");
        let center = (5 * 10 + 5) * 4;
        assert_eq!(&px[center..center + 4], &[200, 200, 200, 255]);
    }
}
