//! Minimize / restore animation: a window "pours" into the island and back out.
//!
//! - Trigger: `HSHELL_GETMINRECT` (see app.rs). Windows sends it synchronously
//!   when a window minimizes or restores (after switching its state), and once
//!   we answer it plays no animation of its own: nothing to fight with, no
//!   global setting to change. The overlay starts from a posted message, once
//!   the window is hidden: a thumbnail of a window that is still minimizing
//!   brings Windows' own animation back on screen.
//! - Picture: a DWM thumbnail of the window inside an overlay window (`Morph`).
//!   DWM thumbnails can't be clipped or masked by the API, so the overlay has a
//!   rounded window region (clips the thumbnail too) and a black background:
//!   fading the thumbnail out over it darkens the picture to a black blob that
//!   slides under the pill. The overlay covers the whole path once (fixed size);
//!   frames only change the region, the thumbnail rect / opacity and the alpha.
//! - Frames come from `FrameClock`: a thread that waits for DWM's next
//!   composition (`DwmFlush`, i.e. the display's refresh) and posts one message
//!   per frame. It only exists while something moves.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWM_THUMBNAIL_PROPERTIES, DWM_TNP_OPACITY, DWM_TNP_RECTDESTINATION, DWM_TNP_RECTSOURCE,
    DWM_TNP_SOURCECLIENTAREAONLY, DWM_TNP_VISIBLE, DwmFlush, DwmRegisterThumbnail,
    DwmUnregisterThumbnail, DwmUpdateThumbnailProperties,
};
use windows::Win32::Graphics::Gdi::{CreateRoundRectRgn, CreateSolidBrush, SetWindowRgn};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetSystemMetricsForDpi;
use windows::Win32::UI::WindowsAndMessaging::{
    ANIMATIONINFO, SPI_GETANIMATION, SPI_GETCLIENTAREAANIMATION,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GWL_STYLE, GetWindowLongW, GetWindowPlacement,
    HWND_TOPMOST, IsIconic, LWA_ALPHA, LWA_COLORKEY, PostMessageW, RegisterClassExW,
    SM_CXPADDEDBORDER, SM_CXSIZEFRAME, SM_CYSIZEFRAME, SW_HIDE, SWP_NOACTIVATE, SWP_SHOWWINDOW,
    SetLayeredWindowAttributes, SetWindowPos, ShowWindow, WINDOWPLACEMENT, WM_APP, WNDCLASSEXW,
    WPF_RESTORETOMAXIMIZED, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP, WS_THICKFRAME,
};
use windows::core::{BOOL, PCWSTR, w};

use crate::island::{ease_out_cubic, lerp};

/// Posted by the frame clock once per display refresh while animating.
pub const WM_APP_FRAME: u32 = WM_APP + 8;

pub const MINIMIZE_DURATION: Duration = Duration::from_millis(400);
pub const RESTORE_DURATION: Duration = Duration::from_millis(220);
/// Minimize: fraction of the way at which the island "swallows" (starts its bounce).
pub const SWALLOW_AT: f32 = 0.78;
/// Restore, after asking the real window to show: the app needs ~50-150 ms to
/// repaint, and until then both the window and its thumbnail are blank
/// (measured). Hiding the overlay at once flashes the desktop; keeping it
/// flashes its black background. It fades out over `LINGER_FADE` instead (the
/// window appears out of the dark), and stays at least until the window is no
/// longer minimized, at most `LINGER_MAX`.
const LINGER_FADE: Duration = Duration::from_millis(110);
const LINGER_MAX: Duration = Duration::from_millis(700);
/// Windows 11 window corner radius, DIPs.
const WINDOW_RADIUS: f32 = 8.0;

// ----- frame clock -----

static CLOCK_GEN: AtomicU32 = AtomicU32::new(0);
static CLOCK_RUNNING: AtomicBool = AtomicBool::new(false);
static FRAME_PENDING: AtomicBool = AtomicBool::new(false);

/// Starts posting `WM_APP_FRAME` to `hwnd` once per composed frame (no-op if running).
pub fn start_clock(hwnd: HWND) {
    if CLOCK_RUNNING.swap(true, Ordering::AcqRel) {
        return;
    }
    let generation = CLOCK_GEN.fetch_add(1, Ordering::AcqRel) + 1;
    let target = hwnd.0 as usize;
    let spawned = std::thread::Builder::new()
        .name("nitrobar-frames".into())
        .stack_size(64 * 1024)
        .spawn(move || {
            while CLOCK_GEN.load(Ordering::Acquire) == generation {
                let started = Instant::now();
                // Returns at the next composition: the display's refresh rate.
                let _ = unsafe { DwmFlush() };
                // DWM may return at once when it has nothing to compose: don't spin.
                if started.elapsed() < Duration::from_millis(3) {
                    std::thread::sleep(Duration::from_millis(4));
                }
                if CLOCK_GEN.load(Ordering::Acquire) != generation {
                    break;
                }
                // At most one frame message queued at a time.
                if !FRAME_PENDING.swap(true, Ordering::AcqRel) {
                    unsafe {
                        let _ = PostMessageW(
                            Some(HWND(target as *mut _)),
                            WM_APP_FRAME,
                            WPARAM(0),
                            LPARAM(0),
                        );
                    }
                }
            }
        });
    if spawned.is_err() {
        CLOCK_RUNNING.store(false, Ordering::Release);
    }
}

pub fn stop_clock() {
    if CLOCK_RUNNING.swap(false, Ordering::AcqRel) {
        CLOCK_GEN.fetch_add(1, Ordering::AcqRel);
    }
}

/// Call when handling `WM_APP_FRAME`, before drawing.
pub fn frame_handled() {
    FRAME_PENDING.store(false, Ordering::Release);
}

/// The user's Windows setting "Animate windows when minimizing and maximizing"
/// (and "Animation effects", which turns it off too): respected, not changed.
pub fn system_allows() -> bool {
    let mut info = ANIMATIONINFO {
        cbSize: size_of::<ANIMATIONINFO>() as u32,
        iMinAnimate: 1,
    };
    let mut client = BOOL(1);
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETANIMATION,
            info.cbSize,
            Some((&raw mut info).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
        let _ = SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some((&raw mut client).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
    info.iMinAnimate != 0 && client.as_bool()
}

// ----- easing -----

pub fn ease_in_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Damped spring from 0 to 1 (`t` in seconds): overshoots, then settles.
pub fn spring(t: f32, omega: f32, zeta: f32) -> f32 {
    if t <= 0.0 {
        return 0.0;
    }
    let wd = omega * (1.0 - zeta * zeta).sqrt();
    let decay = (-zeta * omega * t).exp();
    1.0 - decay * ((wd * t).cos() + zeta * omega / wd * (wd * t).sin())
}

fn lerp_px(a: &RECT, b: &RECT, t: f32) -> RECT {
    let l = |x: i32, y: i32| lerp(x as f32, y as f32, t).round() as i32;
    RECT {
        left: l(a.left, b.left),
        top: l(a.top, b.top),
        right: l(a.right, b.right),
        bottom: l(a.bottom, b.bottom),
    }
}

fn union(a: &RECT, b: &RECT) -> RECT {
    RECT {
        left: a.left.min(b.left),
        top: a.top.min(b.top),
        right: a.right.max(b.right),
        bottom: a.bottom.max(b.bottom),
    }
}

/// A topmost window covering a whole monitor is a "fullscreen app" to Explorer
/// (`ABN_FULLSCREENAPP`): the bar would hide itself and cancel the very animation
/// that tripped it (a maximized app could then never be reopened from the bar).
/// Leave a thin edge of the monitor uncovered.
pub fn avoid_fullscreen(bounds: RECT, monitor: &RECT) -> RECT {
    let covers = bounds.left <= monitor.left
        && bounds.top <= monitor.top
        && bounds.right >= monitor.right
        && bounds.bottom >= monitor.bottom;
    if !covers {
        return bounds;
    }
    RECT {
        left: monitor.left + 2,
        top: monitor.top + 2,
        right: monitor.right - 2,
        bottom: monitor.bottom - 2,
    }
}

fn size(r: &RECT) -> (i32, i32) {
    (r.right - r.left, r.bottom - r.top)
}

// ----- window geometry -----

/// Where a minimized window will appear when restored: (visible frame, thumbnail
/// source). `work` is the work area of its monitor (placement coordinates are
/// relative to it), `monitor_dpi` its DPI.
pub fn restored_frame(hwnd: HWND, work: &RECT, monitor_dpi: u32) -> Option<(RECT, RECT)> {
    let mut placement = WINDOWPLACEMENT {
        length: size_of::<WINDOWPLACEMENT>() as u32,
        ..Default::default()
    };
    unsafe { GetWindowPlacement(hwnd, &raw mut placement).ok()? };
    let resizable = unsafe { GetWindowLongW(hwnd, GWL_STYLE) } as u32 & WS_THICKFRAME.0 != 0;
    // Invisible resize borders (left, right, bottom) of a normal resizable window.
    let border = if resizable {
        unsafe {
            GetSystemMetricsForDpi(SM_CXSIZEFRAME, monitor_dpi)
                + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, monitor_dpi)
        }
    } else {
        0
    };
    let bottom_border = if resizable {
        unsafe {
            GetSystemMetricsForDpi(SM_CYSIZEFRAME, monitor_dpi)
                + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, monitor_dpi)
        }
    } else {
        0
    };
    if placement.flags & WPF_RESTORETOMAXIMIZED == WPF_RESTORETOMAXIMIZED {
        // A maximized window's visible frame is the work area; its rect overhangs it.
        let (w, h) = size(work);
        let source = RECT {
            left: border,
            top: border,
            right: border + w,
            bottom: border + h,
        };
        return Some((*work, source));
    }
    let n = placement.rcNormalPosition;
    let window = RECT {
        left: n.left + work.left,
        top: n.top + work.top,
        right: n.right + work.left,
        bottom: n.bottom + work.top,
    };
    let frame = RECT {
        left: window.left + border,
        top: window.top,
        right: window.right - border,
        bottom: window.bottom - bottom_border,
    };
    let (w, h) = size(&frame);
    let source = RECT {
        left: border,
        top: 0,
        right: border + w,
        bottom: h,
    };
    Some((frame, source))
}

// ----- the overlay -----

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// Window -> island.
    Minimize,
    /// Island -> window; the real window is restored at the end.
    Restore,
}

struct Run {
    kind: Kind,
    target: HWND,
    thumb: isize,
    /// Visible window frame and the island rect, screen pixels.
    frame: RECT,
    island: RECT,
    /// Overlay window rect (covers the whole path).
    bounds: RECT,
    source: RECT,
    start: Instant,
    duration: Duration,
    window_radius: f32,
    swallowed: bool,
    /// Restore: when the real window was asked to show.
    linger: Option<Instant>,
}

/// What happened during a frame.
#[derive(Default, Debug)]
pub struct Tick {
    pub running: bool,
    /// Minimize reached the island: start the island bounce.
    pub swallowed: bool,
    /// Restore finished its growth: show this window now.
    pub show: Option<HWND>,
}

const CLASS: PCWSTR = w!("NitrobarMorph");
/// The overlay's background: near black for the minimize blob; made see-through
/// with a color key while a window grows back (so there is no black shape).
const BACKDROP: COLORREF = COLORREF(0x0002_0202);

extern "system" fn morph_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

pub struct Morph {
    hwnd: HWND,
    run: Option<Run>,
}

impl Morph {
    pub fn new() -> Option<Self> {
        unsafe {
            let instance = GetModuleHandleW(None).ok()?.into();
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(morph_proc),
                hInstance: instance,
                // Black: what the picture darkens to (the thumbnail fades over it).
                hbrBackground: CreateSolidBrush(BACKDROP),
                lpszClassName: CLASS,
                ..Default::default()
            };
            RegisterClassExW(&raw const class);
            // Click-through, never activated, not in Alt+Tab.
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW
                    | WS_EX_TOPMOST
                    | WS_EX_NOACTIVATE
                    | WS_EX_TRANSPARENT
                    | WS_EX_LAYERED,
                CLASS,
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
            crate::theme::no_border(hwnd);
            Some(Self { hwnd, run: None })
        }
    }

    /// The window currently being animated.
    pub fn target(&self) -> Option<(HWND, Kind)> {
        self.run.as_ref().map(|r| (r.target, r.kind))
    }

    /// Starts animating `target` between its visible `frame` and `island`
    /// (screen pixels). `below` is the island window: the overlay slides under it.
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        &mut self,
        kind: Kind,
        target: HWND,
        frame: RECT,
        source: RECT,
        island: RECT,
        below: HWND,
        dpi: u32,
        monitor: &RECT,
    ) -> bool {
        self.finish();
        // Only once the window is hidden: a thumbnail of a window that is still
        // minimizing makes Windows' own minimize animation show up (measured).
        let Ok(thumb) = (unsafe { DwmRegisterThumbnail(self.hwnd, target) }) else {
            return false;
        };
        let bounds = avoid_fullscreen(union(&frame, &island), monitor);
        let (w, h) = size(&bounds);
        self.run = Some(Run {
            kind,
            target,
            thumb,
            frame,
            island,
            bounds,
            source,
            start: Instant::now(),
            duration: match kind {
                Kind::Minimize => MINIMIZE_DURATION,
                Kind::Restore => RESTORE_DURATION,
            },
            window_radius: WINDOW_RADIUS * dpi as f32 / 96.0,
            swallowed: false,
            linger: None,
        });
        // First frame before showing: never flash a full black rectangle.
        self.render(0.0, 1.0);
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(if below.is_invalid() {
                    HWND_TOPMOST
                } else {
                    below
                }),
                bounds.left,
                bounds.top,
                w,
                h,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
        true
    }

    /// Advances one frame.
    pub fn tick(&mut self, now: Instant) -> Tick {
        let Some(run) = &mut self.run else {
            return Tick::default();
        };
        let mut tick = Tick {
            running: true,
            ..Tick::default()
        };
        if let Some(asked) = run.linger {
            let elapsed = now.saturating_duration_since(asked);
            let restored = !unsafe { IsIconic(run.target) }.as_bool();
            if (restored && elapsed >= LINGER_FADE) || elapsed > LINGER_MAX {
                self.finish();
                tick.running = false;
            } else {
                let fade = 1.0 - (elapsed.as_secs_f32() / LINGER_FADE.as_secs_f32()).min(1.0);
                self.render(1.0, fade);
            }
            return tick;
        }
        let p = (now.saturating_duration_since(run.start).as_secs_f32()
            / run.duration.as_secs_f32())
        .min(1.0);
        if run.kind == Kind::Minimize && p >= SWALLOW_AT && !run.swallowed {
            run.swallowed = true;
            tick.swallowed = true;
        }
        let kind = run.kind;
        let target = run.target;
        self.render(p, 1.0);
        if p >= 1.0 {
            match kind {
                Kind::Minimize => {
                    self.finish();
                    tick.running = false;
                }
                Kind::Restore => {
                    tick.show = Some(target);
                    if let Some(run) = &mut self.run {
                        run.linger = Some(now);
                    }
                }
            }
        }
        tick
    }

    /// Draws progress `p` (0..1), with the whole overlay at opacity `fade`.
    fn render(&self, p: f32, fade: f32) {
        let Some(run) = &self.run else {
            return;
        };
        // How far along the path (0 = window, 1 = island), and the looks.
        let (along, picture, alpha) = match run.kind {
            Kind::Minimize => (
                ease_in_out_cubic(p),
                1.0 - smoothstep(0.08, 0.62, p),
                1.0 - smoothstep(0.82, 1.0, p),
            ),
            // The window grows out of its icon and fades in; nothing black around it.
            Kind::Restore => (
                1.0 - ease_out_cubic(p),
                smoothstep(0.0, 0.5, p),
                smoothstep(0.0, 0.08, p),
            ),
        };
        let rect = lerp_px(&run.frame, &run.island, along);
        let (w, h) = size(&rect);
        if w <= 0 || h <= 0 {
            return;
        }
        let island_radius = size(&run.island).1 as f32 / 2.0;
        let radius = lerp(run.window_radius, island_radius, along)
            .max(run.window_radius.min(w.min(h) as f32 / 2.0))
            .min(w.min(h) as f32 / 2.0);
        let local = RECT {
            left: rect.left - run.bounds.left,
            top: rect.top - run.bounds.top,
            right: rect.right - run.bounds.left,
            bottom: rect.bottom - run.bounds.top,
        };
        // Keep the picture's aspect ratio inside the shrinking blob.
        let (sw, sh) = size(&run.source);
        let scale = (w as f32 / sw.max(1) as f32).min(h as f32 / sh.max(1) as f32);
        let (dw, dh) = (
            (sw as f32 * scale).round() as i32,
            (sh as f32 * scale).round() as i32,
        );
        let dest = RECT {
            left: local.left + (w - dw) / 2,
            top: local.top + (h - dh) / 2,
            right: local.left + (w - dw) / 2 + dw,
            bottom: local.top + (h - dh) / 2 + dh,
        };
        let props = DWM_THUMBNAIL_PROPERTIES {
            dwFlags: DWM_TNP_RECTDESTINATION
                | DWM_TNP_RECTSOURCE
                | DWM_TNP_OPACITY
                | DWM_TNP_VISIBLE
                | DWM_TNP_SOURCECLIENTAREAONLY,
            rcDestination: dest,
            rcSource: run.source,
            opacity: (picture * 255.0).round().clamp(0.0, 255.0) as u8,
            fVisible: (picture > 0.0).into(),
            fSourceClientAreaOnly: false.into(),
        };
        let d = (radius * 2.0).round() as i32;
        unsafe {
            let _ = DwmUpdateThumbnailProperties(run.thumb, &raw const props);
            // The region clips the thumbnail too: rounded corners for the picture.
            let region = CreateRoundRectRgn(
                local.left,
                local.top,
                local.right + 1,
                local.bottom + 1,
                d,
                d,
            );
            SetWindowRgn(self.hwnd, Some(region), true);
            let _ = SetLayeredWindowAttributes(
                self.hwnd,
                BACKDROP,
                (alpha * fade * 255.0).round().clamp(0.0, 255.0) as u8,
                if run.kind == Kind::Restore {
                    LWA_ALPHA | LWA_COLORKEY
                } else {
                    LWA_ALPHA
                },
            );
        }
    }

    /// Ends any animation at once and hides the overlay.
    pub fn finish(&mut self) {
        if let Some(run) = self.run.take() {
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
                let _ = DwmUnregisterThumbnail(run.thumb);
            }
        }
    }
}

impl Drop for Morph {
    fn drop(&mut self) {
        self.finish();
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_never_covers_a_whole_monitor() {
        let monitor = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        let maximized = avoid_fullscreen(monitor, &monitor);
        assert!(maximized.left > 0 && maximized.top > 0);
        assert!(maximized.right < 1920 && maximized.bottom < 1080);
        // Bigger than the monitor (a maximized window's frame overhangs by 8 px).
        let overhang = RECT {
            left: -8,
            top: -8,
            right: 1928,
            bottom: 1088,
        };
        assert_ne!(avoid_fullscreen(overhang, &monitor), overhang);
        // A normal window is left alone.
        let window = RECT {
            left: 300,
            top: 100,
            right: 1200,
            bottom: 1070,
        };
        assert_eq!(avoid_fullscreen(window, &monitor), window);
    }

    #[test]
    fn spring_overshoots_then_settles() {
        let peak = (1..100)
            .map(|i| spring(i as f32 * 0.01, 22.0, 0.5))
            .fold(0.0f32, f32::max);
        assert!(peak > 1.05 && peak < 1.25, "peak {peak}");
        assert!((spring(1.0, 22.0, 0.5) - 1.0).abs() < 0.01);
        assert_eq!(spring(0.0, 22.0, 0.5), 0.0);
    }

    #[test]
    fn ease_in_out_is_symmetric() {
        assert_eq!(ease_in_out_cubic(0.0), 0.0);
        assert_eq!(ease_in_out_cubic(1.0), 1.0);
        assert!((ease_in_out_cubic(0.5) - 0.5).abs() < 1e-6);
        assert!((ease_in_out_cubic(0.25) + ease_in_out_cubic(0.75) - 1.0).abs() < 1e-6);
    }
}
