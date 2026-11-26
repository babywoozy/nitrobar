//! "Island" mode: the bar collapses into a small pill and expands on hover.
//!
//! - `Island` is the animation state (0 = pill, 1 = full bar), advanced by a 16 ms
//!   timer that only runs while moving: idle CPU stays at zero.
//! - Geometry is in "bar space": DIPs relative to the full bar rectangle at the
//!   screen edge. While collapsed the window shrinks to the pill (plus a small hit
//!   margin), so it never covers clicks elsewhere and reserves no screen space.
//! - `EdgeSensor` is a 1 px tall, nearly invisible window along the screen edge:
//!   touching the edge anywhere expands the bar (no cursor polling).

use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONULL, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GWL_STYLE, GWLP_USERDATA, GetClassNameW,
    GetForegroundWindow, GetWindowLongPtrW, GetWindowLongW, GetWindowRect, HWND_TOPMOST, IDC_ARROW,
    IsZoomed, LWA_ALPHA, LoadCursorW, PostMessageW, RegisterClassExW, SW_HIDE, SW_SHOWNOACTIVATE,
    SWP_NOACTIVATE, SetLayeredWindowAttributes, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    WM_APP, WM_MOUSEMOVE, WNDCLASSEXW, WS_CAPTION, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{PCWSTR, w};

use crate::config::BarConfig;
use crate::tasks;

/// Posted by the edge sensor to the bar: wParam 1 = pointer entered, 0 = left.
pub const WM_APP_EDGE: u32 = WM_APP + 7;
/// Sent after `TrackMouseEvent(TME_LEAVE)` when the pointer leaves the window.
const WM_MOUSELEAVE: u32 = 0x02A3;

pub const EXPAND_DURATION: Duration = Duration::from_millis(180);
pub const COLLAPSE_DURATION: Duration = Duration::from_millis(200);
/// Animation frame interval (~60 fps; the system timer granularity is ~15.6 ms).
pub const FRAME_MS: u32 = 16;
/// Gap between the pill and the screen edge, DIPs.
const PILL_GAP: f32 = 4.0;
/// Extra hover area around the pill, DIPs.
const HIT_MARGIN: f32 = 8.0;
/// Minimum pill size when it shows the clock, DIPs.
const CLOCK_PILL_WIDTH: f32 = 64.0;
const CLOCK_PILL_HEIGHT: f32 = 20.0;

pub fn ease_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

pub fn lerp_rect(a: &D2D_RECT_F, b: &D2D_RECT_F, t: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left: lerp(a.left, b.left, t),
        top: lerp(a.top, b.top, t),
        right: lerp(a.right, b.right, t),
        bottom: lerp(a.bottom, b.bottom, t),
    }
}

/// Expansion animation. `position` goes 0 (pill) -> 1 (full bar) linearly in
/// time; drawing uses `eased()`. Reversing mid-way continues from where it is.
#[derive(Clone, Copy, Debug)]
pub struct Island {
    position: f32,
    from: f32,
    target: f32,
    start: Instant,
    duration: Duration,
}

impl Island {
    pub fn collapsed() -> Self {
        Self {
            position: 0.0,
            from: 0.0,
            target: 0.0,
            start: Instant::now(),
            duration: Duration::ZERO,
        }
    }

    pub fn expanded(&self) -> bool {
        self.target == 1.0
    }

    pub fn is_collapsed(&self) -> bool {
        self.target == 0.0 && self.position == 0.0
    }

    pub fn animating(&self) -> bool {
        self.position != self.target
    }

    pub fn eased(&self) -> f32 {
        ease_out_cubic(self.position)
    }

    /// Starts moving toward expanded (`true`) or collapsed. Returns false if
    /// already there / already heading there.
    pub fn set_target(&mut self, expand: bool, now: Instant) -> bool {
        let target = if expand { 1.0 } else { 0.0 };
        if self.target == target {
            return false;
        }
        let full = if expand {
            EXPAND_DURATION
        } else {
            COLLAPSE_DURATION
        };
        self.from = self.position;
        self.target = target;
        self.start = now;
        // Only the remaining distance takes time when reversing mid-way.
        self.duration = full.mul_f32((target - self.position).abs());
        true
    }

    /// Advances the animation. Returns true while still moving.
    pub fn tick(&mut self, now: Instant) -> bool {
        if !self.animating() {
            return false;
        }
        let elapsed = now.saturating_duration_since(self.start);
        let t = if self.duration.is_zero() {
            1.0
        } else {
            (elapsed.as_secs_f32() / self.duration.as_secs_f32()).min(1.0)
        };
        self.position = lerp(self.from, self.target, t);
        if t >= 1.0 {
            self.position = self.target;
        }
        self.animating()
    }
}

/// Island geometry in bar space (DIPs; the full bar is `0, 0, width, height`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    pub bar: D2D_RECT_F,
    pub pill: D2D_RECT_F,
    /// Window rectangle while collapsed: the pill plus a hover margin, reaching
    /// the screen edge.
    pub hit: D2D_RECT_F,
}

pub fn geometry(width: f32, height: f32, config: &BarConfig, at_top: bool) -> Geometry {
    let (mut pill_w, mut pill_h) = (config.island_width, config.island_height);
    if config.island_show_clock {
        pill_w = pill_w.max(CLOCK_PILL_WIDTH);
        pill_h = pill_h.max(CLOCK_PILL_HEIGHT);
    }
    let pill_w = pill_w.min(width);
    let pill_h = pill_h.min(height - PILL_GAP);
    let left = ((width - pill_w) / 2.0).round();
    let top = if at_top {
        PILL_GAP
    } else {
        height - PILL_GAP - pill_h
    };
    let pill = D2D_RECT_F {
        left,
        top,
        right: left + pill_w,
        bottom: top + pill_h,
    };
    let hit = D2D_RECT_F {
        left: (pill.left - HIT_MARGIN).max(0.0),
        top: if at_top {
            0.0
        } else {
            (pill.top - HIT_MARGIN).max(0.0)
        },
        right: (pill.right + HIT_MARGIN).min(width),
        bottom: if at_top {
            (pill.bottom + HIT_MARGIN).min(height)
        } else {
            height
        },
    };
    Geometry {
        bar: D2D_RECT_F {
            left: 0.0,
            top: 0.0,
            right: width,
            bottom: height,
        },
        pill,
        hit,
    }
}

/// Windows' own flyouts that take the foreground: Start / search, Quick Settings,
/// notification center. While one is open the bar stays expanded (and pressing
/// Win, which opens Start, expands it).
pub fn is_shell_flyout(hwnd: HWND) -> bool {
    const FLYOUT_EXES: &[&str] = &[
        "searchhost.exe",
        "startmenuexperiencehost.exe",
        "shellexperiencehost.exe",
        "shellhost.exe",
    ];
    if hwnd.is_invalid() {
        return false;
    }
    tasks::process_exe(hwnd).is_some_and(|exe| {
        let name = exe.rsplit('\\').next().unwrap_or(&exe).to_ascii_lowercase();
        FLYOUT_EXES.contains(&name.as_str())
    })
}

/// Whether the foreground window is a fullscreen app (game, video) on `monitor`:
/// covers the whole monitor, is not maximized and has no caption. The desktop
/// and shell flyouts don't count.
pub fn foreground_is_fullscreen(monitor_rect: &RECT) -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_invalid() || IsZoomed(fg).as_bool() {
            return false;
        }
        if GetWindowLongW(fg, GWL_STYLE) as u32 & WS_CAPTION.0 == WS_CAPTION.0 {
            return false;
        }
        let mut class = [0u16; 32];
        let len = GetClassNameW(fg, &mut class) as usize;
        let class = String::from_utf16_lossy(&class[..len]);
        if class == "Progman" || class == "WorkerW" || is_shell_flyout(fg) {
            return false;
        }
        let monitor = MonitorFromWindow(fg, MONITOR_DEFAULTTONULL);
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(monitor, &raw mut info).as_bool() || info.rcMonitor != *monitor_rect {
            return false;
        }
        let mut rc = RECT::default();
        let _ = GetWindowRect(fg, &raw mut rc);
        rc.left <= monitor_rect.left
            && rc.top <= monitor_rect.top
            && rc.right >= monitor_rect.right
            && rc.bottom >= monitor_rect.bottom
    }
}

const SENSOR_CLASS: PCWSTR = w!("NitrobarEdgeSensor");

/// A 1 px strip along the screen edge that reports pointer enter / leave to the bar.
pub struct EdgeSensor {
    hwnd: HWND,
}

extern "system" fn sensor_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let bar = || HWND(unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut _);
    match msg {
        WM_MOUSEMOVE => unsafe {
            let mut track = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            let _ = TrackMouseEvent(&raw mut track);
            let _ = PostMessageW(Some(bar()), WM_APP_EDGE, WPARAM(1), LPARAM(0));
            LRESULT(0)
        },
        WM_MOUSELEAVE => unsafe {
            let _ = PostMessageW(Some(bar()), WM_APP_EDGE, WPARAM(0), LPARAM(0));
            LRESULT(0)
        },
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

impl EdgeSensor {
    pub fn new(bar: HWND) -> Option<Self> {
        unsafe {
            let instance = GetModuleHandleW(None).ok()?.into();
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(sensor_proc),
                hInstance: instance,
                hCursor: LoadCursorW(None, IDC_ARROW).ok()?,
                lpszClassName: SENSOR_CLASS,
                ..Default::default()
            };
            // Fails harmlessly if already registered (config reload).
            RegisterClassExW(&raw const class);
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_LAYERED,
                SENSOR_CLASS,
                w!(""),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance),
                None,
            )
            .ok()?;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, bar.0 as isize);
            // Alpha 1/255: invisible, but still hit-tested (alpha 0 is not).
            let _ = SetLayeredWindowAttributes(hwnd, Default::default(), 1, LWA_ALPHA);
            Some(Self { hwnd })
        }
    }

    /// Places the strip along the edge of `monitor` and shows it (or hides it).
    pub fn place(&self, monitor: &RECT, at_top: bool, visible: bool) {
        unsafe {
            if !visible {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
                return;
            }
            let y = if at_top {
                monitor.top
            } else {
                monitor.bottom - 1
            };
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                monitor.left,
                y,
                monitor.right - monitor.left,
                1,
                SWP_NOACTIVATE,
            );
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
    }
}

impl Drop for EdgeSensor {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easing_is_monotonic_and_bounded() {
        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert_eq!(ease_out_cubic(1.0), 1.0);
        assert!(ease_out_cubic(0.5) > 0.5, "ease-out moves fast first");
        let mut last = 0.0;
        for i in 0..=20 {
            let v = ease_out_cubic(i as f32 / 20.0);
            assert!(v >= last);
            last = v;
        }
    }

    #[test]
    fn animation_runs_and_reverses() {
        let t0 = Instant::now();
        let mut island = Island::collapsed();
        assert!(island.set_target(true, t0));
        assert!(!island.set_target(true, t0), "already heading there");
        assert!(island.tick(t0 + EXPAND_DURATION / 2));
        let half = island.eased();
        assert!(half > 0.5 && half < 1.0);
        // Reverse mid-way: collapsing takes only the remaining distance.
        let t1 = t0 + EXPAND_DURATION / 2;
        assert!(island.set_target(false, t1));
        assert!(!island.tick(t1 + COLLAPSE_DURATION));
        assert!(island.is_collapsed());
    }

    #[test]
    fn pill_is_centered_at_the_edge() {
        let config = BarConfig::default();
        let g = geometry(1920.0, 48.0, &config, false);
        assert_eq!(g.pill.right - g.pill.left, config.island_width);
        assert_eq!((g.pill.left + g.pill.right) / 2.0, 960.0);
        assert_eq!(g.pill.bottom, 48.0 - PILL_GAP);
        assert_eq!(g.hit.bottom, 48.0);
        let top = geometry(1920.0, 48.0, &config, true);
        assert_eq!(top.pill.top, PILL_GAP);
        assert_eq!(top.hit.top, 0.0);
        let clock = BarConfig {
            island_show_clock: true,
            ..BarConfig::default()
        };
        let g = geometry(1920.0, 48.0, &clock, false);
        assert!(g.pill.bottom - g.pill.top >= CLOCK_PILL_HEIGHT);
    }
}
