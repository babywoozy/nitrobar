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
use crate::layout::{STATUS_CELL, STATUS_PADDING};
use crate::media::BARS;
use crate::morph::spring;
use crate::tasks;

/// Posted by the edge sensor to the bar: wParam 1 = pointer entered, 0 = left.
pub const WM_APP_EDGE: u32 = WM_APP + 7;
/// Sent after `TrackMouseEvent(TME_LEAVE)` when the pointer leaves the window.
const WM_MOUSELEAVE: u32 = 0x02A3;

pub const EXPAND_DURATION: Duration = Duration::from_millis(520);
pub const COLLAPSE_DURATION: Duration = Duration::from_millis(380);
/// Spring (rad/s, damping ratio) of the expansion: a small overshoot.
const EXPAND_SPRING: (f32, f32) = (24.0, 0.70);
/// Closing barely overshoots.
const COLLAPSE_SPRING: (f32, f32) = (24.0, 0.86);
/// Squash and stretch: how much flatter (DIPs) the island gets at full speed, and
/// the speed (progress per second) that counts as full.
const SQUASH: f32 = 4.0;
const SQUASH_FULL_SPEED: f32 = 9.0;
/// The part of the screen edge that expands the island: the pill's width plus this
/// much on each side, DIPs. The rest of the edge (a chat box of another app...) does nothing.
pub const EDGE_ZONE_MARGIN: f32 = 40.0;
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

/// How long an icon bounces after its app is raised / launched.
pub const BOUNCE_DURATION: Duration = Duration::from_millis(700);

/// Height (DIPs) an icon is lifted `since` the bounce started: two decaying hops.
pub fn bounce_height(since: Duration) -> f32 {
    let t = since.as_secs_f32();
    if t >= BOUNCE_DURATION.as_secs_f32() {
        return 0.0;
    }
    12.0 * (-3.2 * t).exp() * (std::f32::consts::PI * t / 0.3).sin().abs()
}

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

    /// Panel progress: 0 = pill, 1 = full. A spring both ways: it may pass 1 when
    /// opening and dip just under 0 when closing.
    pub fn eased(&self) -> f32 {
        if !self.animating() {
            return self.target;
        }
        self.curve(self.phase())
    }

    /// Seconds into the movement in the current direction (a movement reversed
    /// half-way starts part-way in).
    pub fn phase(&self) -> f32 {
        if self.expanded() {
            self.position * EXPAND_DURATION.as_secs_f32()
        } else {
            (1.0 - self.position) * COLLAPSE_DURATION.as_secs_f32()
        }
    }

    fn curve(&self, secs: f32) -> f32 {
        if self.expanded() {
            spring(secs, EXPAND_SPRING.0, EXPAND_SPRING.1)
        } else {
            1.0 - spring(secs, COLLAPSE_SPRING.0, COLLAPSE_SPRING.1)
        }
    }

    /// How much the island is flattened right now (DIPs): grows with its speed and
    /// vanishes as it comes to rest, like a drop of liquid.
    pub fn squash(&self) -> f32 {
        if !self.animating() {
            return 0.0;
        }
        let t = self.phase();
        let speed = (self.curve(t) - self.curve((t - 0.016).max(0.0))).abs() / 0.016;
        SQUASH * (speed / SQUASH_FULL_SPEED).min(1.0)
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

/// Equalizer bar width and the space between bars, DIPs.
pub const BAR_WIDTH: f32 = 4.0;
pub const BAR_GAP: f32 = 3.0;
/// Size of the active app's icon on the resting pill, DIPs.
pub const APP_ICON: f32 = 20.0;
/// Empty space on each side of the flame inside its box (3 of 24 units).
const MASCOT_SIDE: f32 = 3.0 / 24.0;
const CLOCK_TEXT_WIDTH: f32 = 90.0;

/// (content size, padding) of a pill `height` tall: the content is centered with the
/// same padding above, below and at the pill's end.
pub fn pill_padding(height: f32) -> (f32, f32) {
    let content = (height * 0.68).round();
    (content, (height - content) / 2.0)
}

/// Where the collapsed pill's content goes (bar space).
#[derive(Clone, Copy, Debug)]
pub struct RestLayout {
    /// The mascot's box (its drawing leaves a margin inside it).
    pub mascot: D2D_RECT_F,
    /// The active app's small icon, in the mascot's place.
    pub app: D2D_RECT_F,
    pub clock: D2D_RECT_F,
    /// Round cover art: concentric with the pill's round end.
    pub art: D2D_RECT_F,
    pub title: D2D_RECT_F,
    pub artist: D2D_RECT_F,
    pub bars: D2D_RECT_F,
}

pub fn rest_layout(pill: &D2D_RECT_F) -> RestLayout {
    let h = pill.bottom - pill.top;
    let (content, pad) = pill_padding(h);
    let mid = (pill.top + pill.bottom) / 2.0;
    let rect = |left: f32, top: f32, right: f32, bottom: f32| D2D_RECT_F {
        left,
        top,
        right,
        bottom,
    };
    let square = |left: f32| {
        rect(
            left,
            mid - content / 2.0,
            left + content,
            mid + content / 2.0,
        )
    };
    // Nothing reaches into the curved ends: the visible content starts half a pill
    // height in from either side.
    let inset = (h / 2.0).round();
    let mascot = square(pill.left + inset - content * MASCOT_SIDE);
    let clock_right = pill.right - inset;
    let clock = rect(
        clock_right - CLOCK_TEXT_WIDTH,
        pill.top,
        clock_right,
        pill.bottom,
    );
    let art = square(pill.left + inset);
    let bars_w = BARS as f32 * (BAR_WIDTH + BAR_GAP) - BAR_GAP;
    let bars_h = (h * 0.42).round();
    let bars_right = pill.right - inset;
    let bars = rect(
        bars_right - bars_w,
        mid - bars_h / 2.0,
        bars_right,
        mid + bars_h / 2.0,
    );
    let left = art.right + pad + 3.0;
    let right = (bars.left - pad - 2.0).max(left);
    let app = rect(
        pill.left + inset,
        mid - APP_ICON / 2.0,
        pill.left + inset + APP_ICON,
        mid + APP_ICON / 2.0,
    );
    RestLayout {
        app,
        mascot,
        clock,
        art,
        title: rect(left, mid - 16.0, right, mid + 1.0),
        artist: rect(left, mid, right, mid + 15.0),
        bars,
    }
}

/// The morphing panel between the resting pill and the strip: both share a center,
/// so it grows and shrinks symmetrically; `squash` flattens it around that center.
pub fn morph_panel(
    pill: &D2D_RECT_F,
    strip: &D2D_RECT_F,
    progress: f32,
    squash: f32,
) -> D2D_RECT_F {
    let mut panel = lerp_rect(pill, strip, progress);
    panel.top += squash / 2.0;
    panel.bottom -= squash / 2.0;
    panel
}

/// How one piece of the strip's content looks during the movement. The content
/// never scales with the panel: it sits at its final place, the panel's edge
/// uncovers it, and each piece rolls in on its own.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Reveal {
    pub alpha: f32,
    pub scale: f32,
    /// Horizontal offset from the final place, DIPs.
    pub dx: f32,
    /// Degrees.
    pub angle: f32,
}

impl Reveal {
    pub const SHOWN: Self = Self {
        alpha: 1.0,
        scale: 1.0,
        dx: 0.0,
        angle: 0.0,
    };
}

impl Default for Reveal {
    fn default() -> Self {
        Self::SHOWN
    }
}

/// Delay of the first piece, and per step of distance from the center (seconds).
const REVEAL_START: f32 = 0.015;
const REVEAL_STAGGER: f32 = 0.025;
const HIDE_STAGGER: f32 = 0.012;
const HIDE_TIME: f32 = 0.09;
/// The resting content leaves right away when opening, and when closing comes back
/// only once the strip is nearly gone (so the two never overlap).
const REST_LEAVE: f32 = 0.075;
const REST_RETURN: (f32, f32) = (0.14, 0.10);
/// Roll-in: travel from the center side (DIPs), turn (degrees), starting size.
const ROLL: f32 = 8.0;
const TURN: f32 = 15.0;
const SMALL: f32 = 0.6;

/// One piece `distance` steps from the center (of `farthest`), on the left
/// (`side` -1), the right (1) or in the middle (0), `secs` into the movement.
/// Opening staggers from the center outward and ends in a small bounce; closing
/// takes the outermost pieces first, back toward the center.
pub fn reveal(distance: f32, farthest: f32, side: f32, opening: bool, secs: f32) -> Reveal {
    let (shown, alpha) = if opening {
        let u = secs - REVEAL_START - REVEAL_STAGGER * distance;
        if u <= 0.0 {
            (0.0, 0.0)
        } else {
            (spring(u, 28.0, 0.55), (u / 0.09).min(1.0))
        }
    } else {
        let u = secs - HIDE_STAGGER * (farthest - distance);
        let gone = (u / HIDE_TIME).clamp(0.0, 1.0);
        let gone = gone * gone;
        (1.0 - gone, 1.0 - gone)
    };
    let away = 1.0 - shown;
    Reveal {
        alpha,
        scale: SMALL + (1.0 - SMALL) * shown,
        dx: -side * ROLL * away,
        angle: -side * TURN * away,
    }
}

/// Opacity of the resting content (mascot, clock, music) during the movement.
pub fn rest_opacity(anim: &Island) -> f32 {
    if !anim.animating() {
        return if anim.expanded() { 0.0 } else { 1.0 };
    }
    let secs = anim.phase();
    if anim.expanded() {
        (1.0 - secs / REST_LEAVE).clamp(0.0, 1.0)
    } else {
        ((secs - REST_RETURN.0) / REST_RETURN.1).clamp(0.0, 1.0)
    }
}

/// `reveal` for pieces centered at `centers` in a strip centered at `middle`.
pub fn reveals(centers: &[f32], middle: f32, step: f32, anim: &Island) -> Vec<Reveal> {
    if !anim.animating() {
        let shown = if anim.expanded() {
            Reveal::SHOWN
        } else {
            Reveal {
                alpha: 0.0,
                ..Reveal::SHOWN
            }
        };
        return vec![shown; centers.len()];
    }
    let distance = |x: f32| (x - middle).abs() / step.max(1.0);
    let farthest = centers.iter().map(|&x| distance(x)).fold(0.0, f32::max);
    centers
        .iter()
        .map(|&x| {
            let side = if (x - middle).abs() < step / 2.0 {
                0.0
            } else {
                (x - middle).signum()
            };
            reveal(distance(x), farthest, side, anim.expanded(), anim.phase())
        })
        .collect()
}

/// The grid: this much empty space between the visible things in the strip
/// (Start logo, app icons, divider, status glyphs, clock).
pub const GRID_GAP: f32 = 16.0;
/// A button extends this far beyond its visible icon on each side; so does a
/// hover plate around the clock.
const PLATE: f32 = 8.0;
/// Empty space around a 16 DIP status glyph inside its cell.
const GLYPH_SIDE: f32 = 4.0;
/// Height of the divider line between the apps and the status area.
const DIVIDER_HEIGHT: f32 = 20.0;

/// What follows the apps: status glyphs, the measured widths of the battery
/// percentage and of the clock's wider line.
#[derive(Clone, Copy, Debug, Default)]
pub struct Tail {
    pub glyphs: usize,
    pub percent: f32,
    pub clock: f32,
}

/// The pill expanded into the strip: Start, app icons, divider, status, clock
/// (bar space, DIPs). Everything is laid out at its final position.
#[derive(Clone, Debug, PartialEq)]
pub struct StripLayout {
    pub panel: D2D_RECT_F,
    /// Window rect that holds the panel, its shadow and the spring overshoot.
    pub window: D2D_RECT_F,
    pub start: D2D_RECT_F,
    pub buttons: Vec<D2D_RECT_F>,
    pub divider: D2D_RECT_F,
    /// Zero width when there is nothing to show.
    pub status: D2D_RECT_F,
    pub clock: D2D_RECT_F,
}

/// Lays the strip out on the grid in one panel centered on the screen. The
/// visible content starts and ends half a panel height from the panel's ends,
/// clear of the curves. App buttons that don't fit are dropped.
#[allow(clippy::too_many_arguments)]
pub fn strip_layout(
    width: f32,
    strip_height: f32,
    config: &BarConfig,
    icon_size: f32,
    start_size: f32,
    items: usize,
    tail: Tail,
    at_top: bool,
) -> StripLayout {
    let button = icon_size + 2.0 * PLATE;
    let panel_h = (button + 12.0).max(config.island_height);
    let inset = (panel_h / 2.0).round();
    // Everything after the last app icon's right edge, up to the last visible pixel.
    let status_w = if tail.glyphs == 0 {
        0.0
    } else {
        let cells = tail.glyphs as f32 * STATUS_CELL;
        // Visible width: without the empty side of the first (and last) glyph cell.
        if tail.percent > 0.0 {
            cells - GLYPH_SIDE + tail.percent
        } else {
            cells - 2.0 * GLYPH_SIDE
        }
    };
    let after = GRID_GAP
        + 1.0
        + GRID_GAP
        + status_w
        + if status_w > 0.0 { GRID_GAP } else { 0.0 }
        + tail.clock;
    let fixed = inset * 2.0 + start_size + after;
    let step = icon_size + GRID_GAP;
    let room = (width - 32.0 - fixed) / step;
    let n = items.min(room.floor().max(0.0) as usize);
    let natural = fixed + n as f32 * step;
    // Never narrower than the pill; any extra is split between the two ends.
    let panel_w = natural.max(config.island_width).min(width);
    let left = ((width - panel_w) / 2.0).round();
    // Same center as the resting pill, so the island grows symmetrically.
    let gap = (config.island_gap + (config.island_height - panel_h) / 2.0).max(0.0);
    let (top, bottom) = band_y(at_top, strip_height, gap, panel_h);
    let panel = D2D_RECT_F {
        left,
        top,
        right: left + panel_w,
        bottom,
    };
    // Content row, vertically centered in the panel.
    let row_top = top + (panel_h - button) / 2.0;
    let row = |from: f32, to: f32| D2D_RECT_F {
        left: from,
        top: row_top,
        right: to,
        bottom: row_top + button,
    };
    let first = left + inset + ((panel_w - natural) / 2.0).max(0.0).floor();
    let start = row(first - PLATE, first + start_size + PLATE);
    // `x`: right edge of the last visible thing placed.
    let mut x = first + start_size;
    let buttons: Vec<D2D_RECT_F> = (0..n)
        .map(|_| {
            let icon = x + GRID_GAP;
            x = icon + icon_size;
            row(icon - PLATE, x + PLATE)
        })
        .collect();
    let middle = (top + bottom) / 2.0;
    let divider = D2D_RECT_F {
        left: x + GRID_GAP,
        top: middle - DIVIDER_HEIGHT / 2.0,
        right: x + GRID_GAP + 1.0,
        bottom: middle + DIVIDER_HEIGHT / 2.0,
    };
    x = divider.right;
    let status = if status_w > 0.0 {
        let visible = x + GRID_GAP;
        x = visible + status_w;
        // The renderer starts the first glyph cell `STATUS_PADDING` inside the rect.
        row(visible - GLYPH_SIDE - STATUS_PADDING, x + PLATE)
    } else {
        row(x, x)
    };
    let text = x + GRID_GAP;
    let clock = row(text - PLATE, text + tail.clock + PLATE);
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
        start,
        buttons,
        divider,
        status,
        clock,
    }
}

/// Whether `hwnd` is the bare desktop.
pub fn is_desktop(hwnd: HWND) -> bool {
    let mut class = [0u16; 16];
    let len = unsafe { GetClassNameW(hwnd, &mut class) }.max(0) as usize;
    let class = String::from_utf16_lossy(&class[..len]);
    class == "Progman" || class == "WorkerW"
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
            crate::theme::no_border(hwnd);
            // Alpha 1/255: invisible, but still hit-tested (alpha 0 is not).
            let _ = SetLayeredWindowAttributes(hwnd, Default::default(), 1, LWA_ALPHA);
            Some(Self { hwnd })
        }
    }

    /// Places the 1 px strip at screen height `y`, from `left` to `right` (screen
    /// pixels), and shows it (or hides it).
    pub fn place(&self, left: i32, right: i32, y: i32, visible: bool) {
        unsafe {
            if !visible {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
                return;
            }
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                left,
                y,
                (right - left).max(1),
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
    fn bounce_hops_twice_and_stops() {
        let at = |ms: u64| bounce_height(Duration::from_millis(ms));
        assert_eq!(at(0), 0.0);
        assert!(at(150) > 6.0, "first hop");
        assert!(at(300) < 0.5, "lands");
        assert!(at(450) > 2.0 && at(450) < at(150), "smaller second hop");
        assert_eq!(at(700), 0.0);
    }

    #[test]
    fn expansion_springs_past_the_target_and_settles() {
        let t0 = Instant::now();
        let mut island = Island::collapsed();
        island.set_target(true, t0);
        let mut peak = 0.0f32;
        for ms in (0..520).step_by(4) {
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
            (200.0, 44.0)
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
    fn content_reveals_from_the_center_and_leaves_from_the_edges() {
        let centers = [-84.0, -42.0, 0.0, 42.0, 84.0];
        let mut anim = Island::collapsed();
        let t0 = Instant::now();
        anim.set_target(true, t0);
        anim.tick(t0 + Duration::from_millis(100));
        let r = reveals(&centers, 0.0, 42.0, &anim);
        assert!(r[2].alpha > r[1].alpha && r[1].alpha > r[0].alpha, "{r:?}");
        assert_eq!(r[1].alpha, r[3].alpha, "symmetric");
        // Pieces start toward the center and turned, mirrored on the two sides.
        assert!(r[0].dx > 0.0 && r[4].dx < 0.0 && r[2].dx == 0.0);
        assert!(r[4].angle < 0.0 && r[0].angle > 0.0);
        anim.tick(t0 + EXPAND_DURATION);
        assert!(
            reveals(&centers, 0.0, 42.0, &anim)
                .iter()
                .all(|&r| r == Reveal::SHOWN)
        );
        // Closing: the outermost go first.
        let t1 = t0 + EXPAND_DURATION;
        anim.set_target(false, t1);
        anim.tick(t1 + Duration::from_millis(70));
        let r = reveals(&centers, 0.0, 42.0, &anim);
        assert!(r[0].alpha < r[1].alpha && r[1].alpha < r[2].alpha, "{r:?}");
    }

    #[test]
    fn resting_content_and_strip_never_overlap_much() {
        let centers = [-168.0, -126.0, -84.0, -42.0, 0.0, 42.0, 84.0, 168.0];
        let t0 = Instant::now();
        let mut anim = Island::collapsed();
        anim.set_target(true, t0);
        anim.tick(t0 + EXPAND_DURATION);
        let t1 = t0 + EXPAND_DURATION;
        anim.set_target(false, t1);
        for ms in (0..380).step_by(10) {
            anim.tick(t1 + Duration::from_millis(ms));
            let strip = reveals(&centers, 0.0, 42.0, &anim)
                .iter()
                .fold(0.0f32, |m, r| m.max(r.alpha));
            assert!(
                strip.min(rest_opacity(&anim)) < 0.35,
                "{ms} ms: {strip} and {}",
                rest_opacity(&anim)
            );
        }
        assert_eq!(rest_opacity(&Island::collapsed()), 1.0);
    }

    #[test]
    fn the_island_squashes_while_moving_only() {
        let mut anim = Island::collapsed();
        assert_eq!(anim.squash(), 0.0);
        let t0 = Instant::now();
        anim.set_target(true, t0);
        anim.tick(t0 + Duration::from_millis(60));
        assert!(anim.squash() > 2.0 && anim.squash() <= SQUASH);
        anim.tick(t0 + Duration::from_millis(500));
        assert!(anim.squash() < 0.3);
        // Symmetric around the shared center.
        let pill = D2D_RECT_F {
            left: 100.0,
            top: 20.0,
            right: 300.0,
            bottom: 64.0,
        };
        let strip = D2D_RECT_F {
            left: 0.0,
            top: 16.0,
            right: 400.0,
            bottom: 68.0,
        };
        let panel = morph_panel(&pill, &strip, 0.6, 3.0);
        assert_eq!(panel.left + panel.right, 400.0);
        assert_eq!(panel.top + panel.bottom, 84.0);
    }

    #[test]
    fn collapsed_content_has_equal_margins() {
        let pill = D2D_RECT_F {
            left: 100.0,
            top: 20.0,
            right: 388.0,
            bottom: 64.0,
        };
        let l = rest_layout(&pill);
        let (content, pad) = pill_padding(44.0);
        assert_eq!(l.app.left - pill.left, 22.0);
        assert_eq!(l.app.right - l.app.left, APP_ICON);
        assert_eq!(l.app.top - pill.top, pill.bottom - l.app.bottom);
        // Nothing in the curved ends, and the same inset on both sides.
        assert_eq!(l.art.left - pill.left, 22.0);
        assert_eq!(pill.right - l.bars.right, 22.0);
        assert_eq!(pill.right - l.clock.right, 22.0);
        assert!((l.mascot.left + content * MASCOT_SIDE - pill.left - 22.0).abs() < 0.01);
        // Centered vertically.
        assert_eq!(l.art.top - pill.top, pad);
        assert_eq!(pill.bottom - l.art.bottom, pad);
        assert_eq!(l.bars.top - pill.top, pill.bottom - l.bars.bottom);
        // Two text lines centered between the cover and the bars.
        assert!(l.title.left > l.art.right && l.title.right < l.bars.left);
        assert!(((l.title.top + l.artist.bottom) / 2.0 - 42.0).abs() <= 1.0);
    }

    const TAIL: Tail = Tail {
        glyphs: 3,
        percent: 28.0,
        clock: 62.0,
    };

    #[test]
    fn strip_sits_on_a_grid_with_equal_ends() {
        let config = BarConfig::default();
        let strip = strip_height(&config, 24.0);
        let l = strip_layout(1920.0, strip, &config, 24.0, 20.0, 5, TAIL, false);
        assert_eq!(l.buttons.len(), 5);
        // Visible edges: the logo and the clock text are equally far from the ends.
        let logo_left = l.start.left + PLATE;
        let clock_right = l.clock.right - PLATE;
        assert_eq!(logo_left - l.panel.left, l.panel.right - clock_right);
        assert_eq!(logo_left - l.panel.left, 26.0);
        // 16 between logo and first icon, between icons, around the divider,
        // before the first status glyph and before the clock.
        let icon = |r: &D2D_RECT_F| (r.left + PLATE, r.right - PLATE);
        assert_eq!(icon(&l.buttons[0]).0 - (l.start.right - PLATE), GRID_GAP);
        for pair in l.buttons.windows(2) {
            assert_eq!(icon(&pair[1]).0 - icon(&pair[0]).1, GRID_GAP);
        }
        assert_eq!(l.divider.left - icon(&l.buttons[4]).1, GRID_GAP);
        let glyph_left = l.status.left + STATUS_PADDING + GLYPH_SIDE;
        assert_eq!(glyph_left - l.divider.right, GRID_GAP);
        let percent_right = l.status.right - PLATE;
        assert_eq!((l.clock.left + PLATE) - percent_right, GRID_GAP);
        assert_eq!(
            percent_right,
            l.status.left + STATUS_PADDING + 3.0 * STATUS_CELL + 28.0
        );
        // Centered on the screen and on the pill; the window holds it.
        assert!(((l.panel.left + l.panel.right) / 2.0 - 960.0).abs() <= 1.0);
        let pill = geometry(1920.0, strip, &config, false).pill;
        assert_eq!(l.panel.top + l.panel.bottom, pill.top + pill.bottom);
        assert!(l.window.left < l.panel.left && l.window.right > l.panel.right);
        assert_eq!(l.window.bottom, strip);
        assert!(l.window.top >= 0.0);
    }

    #[test]
    fn strip_handles_few_and_too_many_items() {
        let config = BarConfig::default();
        let strip = strip_height(&config, 24.0);
        let l = strip_layout(600.0, strip, &config, 24.0, 20.0, 40, TAIL, false);
        assert!(l.buttons.len() < 40 && !l.buttons.is_empty());
        assert!(l.panel.left >= 0.0 && l.panel.right <= 600.0);
        let none = strip_layout(1920.0, strip, &config, 24.0, 20.0, 0, Tail::default(), true);
        assert!(none.buttons.is_empty() && none.status.right == none.status.left);
        assert!(none.panel.right - none.panel.left >= config.island_width);
        // Padded to the pill's width: the extra is split between the ends.
        let left = none.start.left + PLATE - none.panel.left;
        let right = none.panel.right - (none.clock.right - PLATE);
        assert!((left - right).abs() <= 1.0, "{left} {right}");
        assert_eq!(
            none.panel.top,
            config.island_gap + (config.island_height - 52.0) / 2.0
        );
        assert_eq!(none.window.top, 0.0);
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
