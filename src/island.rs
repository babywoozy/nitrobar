//! "Island" mode: the bar collapses into a small pill and expands on hover.
//!
//! - `Island` is the animation state (0 = pill, 1 = full bar), advanced once per
//!   display frame by `morph::start_clock`, which only runs while something moves:
//!   idle CPU stays at zero.
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
use crate::icons::Icon;
use crate::layout::{CLOCK_WIDTH, STATUS_GAP};
use crate::morph::spring;
use crate::tasks;

/// Posted by the edge sensor to the bar: wParam 1 = pointer entered, 0 = left.
pub const WM_APP_EDGE: u32 = WM_APP + 7;
/// Sent after `TrackMouseEvent(TME_LEAVE)` when the pointer leaves the window.
const WM_MOUSELEAVE: u32 = 0x02A3;

pub const EXPAND_DURATION: Duration = Duration::from_millis(480);
pub const COLLAPSE_DURATION: Duration = Duration::from_millis(260);
/// Spring (rad/s, damping ratio) of the expansion: a small overshoot.
const EXPAND_SPRING: (f32, f32) = (22.0, 0.72);
/// Extra hover area around the pill, DIPs. Also room for its soft shadow.
pub const HIT_MARGIN: f32 = 14.0;
/// "Activity" pill shown while an app goes into / comes out of the island: grows
/// by this much in width, to this height (DIPs), and holds for `PULSE_HOLD`.
const ACTIVITY_EXTRA_WIDTH: f32 = 56.0;
const ACTIVITY_EXTRA_HEIGHT: f32 = 8.0;
pub const PULSE_HOLD: Duration = Duration::from_millis(1500);
/// Springs (rad/s, damping ratio): a lively bounce out, a calmer return.
const PULSE_OUT: (f32, f32) = (20.0, 0.42);
const PULSE_BACK: (f32, f32) = (18.0, 0.75);
/// Time the return spring needs to settle.
const PULSE_SETTLE: f32 = 0.55;

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

    /// Panel progress: 0 = pill, 1 = full. Expanding follows a spring and may
    /// overshoot 1 a little; collapsing is a plain smooth ease.
    pub fn eased(&self) -> f32 {
        if self.position >= 1.0 {
            return 1.0;
        }
        if self.target == 1.0 {
            spring(
                self.position * EXPAND_DURATION.as_secs_f32(),
                EXPAND_SPRING.0,
                EXPAND_SPRING.1,
            )
        } else {
            crate::morph::ease_in_out_cubic(self.position)
        }
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

/// The island "swallowing" a window (or spitting one out): the pill bounces up
/// to the activity size with the app's icon, holds, then springs back.
pub struct Pulse {
    start: Instant,
    pub icon: Option<Icon>,
}

impl Pulse {
    pub fn new(icon: Option<Icon>, now: Instant) -> Self {
        Self { start: now, icon }
    }

    /// 0 = plain pill, 1 = activity size (overshoots both ways while springing).
    pub fn value(&self, now: Instant) -> f32 {
        let t = now.saturating_duration_since(self.start).as_secs_f32();
        let hold = PULSE_HOLD.as_secs_f32();
        if t < hold {
            spring(t, PULSE_OUT.0, PULSE_OUT.1)
        } else {
            1.0 - spring(t - hold, PULSE_BACK.0, PULSE_BACK.1)
        }
    }

    pub fn started(&self) -> Instant {
        self.start
    }

    pub fn done(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.start).as_secs_f32()
            > PULSE_HOLD.as_secs_f32() + PULSE_SETTLE
    }

    /// Whether the bounce is moving (the hold in between is still).
    pub fn moving(&self, now: Instant) -> bool {
        let t = now.saturating_duration_since(self.start).as_secs_f32();
        let hold = PULSE_HOLD.as_secs_f32();
        t < PULSE_SETTLE.max(0.6) || t > hold
    }
}

/// Island geometry in bar space (DIPs). Bar space is the strip along the screen
/// edge: full monitor width, `strip_height` tall.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    /// The pill at rest.
    pub pill: D2D_RECT_F,
    /// Window rectangle while collapsed: the pill plus a hover / shadow margin,
    /// reaching the screen edge.
    pub hit: D2D_RECT_F,
    /// The pill grown for an app going in / out (`Pulse`), and the window rect
    /// that holds it with its overshoot.
    pub activity: D2D_RECT_F,
    pub activity_hit: D2D_RECT_F,
}

/// The pill at pulse value `v` (may overshoot past the activity size).
pub fn pulsed_pill(g: &Geometry, v: f32) -> D2D_RECT_F {
    lerp_rect(&g.pill, &g.activity, v)
}

/// Height of the strip the island lives in: gap + the tallest of its shapes (rest,
/// activity, expanded) + room for the shadow and the bounce.
pub fn strip_height(config: &BarConfig, icon_size: f32) -> f32 {
    let activity = config.island_height + ACTIVITY_EXTRA_HEIGHT;
    let expanded = config.island_height.max(icon_size + 28.0);
    config.island_gap + activity.max(expanded) + HIT_MARGIN + 8.0
}

/// A band `dist` away from the screen edge, `h` tall, inside the strip: (top, bottom).
fn band_y(at_top: bool, strip_height: f32, dist: f32, h: f32) -> (f32, f32) {
    if at_top {
        (dist, dist + h)
    } else {
        (strip_height - dist - h, strip_height - dist)
    }
}

/// Window rect for a pill-like shape: grown by `margin_x` sideways and `margin_y`
/// inward, reaching the screen edge.
fn edge_window(
    r: &D2D_RECT_F,
    margin_x: f32,
    margin_y: f32,
    width: f32,
    strip_height: f32,
    at_top: bool,
) -> D2D_RECT_F {
    D2D_RECT_F {
        left: (r.left - margin_x).max(0.0),
        right: (r.right + margin_x).min(width),
        top: if at_top {
            0.0
        } else {
            (r.top - margin_y).max(0.0)
        },
        bottom: if at_top {
            (r.bottom + margin_y).min(strip_height)
        } else {
            strip_height
        },
    }
}

pub fn geometry(width: f32, strip_height: f32, config: &BarConfig, at_top: bool) -> Geometry {
    let band = |dist: f32, h: f32| band_y(at_top, strip_height, dist, h);
    let window =
        |r: &D2D_RECT_F, mx: f32, my: f32| edge_window(r, mx, my, width, strip_height, at_top);
    let gap = config.island_gap;
    let pill_w = config.island_width.min(width);
    let pill_h = config.island_height.min(strip_height - gap);
    let (top, bottom) = band(gap, pill_h);
    let left = ((width - pill_w) / 2.0).round();
    let pill = D2D_RECT_F {
        left,
        top,
        right: left + pill_w,
        bottom,
    };
    let hit = window(&pill, HIT_MARGIN, HIT_MARGIN);

    let act_w = (pill_w + ACTIVITY_EXTRA_WIDTH).min(width);
    let act_h = (pill_h + ACTIVITY_EXTRA_HEIGHT).min(strip_height - gap);
    let (top, bottom) = band(gap, act_h);
    let left = ((width - act_w) / 2.0).round();
    let activity = D2D_RECT_F {
        left,
        top,
        right: left + act_w,
        bottom,
    };
    // Room for the bounce overshoot (~25% past the activity size).
    let activity_hit = window(
        &activity,
        (act_w - pill_w) * 0.25 + HIT_MARGIN,
        (act_h - pill_h) * 0.25 + HIT_MARGIN,
    );
    Geometry {
        activity,
        activity_hit,
        pill,
        hit,
    }
}

/// Space around the icons inside the expanded pill, between buttons, and around the divider.
const STRIP_PAD: f32 = 12.0;
const STRIP_GAP: f32 = 2.0;
const DIVIDER_SPACE: f32 = 8.0;
/// Height of the divider line between the apps and the status area.
const DIVIDER_HEIGHT: f32 = 22.0;

/// The pill expanded into the strip of app icons (bar space, DIPs). Buttons are
/// laid out at their final positions; the panel grows from the pill to `panel`.
#[derive(Clone, Debug, PartialEq)]
pub struct StripLayout {
    pub panel: D2D_RECT_F,
    /// Window rect that holds the panel, its shadow and the spring overshoot.
    pub window: D2D_RECT_F,
    pub buttons: Vec<D2D_RECT_F>,
    pub divider: Option<D2D_RECT_F>,
    /// Zero width when there is nothing to show.
    pub status: D2D_RECT_F,
    pub clock: D2D_RECT_F,
}

/// Lays out `items` app buttons, then a divider, then `status_width` and the clock,
/// in one rounded panel centered on the screen. Buttons that don't fit are dropped.
pub fn strip_layout(
    width: f32,
    strip_height: f32,
    config: &BarConfig,
    icon_size: f32,
    items: usize,
    status_width: f32,
    at_top: bool,
) -> StripLayout {
    let button = icon_size + 16.0;
    let panel_h = (button + 12.0).max(config.island_height);
    let status_gap = if status_width > 0.0 { STATUS_GAP } else { 0.0 };
    let tail = status_width + status_gap + CLOCK_WIDTH;
    let divider_w = |apps: bool| {
        if apps { DIVIDER_SPACE * 2.0 + 1.0 } else { 0.0 }
    };
    let fixed = STRIP_PAD * 2.0 + tail + divider_w(items > 0);
    let room = (width - 32.0 - fixed + STRIP_GAP) / (button + STRIP_GAP);
    let n = items.min(room.floor().max(0.0) as usize);
    let apps_w = if n > 0 {
        n as f32 * button + (n - 1) as f32 * STRIP_GAP
    } else {
        0.0
    };
    let panel_w = (STRIP_PAD * 2.0 + apps_w + divider_w(n > 0) + tail)
        .max(config.island_width)
        .min(width);
    let left = ((width - panel_w) / 2.0).round();
    let (top, bottom) = band_y(at_top, strip_height, config.island_gap, panel_h);
    let panel = D2D_RECT_F {
        left,
        top,
        right: left + panel_w,
        bottom,
    };
    // Content row, vertically centered in the panel.
    let row_top = top + (panel_h - button) / 2.0;
    let row = |l: f32, w: f32| D2D_RECT_F {
        left: l,
        top: row_top,
        right: l + w,
        bottom: row_top + button,
    };
    let first = left + STRIP_PAD;
    let buttons: Vec<D2D_RECT_F> = (0..n)
        .map(|i| row(first + i as f32 * (button + STRIP_GAP), button))
        .collect();
    let mut x = first + apps_w;
    let divider = (n > 0).then(|| {
        let line = D2D_RECT_F {
            left: x + DIVIDER_SPACE,
            top: (top + bottom) / 2.0 - DIVIDER_HEIGHT / 2.0,
            right: x + DIVIDER_SPACE + 1.0,
            bottom: (top + bottom) / 2.0 + DIVIDER_HEIGHT / 2.0,
        };
        x += divider_w(true);
        line
    });
    let status = row(x, status_width);
    let clock = row(x + status_width + status_gap, CLOCK_WIDTH);
    let overshoot = (panel_w - config.island_width).max(0.0) * 0.06;
    let window = edge_window(
        &panel,
        HIT_MARGIN + 8.0 + overshoot,
        HIT_MARGIN + 6.0,
        width,
        strip_height,
        at_top,
    );
    StripLayout {
        panel,
        window,
        buttons,
        divider,
        status,
        clock,
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
        assert!(
            half > 0.5 && half < 1.2,
            "spring is well on its way: {half}"
        );
        // Reverse mid-way: collapsing takes only the remaining distance.
        let t1 = t0 + EXPAND_DURATION / 2;
        assert!(island.set_target(false, t1));
        assert!(!island.tick(t1 + COLLAPSE_DURATION));
        assert!(island.is_collapsed());
    }

    #[test]
    fn expansion_springs_past_the_target_and_settles() {
        let t0 = Instant::now();
        let mut island = Island::collapsed();
        island.set_target(true, t0);
        let mut peak = 0.0f32;
        for ms in (0..480).step_by(8) {
            island.tick(t0 + Duration::from_millis(ms));
            peak = peak.max(island.eased());
        }
        assert!(peak > 1.01 && peak < 1.08, "small overshoot: {peak}");
        island.tick(t0 + EXPAND_DURATION);
        assert_eq!(island.eased(), 1.0);
    }

    #[test]
    fn pill_floats_above_the_edge() {
        let config = BarConfig::default();
        let strip = strip_height(&config, 24.0);
        let g = geometry(1920.0, strip, &config, false);
        assert_eq!(
            (g.pill.right - g.pill.left, g.pill.bottom - g.pill.top),
            (200.0, 40.0)
        );
        assert_eq!((g.pill.left + g.pill.right) / 2.0, 960.0);
        assert_eq!(g.pill.bottom, strip - config.island_gap, "gap to the edge");
        assert_eq!(g.hit.bottom, strip, "window reaches the screen edge");
        let top = geometry(1920.0, strip, &config, true);
        assert_eq!(top.pill.top, config.island_gap);
        assert_eq!(top.hit.top, 0.0);
        assert!(g.activity_hit.top >= 0.0);
    }

    #[test]
    fn strip_holds_buttons_divider_status_and_clock() {
        let config = BarConfig::default();
        let strip = strip_height(&config, 24.0);
        let l = strip_layout(1920.0, strip, &config, 24.0, 5, 100.0, false);
        assert_eq!(l.buttons.len(), 5);
        let inside = |r: &D2D_RECT_F| {
            r.left >= l.panel.left
                && r.right <= l.panel.right
                && r.top >= l.panel.top
                && r.bottom <= l.panel.bottom
        };
        assert!(l.buttons.iter().all(inside));
        assert!(inside(&l.status) && inside(&l.clock));
        let divider = l.divider.unwrap();
        assert!(divider.left > l.buttons[4].right && divider.right < l.status.left);
        assert!(l.clock.left >= l.status.right && l.clock.right < l.panel.right);
        // Centered on the screen, bottom edge on the pill's baseline, window holds it.
        assert!(((l.panel.left + l.panel.right) / 2.0 - 960.0).abs() <= 1.0);
        assert_eq!(l.panel.bottom, strip - config.island_gap);
        assert!(l.window.left < l.panel.left && l.window.right > l.panel.right);
        assert_eq!(l.window.bottom, strip);
        assert!(l.window.top >= 0.0);
        // Wider than the pill, and never narrower.
        assert!(l.panel.right - l.panel.left > config.island_width);
        let few = strip_layout(1920.0, strip, &config, 24.0, 0, 0.0, false);
        assert!(few.buttons.is_empty() && few.divider.is_none());
        assert!(few.panel.right - few.panel.left >= config.island_width);
    }

    #[test]
    fn strip_drops_buttons_that_do_not_fit() {
        let config = BarConfig::default();
        let strip = strip_height(&config, 24.0);
        let l = strip_layout(600.0, strip, &config, 24.0, 40, 100.0, false);
        assert!(l.buttons.len() < 40 && !l.buttons.is_empty());
        assert!(l.panel.left >= 0.0 && l.panel.right <= 600.0);
        let top = strip_layout(1920.0, strip, &config, 24.0, 3, 0.0, true);
        assert_eq!(top.panel.top, config.island_gap);
        assert_eq!(top.window.top, 0.0);
    }

    #[test]
    fn activity_holds_the_pill_and_its_bounce() {
        let config = BarConfig::default();
        let g = geometry(1920.0, strip_height(&config, 24.0), &config, false);
        let inside = |a: &D2D_RECT_F, b: &D2D_RECT_F| {
            a.left >= b.left && a.top >= b.top && a.right <= b.right && a.bottom <= b.bottom
        };
        assert!(inside(&g.pill, &g.activity));
        assert!(inside(&g.hit, &g.activity_hit));
        // 20% overshoot still fits the window.
        assert!(inside(&pulsed_pill(&g, 1.2), &g.activity_hit));
        assert_eq!(g.activity.bottom, g.pill.bottom, "grows away from the edge");
    }

    #[test]
    fn pulse_bounces_holds_and_returns() {
        let t0 = Instant::now();
        let pulse = Pulse::new(None, t0);
        assert_eq!(pulse.value(t0), 0.0);
        let at = |ms: u64| pulse.value(t0 + Duration::from_millis(ms));
        let peak = (0..400).step_by(10).map(at).fold(0.0f32, f32::max);
        assert!(peak > 1.05, "springy overshoot: {peak}");
        assert!((at(1400) - 1.0).abs() < 0.02, "holds");
        assert!(
            !pulse.moving(t0 + Duration::from_millis(1000)),
            "no frames while holding"
        );
        assert!(pulse.done(t0 + PULSE_HOLD + Duration::from_millis(600)));
        assert!(at(1500 + 600).abs() < 0.05, "back to the pill");
    }
}
