//! Network, volume and battery status (right side of the bar).
//!
//! Every source is event-driven. Callbacks arrive on OS worker threads, so they
//! only set a bit in `PENDING` and post one `WM_APP_STATUS`; the UI thread then
//! re-reads the affected source and repaints only if what is shown changed.

pub mod battery;
pub mod network;
pub mod volume;

use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use battery::BatteryState;
use network::NetState;
use volume::VolumeState;

/// Posted to the bar when any status source has news (see `take_pending`).
pub const WM_APP_STATUS: u32 = WM_APP + 4;

pub const SOURCE_NETWORK: u32 = 1;
pub const SOURCE_VOLUME: u32 = 1 << 1;
pub const SOURCE_AUDIO_DEVICE: u32 = 1 << 2;

static BAR: AtomicIsize = AtomicIsize::new(0);
static PENDING: AtomicU32 = AtomicU32::new(0);

/// Called from any thread. Posts at most one message until the UI thread catches up.
pub fn notify(source: u32) {
    if PENDING.fetch_or(source, Ordering::SeqCst) == 0 {
        let bar = BAR.load(Ordering::SeqCst);
        if bar != 0 {
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(bar as *mut _)),
                    WM_APP_STATUS,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        }
    }
}

/// Which sources changed since the last call (UI thread).
pub fn take_pending() -> u32 {
    PENDING.swap(0, Ordering::SeqCst)
}

// Segoe Fluent Icons code points (checked against the font, see CLAUDE.md).
const GLYPH_WIFI_FULL: u16 = 0xE701;
const GLYPH_BLANK: u16 = 0x0020;
const GLYPH_WIFI_BARS: [u16; 4] = [0xE872, 0xE873, 0xE874, 0xE701];
const GLYPH_ETHERNET: u16 = 0xE839;
const GLYPH_OFFLINE: u16 = 0xF384;
const GLYPH_MUTED: u16 = 0xE74F;
const GLYPH_VOLUME: [u16; 4] = [0xE992, 0xE993, 0xE994, 0xE995];
const GLYPH_BATTERY_0: u16 = 0xEBA0;
const GLYPH_BATTERY_CHARGING_0: u16 = 0xEBAB;

/// One icon cell: an optional dimmed "track" glyph with the real glyph on top.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StatusIcon {
    pub glyph: u16,
    pub track: Option<u16>,
}

/// What the status button shows; compared to decide whether to repaint.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct StatusView {
    pub icons: Vec<StatusIcon>,
    pub percent: Option<Vec<u16>>,
}

pub struct Status {
    volume: Option<volume::Volume>,
    power: battery::PowerNotifications,
    pub net: NetState,
    pub vol: Option<VolumeState>,
    pub battery: Option<BatteryState>,
}

impl Status {
    /// Starts all event sources. Requires COM on the calling (UI) thread.
    /// Network setup runs on a worker thread and reports back via `notify`.
    pub fn new(bar: HWND) -> Self {
        BAR.store(bar.0 as isize, Ordering::SeqCst);
        network::start();
        let volume = volume::Volume::new()
            .inspect_err(|_err| debug_log!("nitrobar: volume unavailable: {_err}"))
            .ok();
        Self {
            net: network::current(),
            vol: volume::current(),
            battery: battery::read(),
            volume,
            power: battery::PowerNotifications::register(bar),
        }
    }

    /// Re-reads the given sources (bits from `take_pending`). Cheap except for a
    /// default-device change, which re-binds to the new audio endpoint.
    pub fn refresh(&mut self, sources: u32) {
        if sources & SOURCE_AUDIO_DEVICE != 0
            && let Some(volume) = &mut self.volume
        {
            volume.rebind();
        }
        if sources & (SOURCE_VOLUME | SOURCE_AUDIO_DEVICE) != 0 {
            self.vol = volume::current();
        }
        if sources & SOURCE_NETWORK != 0 {
            self.net = network::current();
        }
    }

    /// WM_POWERBROADCAST: AC line or battery level changed.
    pub fn refresh_battery(&mut self) {
        self.battery = battery::read();
    }

    pub fn view(&self) -> StatusView {
        let mut icons = Vec::with_capacity(3);
        icons.push(match self.net {
            NetState::Wifi {
                bars,
                internet: true,
            } => StatusIcon {
                glyph: GLYPH_WIFI_BARS[usize::from(bars.clamp(1, 4)) - 1],
                track: Some(GLYPH_WIFI_FULL),
            },
            // Still starting: just the dimmed Wi-Fi outline, not a misleading "offline".
            NetState::Unknown => StatusIcon {
                glyph: GLYPH_BLANK,
                track: Some(GLYPH_WIFI_FULL),
            },
            NetState::Ethernet { internet: true } => StatusIcon {
                glyph: GLYPH_ETHERNET,
                track: None,
            },
            _ => StatusIcon {
                glyph: GLYPH_OFFLINE,
                track: None,
            },
        });
        if let Some(vol) = self.vol {
            let glyph = if vol.muted {
                GLYPH_MUTED
            } else {
                let step = (vol.level * 3.0).ceil().clamp(0.0, 3.0) as usize;
                GLYPH_VOLUME[step]
            };
            icons.push(StatusIcon { glyph, track: None });
        }
        let mut percent = None;
        if let Some(battery) = self.battery {
            let step = u16::from((battery.percent.min(100) + 5) / 10);
            let base = if battery.charging {
                GLYPH_BATTERY_CHARGING_0
            } else {
                GLYPH_BATTERY_0
            };
            icons.push(StatusIcon {
                glyph: base + step,
                track: None,
            });
            percent = Some(format!("{}%", battery.percent).encode_utf16().collect());
        }
        StatusView { icons, percent }
    }

    pub fn shutdown(&mut self) {
        self.volume = None;
        network::stop();
        self.power.unregister();
        BAR.store(0, Ordering::SeqCst);
    }
}
