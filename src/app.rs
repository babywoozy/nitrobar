//! The bar window: creation, message loop and window procedure.
//!
//! State lives in a `RefCell<App>` behind `GWLP_USERDATA`. Win32 re-enters the
//! window procedure synchronously (e.g. `SetWindowPos` sends `WM_WINDOWPOSCHANGED`),
//! so messages that arrive while `App` is borrowed fall back to stateless
//! handling / `DefWindowProc` instead of aliasing `&mut App`.

use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::time::Instant;

use windows::Win32::Foundation::{
    GetLastError, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, WAIT_OBJECT_0, WPARAM,
};
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateRectRgn, EndPaint, GetMonitorInfoW, InvalidateRect, MONITOR_DEFAULTTONEAREST,
    MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint, MonitorFromWindow, PAINTSTRUCT,
    ScreenToClient, SetWindowRgn,
};
use windows::Win32::Storage::FileSystem::{
    FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FindCloseChangeNotification,
    FindFirstChangeNotificationW, FindNextChangeNotification,
};
use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::RemoteDesktop::{
    NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification, WTSUnRegisterSessionNotification,
};
use windows::Win32::System::Threading::INFINITE;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_LBUTTON,
};
use windows::Win32::UI::Shell::{ABN_FULLSCREENAPP, ABN_POSCHANGED};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DeregisterShellHookWindow, DispatchMessageW, FindWindowW,
    GWLP_USERDATA, GetClientRect, GetWindowLongPtrW, HWND_BOTTOM, HWND_TOPMOST, IDC_ARROW,
    KillTimer, LoadCursorW, MSG, PostMessageW, PostQuitMessage, RegisterClassExW,
    RegisterShellHookWindow, RegisterWindowMessageW, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SendMessageW, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    TranslateMessage, WM_ACTIVATE, WM_APP, WM_CLOSE, WM_CONTEXTMENU, WM_DESTROY, WM_DISPLAYCHANGE,
    WM_DPICHANGED, WM_ENDSESSION, WM_ERASEBKGND, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_NCDESTROY,
    WM_PAINT, WM_QUERYENDSESSION, WM_SETTINGCHANGE, WM_TIMECHANGE, WM_TIMER, WM_WINDOWPOSCHANGED,
    WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetForegroundWindow, GetWindowRect, IsIconic, MsgWaitForMultipleObjects,
    PBT_APMPOWERSTATUSCHANGE, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND, PBT_POWERSETTINGCHANGE,
    PM_REMOVE, PeekMessageW, QS_ALLINPUT, SW_HIDE, SWP_SHOWWINDOW, WM_DWMCOLORIZATIONCOLORCHANGED,
    WM_DWMCOMPOSITIONCHANGED, WM_MBUTTONUP, WM_MOUSEMOVE, WM_POWERBROADCAST, WM_QUIT,
    WM_THEMECHANGED, WM_WTSSESSION_CHANGE, WTS_SESSION_UNLOCK,
};
use windows::core::{HSTRING, PCWSTR, Result, w};

use crate::config::{
    self, Alignment, Background, BarMode, Config, IslandStyle, Position, ThemeMode,
};
use crate::glass::Glass;
use crate::icons::{Icon, IconLoader};
use crate::island::{self, EdgeSensor, Island, Pulse};
use crate::layout::{BarLayout, Hit, Metrics};
use crate::mascot::{self, Mascot};
use crate::media::{self, Bars, Media, Meter, Track};
use crate::morph::{self, Morph};
use crate::pins::{self, Pins};
use crate::preview::{self, Preview};
use crate::render::{Badge, IslandView, MascotView, MusicView, RestView};
use crate::render::{ButtonState, ButtonView, Pointer, Renderer, Scene};
use crate::status::{self, Status};
use crate::tasks::AppId;
use crate::tasks::{self, TaskList};
use crate::theme::Palette;
use crate::theme::{self, Theme};
use crate::wallpaper;
use crate::{appbar, clock, layout, menu, safety, shell, taskbar, winpins};

/// Posted by the message loop when a file in the config directory changed.
const WM_APP_CONFIG: u32 = WM_APP + 5;
/// Posted by the message loop when the native taskbar's pin list changed.
const WM_APP_PINS: u32 = WM_APP + 6;
/// Posted to ourselves after `HSHELL_GETMINRECT`: wParam = the window, lParam = 1 if it minimized, 0 if it restores.
const WM_APP_MINIMIZED: u32 = WM_APP + 9;

const CLASS_NAME: PCWSTR = w!("NitrobarWindow");
const TIMER_CLOCK: usize = 1;
/// Island: hover delay before expanding, delay before collapsing, animation frames.
const TIMER_EXPAND: usize = 2;
const TIMER_COLLAPSE: usize = 3;
/// One-shot: the island pulse's hold ends (frames are not needed while it holds).
const TIMER_PULSE: usize = 4;
/// A restore we started ourselves still gets its own `HSHELL_GETMINRECT`; ignore it.
const OWN_RESTORE_GRACE: std::time::Duration = std::time::Duration::from_secs(2);
/// Dock-like zoom of the icon under the pointer, and how fast it follows (s).
const ZOOM_MAX: f32 = 0.20;
const ZOOM_EASE: f32 = 0.05;
/// The resting pill: black like the Dynamic Island, with a faint hairline.
const REST_FILL: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 0.027,
    g: 0.027,
    b: 0.031,
    a: 1.0,
};
/// Hairline around the glass pill.
const GLASS_BORDER: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 0.08,
};
const REST_TEXT: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 0.95,
};
/// Mascot: one-shot "has the machine been idle long enough?", the floating "z".
const TIMER_SLEEP: usize = 5;
const TIMER_ZZZ: usize = 6;
/// Equalizer frames, only while music plays and the pill is on screen.
const TIMER_BARS: usize = 7;
const BARS_FRAME_MS: u32 = 33;
/// Collapse delay while window previews are open, and how often a blocked
/// collapse (menu, Start, drag) is checked again.
const PREVIEW_GRACE_MS: u32 = 500;
const BLOCKED_RECHECK_MS: u32 = 300;
/// Hover delay when the mascot is asleep, so that its wake-up hop is seen first.
const WAKE_EXPAND_DELAY_MS: u32 = 320;
/// Pill width while music shows, DIPs, and how long it takes to grow / shrink.
const MUSIC_WIDTH: f32 = 288.0;
const MUSIC_BLEND: std::time::Duration = std::time::Duration::from_millis(420);
/// Sent once after creation to run setup inside the window procedure.
const WM_APP_INIT: u32 = WM_APP + 2;
/// `HSHELL_REDRAW | HSHELL_HIGHBIT`: a window is flashing for attention.
const HSHELL_FLASH: usize = 0x8006;
/// Sent after `TrackMouseEvent(TME_LEAVE)` when the pointer leaves the window.
const WM_MOUSELEAVE: u32 = 0x02A3;
/// Shell hook code: Windows asks where a window minimizes to / restores from.
/// lParam points to a `SHELLHOOKINFO`; the answer is written into its `rc`.
const HSHELL_GETMINRECT: usize = 5;

/// `SHELLHOOKINFO` (winuser.h).
#[repr(C)]
struct ShellHookInfo {
    hwnd: HWND,
    rc: RECT,
}

thread_local! {
    /// The registered "SHELLHOOK" message, readable without borrowing `App`.
    static SHELL_HOOK_MSG: Cell<u32> = const { Cell::new(0) };
    /// `[bar] window_animation`, readable without borrowing `App`.
    static WINDOW_ANIMATION: Cell<bool> = const { Cell::new(true) };
    /// Minimize target used when `App` is busy: the island pill / the bar.
    static MIN_RECT: Cell<RECT> = const { Cell::new(RECT { left: 0, top: 0, right: 0, bottom: 0 }) };
}

pub struct Options {
    pub hide_taskbar: bool,
}

struct App {
    hwnd: HWND,
    options: Options,
    dpi: u32,
    fullscreen_app: bool,
    cleaned_up: bool,
    msg_taskbar_created: u32,
    msg_shell_hook: u32,
    /// `None` only while a paint is in progress (see `paint`).
    renderer: Option<Renderer>,
    icons: IconLoader,
    icon_px: u32,
    tasks: TaskList,
    /// Started in `init` (needs the window); `None` after cleanup.
    status: Option<Status>,
    theme: Theme,
    config: Config,
    /// Last config.toml parse error (shown in the context menu).
    config_error: Option<String>,
    metrics: Metrics,
    pins: Pins,
    /// Whether we currently keep the native taskbar hidden.
    native_hidden: bool,
    /// Element under the pointer, and whether we asked for WM_MOUSELEAVE.
    hover: Option<Hit>,
    tracking_mouse: bool,
    /// Element under the last left-button press, while the button is held.
    pressed: Option<Hit>,
    /// Island mode state; `None` in full mode.
    island: Option<IslandState>,
    /// A context menu (nested loop) is open: the island must not collapse.
    menu_open: bool,
    /// A Windows flyout (Start, Quick Settings, notifications) has the foreground.
    flyout_open: bool,
    /// The foreground window is a fullscreen app on our monitor (backup for
    /// ABN_FULLSCREENAPP, which island mode may not get without reserved space).
    foreground_fullscreen: bool,
    /// Primary monitor and the full bar strip on it, in screen pixels.
    monitor_rect: RECT,
    bar_rect: RECT,
    /// Where the window sits inside bar space, DIPs (non-zero for the island pill).
    origin: (f32, f32),
    /// Island: the part of the window that takes the pointer (screen pixels).
    hit_rect: RECT,
    /// Island: the mascot, and the foreground window that last woke it.
    mascot: Mascot,
    mascot_fg: HWND,
    rest_app: RestApp,
    /// Island: the media watcher, what it reported last, its cover as an icon, and
    /// the equalizer (meter and timer only while bars are on screen).
    media: Option<Media>,
    track: Option<Arc<Track>>,
    art: Option<Icon>,
    music: Blend,
    meter: Option<Meter>,
    bars: Bars,
    /// Island glass style: the blur window below the bar (`None`: black style, or
    /// Windows.UI.Composition unavailable), and whether the backdrop behind the
    /// pill is light (text / icons turn dark).
    glass: Option<Glass>,
    /// Dominant color of the current wallpaper (island_color = "wallpaper"), found
    /// on a thread; `None` until it is ready.
    wallpaper_color: Option<D2D1_COLOR_F>,
    /// Measured widths of the strip's battery percentage and clock.
    strip_text: (f32, f32),
    /// Island strip: eased zoom amount (0..1) of each app button, then Start.
    zoom: Vec<f32>,
    last_frame: Option<Instant>,
    /// Island: window previews of an app with several windows, whether the
    /// pointer is on them, the icon that bounces (group key) and since when.
    preview: Option<Preview>,
    preview_hover: bool,
    bounce: Option<(String, Instant)>,
    /// After a click the strip collapses and stays closed until the pointer has left once.
    suppress_expand: bool,
    /// Collapse the strip when the animation we started (restore / minimize) ends.
    restore_collapse: bool,
    min_collapse: bool,
    /// Minimize / restore animation overlay.
    morph: Option<Morph>,
    /// A window we are restoring ourselves (after the grow-out animation).
    own_restore: Option<(HWND, Instant)>,
}

/// The resting pill's left side: the active app's small icon (`None`: the
/// mascot), crossfading from what it showed before.
#[derive(Default)]
struct RestApp {
    now: Option<Icon>,
    prev: Option<Icon>,
    changed: Option<Instant>,
}

impl RestApp {
    /// Progress of the morph (1 when settled).
    fn progress(&self, now: Instant) -> f32 {
        self.changed.map_or(1.0, |at| {
            let t = now.saturating_duration_since(at).as_secs_f32();
            (t / island::ICON_MORPH.as_secs_f32()).min(1.0)
        })
    }
}

/// How much of the pill shows music (0 = mascot and clock, 1 = music), easing
/// with a light spring toward `target`.
#[derive(Clone, Copy, Default)]
struct Blend {
    from: f32,
    target: f32,
    start: Option<Instant>,
}

impl Blend {
    fn value(&self, now: Instant) -> f32 {
        match self.start {
            Some(at) if now.saturating_duration_since(at) < MUSIC_BLEND => {
                let t = now.saturating_duration_since(at).as_secs_f32();
                island::lerp(self.from, self.target, morph::spring(t, 20.0, 0.78))
            }
            _ => self.target,
        }
    }

    fn moving(&self, now: Instant) -> bool {
        self.start
            .is_some_and(|at| now.saturating_duration_since(at) < MUSIC_BLEND)
    }

    fn go(&mut self, target: f32, now: Instant) -> bool {
        if self.target == target {
            return false;
        }
        *self = Self {
            from: self.value(now),
            target,
            start: Some(now),
        };
        true
    }
}

struct IslandState {
    anim: Island,
    sensor: Option<EdgeSensor>,
    /// The window is currently shrunk to the pill.
    small_window: bool,
    /// An app went into / came out of the island: the pill bounces with its icon.
    pulse: Option<Pulse>,
}

/// One app button: a pinned app, a running app, or a pinned app that is running.
#[derive(Clone, Copy)]
struct BarItem {
    pin: Option<usize>,
    group: Option<usize>,
}

fn metrics_for(config: &Config) -> Metrics {
    Metrics::new(
        config.bar.height,
        config.bar.icon_size,
        config.bar.alignment == Alignment::Left,
        config.bar.position == Position::Top,
    )
}

fn forced_light(config: &Config) -> Option<bool> {
    match config.bar.theme {
        ThemeMode::Auto => None,
        ThemeMode::Dark => Some(false),
        ThemeMode::Light => Some(true),
    }
}

fn scale(dip: f32, dpi: u32) -> i32 {
    (dip * dpi as f32 / 96.0).round() as i32
}

pub fn is_running() -> bool {
    unsafe { FindWindowW(CLASS_NAME, PCWSTR::null()).is_ok() }
}

pub fn quit_running_instance() {
    unsafe {
        if let Ok(hwnd) = FindWindowW(CLASS_NAME, PCWSTR::null()) {
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

pub fn run(options: Options) -> Result<()> {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE).ok()? };
    let result = run_window(options);
    unsafe { CoUninitialize() };
    result
}

fn run_window(options: Options) -> Result<()> {
    let renderer = Renderer::new()?;
    let icons = IconLoader::new()?;
    let (config, config_error) = match config::load() {
        Ok(config) => (config, None),
        Err(err) => (Config::default(), Some(err)),
    };
    unsafe {
        let instance = GetModuleHandleW(None)?.into();
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };
        if RegisterClassExW(&raw const class) == 0 {
            return Err(GetLastError().to_hresult().into());
        }

        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            CLASS_NAME,
            w!("nitrobar"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance),
            None,
        )?;
        safety::set_bar_window(hwnd);

        let app = App {
            hwnd,
            options,
            dpi: 96,
            fullscreen_app: false,
            cleaned_up: false,
            msg_taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
            msg_shell_hook: RegisterWindowMessageW(w!("SHELLHOOK")),
            renderer: Some(renderer),
            icons,
            icon_px: 0,
            tasks: TaskList::new(hwnd),
            status: None,
            theme: Theme::from_system(forced_light(&config)),
            metrics: metrics_for(&config),
            config,
            config_error,
            pins: Pins::load(),
            native_hidden: false,
            hover: None,
            tracking_mouse: false,
            pressed: None,
            island: None,
            menu_open: false,
            flyout_open: false,
            foreground_fullscreen: false,
            monitor_rect: RECT::default(),
            bar_rect: RECT::default(),
            origin: (0.0, 0.0),
            hit_rect: RECT::default(),
            mascot: Mascot::default(),
            mascot_fg: HWND::default(),
            rest_app: RestApp::default(),
            media: None,
            track: None,
            art: None,
            music: Blend::default(),
            meter: None,
            bars: Bars::default(),
            glass: None,
            wallpaper_color: None,
            strip_text: (28.0, 70.0),
            zoom: Vec::new(),
            last_frame: None,
            preview: None,
            preview_hover: false,
            bounce: None,
            suppress_expand: false,
            restore_collapse: false,
            min_collapse: false,
            morph: None,
            own_restore: None,
        };
        SHELL_HOOK_MSG.set(app.msg_shell_hook);
        let cell = Box::into_raw(Box::new(RefCell::new(app)));
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, cell as isize);
        SendMessageW(hwnd, WM_APP_INIT, None, None);

        message_loop(hwnd);
    }
    Ok(())
}

/// Starts watching the config directory (no thread: the handle is waited on by
/// the message loop itself).
fn watch_config_dir() -> Option<HANDLE> {
    let dir = config::dir()?;
    let _ = std::fs::create_dir_all(&dir);
    unsafe {
        FindFirstChangeNotificationW(
            &HSTRING::from(dir.as_os_str()),
            false,
            FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_FILE_NAME,
        )
    }
    .ok()
}

/// A normal message loop that also wakes up when the config directory or the
/// native taskbar's pin list changes.
fn message_loop(hwnd: HWND) {
    let watch = watch_config_dir();
    let pin_watch = winpins::Watcher::new();
    // (handle, message posted when it is signaled)
    let watched: Vec<(HANDLE, u32)> = watch
        .map(|h| (h, WM_APP_CONFIG))
        .into_iter()
        .chain(pin_watch.as_ref().map(|w| (w.event, WM_APP_PINS)))
        .collect();
    let handles: Vec<HANDLE> = watched.iter().map(|(h, _)| *h).collect();
    let mut msg = MSG::default();
    unsafe {
        'outer: loop {
            let wait = MsgWaitForMultipleObjects(Some(&handles), false, INFINITE, QS_ALLINPUT);
            let index = wait.0.wrapping_sub(WAIT_OBJECT_0.0) as usize;
            if let Some(&(handle, message)) = watched.get(index) {
                if message == WM_APP_CONFIG {
                    let _ = FindNextChangeNotification(handle);
                } else if let Some(w) = &pin_watch {
                    w.rearm();
                }
                let _ = PostMessageW(Some(hwnd), message, WPARAM(0), LPARAM(0));
            }
            while PeekMessageW(&raw mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    break 'outer;
                }
                let _ = TranslateMessage(&raw const msg);
                DispatchMessageW(&raw const msg);
            }
        }
        if let Some(handle) = watch {
            let _ = FindCloseChangeNotification(handle);
        }
    }
}

/// Runs `f` with the window's `App`, or returns `None` if it is unavailable / already borrowed.
fn with_app<R>(hwnd: HWND, f: impl FnOnce(&mut App) -> R) -> Option<R> {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const RefCell<App>;
    let cell = unsafe { ptr.as_ref() }?;
    let mut app = cell.try_borrow_mut().ok()?;
    Some(f(&mut app))
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => return LRESULT(1),
        // These only need the HWND, so they also work while App is borrowed.
        WM_ACTIVATE => appbar::activate(hwnd),
        WM_WINDOWPOSCHANGED => appbar::window_pos_changed(hwnd),
        WM_CONTEXTMENU => {
            show_context_menu(hwnd, lparam);
            return LRESULT(0);
        }
        // Sent synchronously while another app minimizes / restores a window: answer
        // even when `App` is borrowed (the app waits for us).
        _ if msg != 0 && msg == SHELL_HOOK_MSG.get() && wparam.0 == HSHELL_GETMINRECT => {
            return on_get_min_rect(hwnd, lparam);
        }
        WM_NCDESTROY => unsafe {
            let ptr = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) as *mut RefCell<App>;
            if !ptr.is_null() {
                drop(Box::from_raw(ptr));
            }
        },
        _ => match with_app(hwnd, |app| app.handle(msg, wparam, lparam)) {
            Some(Some(result)) => return result,
            Some(None) => {}
            None => on_busy(hwnd, msg, wparam, lparam),
        },
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// `App` is borrowed (we are inside one of its handlers, and something pumped
/// messages: a COM call to the shell, a nested loop). Posted messages that carry
/// state must not be lost, or things stall for good: a lost `WM_APP_FRAME` left the
/// frame clock waiting for a message that never came (a restore animation froze
/// and the app was never restored). Frames can simply be skipped; the rest is
/// posted again.
fn on_busy(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) {
    if msg == morph::WM_APP_FRAME {
        morph::frame_handled();
        return;
    }
    let keep = msg == WM_APP_MINIMIZED
        || msg == media::WM_APP_MEDIA
        || msg == wallpaper::WM_APP_WALLPAPER
        || msg == WM_APP_CONFIG
        || msg == WM_APP_PINS
        || msg == preview::WM_APP_PREVIEW
        || msg == island::WM_APP_EDGE
        || msg == tasks::WM_APP_REFRESH
        || msg == status::WM_APP_STATUS
        || msg == appbar::CALLBACK_MSG
        || (msg != 0 && msg == SHELL_HOOK_MSG.get());
    if keep {
        debug_log!("nitrobar: App busy, re-posting message {msg:#x}");
        unsafe {
            let _ = PostMessageW(Some(hwnd), msg, wparam, lparam);
        }
    }
}

/// `HSHELL_GETMINRECT`: the rect a window minimizes to / restores from: the
/// island pill, or the app's button when the bar is shown in full.
///
/// Measured on Windows 11 (26300): the first shell hook window that answers wins,
/// and once we answer, Explorer is not asked and Windows plays *no* minimize /
/// restore animation at all (the window just disappears / appears). Windows'
/// own animation cannot be redirected to the island; we draw our own.
fn on_get_min_rect(hwnd: HWND, lparam: LPARAM) -> LRESULT {
    let Some(info) = (unsafe { (lparam.0 as *mut ShellHookInfo).as_mut() }) else {
        return LRESULT(0);
    };
    let target = info.hwnd;
    let rc = match with_app(hwnd, |app| {
        app.config
            .bar
            .window_animation
            .then(|| app.min_rect(target))
    }) {
        // Animation turned off: let Explorer answer (Windows' own animation).
        Some(None) => return LRESULT(0),
        Some(Some(rc)) => rc,
        // `App` is busy (we are inside its handler): use the last known rect.
        None => WINDOW_ANIMATION.get().then(|| MIN_RECT.get()),
    };
    let Some(rc) = rc else {
        return LRESULT(0);
    };
    if rc.right <= rc.left {
        return LRESULT(0);
    }
    info.rc = rc;
    // Tell `App` (also when it was busy): start our animation once Windows has
    // hidden / shown the window. Windows asks AFTER switching the state: a window
    // that is minimizing is already iconic.
    unsafe {
        let _ = PostMessageW(
            Some(hwnd),
            WM_APP_MINIMIZED,
            WPARAM(target.0 as usize),
            LPARAM(isize::from(IsIconic(target).as_bool())),
        );
    }
    LRESULT(1)
}

/// The menu runs a nested message loop, so `App` is only borrowed before and after it.
fn show_context_menu(hwnd: HWND, lparam: LPARAM) {
    let mut screen = menu::menu_point(lparam);
    let mut client = screen;
    let mut bar = RECT::default();
    unsafe {
        let _ = ScreenToClient(hwnd, &raw mut client);
        let _ = GetWindowRect(hwnd, &raw mut bar);
    }
    // Right click on Start: Windows' own power-user menu (Win+X).
    if with_app(hwnd, |app| app.hit_at(client.x, client.y)) == Some(Some(Hit::Start)) {
        shell::open_power_menu();
        return;
    }
    // The right-clicked app button, plus the menu description. Lists may change
    // while the menu is open, so the command re-finds the app by identity.
    let Some((target, ctx)) = with_app(hwnd, |app| {
        let item = match app.hit_at(client.x, client.y) {
            Some(Hit::Task(i)) => app.items().get(i).copied(),
            _ => None,
        };
        let target = item.map(|item| app.item_app(item));
        let ctx = menu::MenuContext {
            on_app: target.is_some(),
            pinned: item.is_some_and(|item| item.pin.is_some()),
            window_count: item
                .and_then(|item| item.group)
                .map_or(0, |g| app.tasks.groups[g].windows.len()),
            config_error: app.config_error.clone(),
            at_top: app.metrics.at_top,
            island_colors: app.glass_style().then(|| {
                match config::island_color(&app.config.bar.island_color) {
                    config::IslandColor::Wallpaper => menu::IslandColorChoice::Wallpaper,
                    config::IslandColor::Accent => menu::IslandColorChoice::Accent,
                    config::IslandColor::Custom(_) => menu::IslandColorChoice::Custom,
                }
            }),
        };
        (target, ctx)
    }) else {
        return;
    };
    // Open away from the screen edge, from the bar's inner edge, like the Windows 11 taskbar.
    screen.y = if ctx.at_top { bar.bottom } else { bar.top };

    with_app(hwnd, |app| app.menu_open = true);
    let command = menu::show(hwnd, screen, &ctx);
    with_app(hwnd, |app| {
        app.menu_open = false;
        let inside = app.cursor_inside();
        app.island_pointer(inside);
    });
    let Some(command) = command else {
        return;
    };
    match command {
        menu::Command::Exit => unsafe {
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        },
        menu::Command::OpenConfig => {
            if let Some(path) = config::path() {
                pins::open_file(&path);
            }
        }
        // The config file is rewritten; the running bar reloads it and applies the color.
        menu::Command::ColorWallpaper => {
            config::set_bar_value("island_color", "\"wallpaper\"");
        }
        menu::Command::ColorAccent => {
            config::set_bar_value("island_color", "\"accent\"");
        }
        menu::Command::ColorCustom => {
            let initial = with_app(hwnd, |app| {
                let c = app.island_base();
                (
                    (c.r * 255.0).round() as u8,
                    (c.g * 255.0).round() as u8,
                    (c.b * 255.0).round() as u8,
                )
            })
            .unwrap_or((0x5a, 0x6b, 0x85));
            // The dialog is modal: keep the island open behind it.
            with_app(hwnd, |app| app.menu_open = true);
            let picked = menu::choose_color(hwnd, initial);
            with_app(hwnd, |app| {
                app.menu_open = false;
                let inside = app.cursor_inside();
                app.island_pointer(inside);
            });
            if let Some((r, g, b)) = picked {
                config::set_bar_value("island_color", &format!("\"#{r:02X}{g:02X}{b:02X}\""));
            }
        }
        menu::Command::Pin | menu::Command::Unpin | menu::Command::CloseWindows => {
            let Some((key, app_id)) = target else {
                return;
            };
            with_app(hwnd, |app| app.on_app_command(command, &key, app_id));
        }
    }
}

impl App {
    /// Returns `None` to let `DefWindowProc` handle the message.
    fn handle(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        if msg != 0 && msg == self.msg_taskbar_created {
            self.on_explorer_restarted();
            return Some(LRESULT(0));
        }
        if msg != 0 && msg == self.msg_shell_hook {
            self.on_shell_hook(wparam.0, lparam);
            return Some(LRESULT(0));
        }
        match msg {
            WM_APP_INIT => self.init(),
            WM_APP_CONFIG => self.reload_config(),
            WM_APP_PINS => {
                if self.pins.reload_native() {
                    self.pins.load_icons(&self.icons, self.icon_px, false);
                    self.invalidate();
                }
            }
            tasks::WM_APP_REFRESH => self.refresh_tasks(),
            status::WM_APP_STATUS => self.refresh_status(),
            WM_POWERBROADCAST
                if matches!(
                    wparam.0 as u32,
                    PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND
                ) =>
            {
                // Back from sleep / hibernate.
                self.recover();
                return Some(LRESULT(1));
            }
            WM_WTSSESSION_CHANGE if wparam.0 as u32 == WTS_SESSION_UNLOCK => self.recover(),
            WM_DWMCOMPOSITIONCHANGED => self.recover(),
            WM_POWERBROADCAST => {
                if matches!(
                    wparam.0 as u32,
                    PBT_APMPOWERSTATUSCHANGE | PBT_POWERSETTINGCHANGE
                ) && let Some(status) = &mut self.status
                {
                    let before = status.battery;
                    status.refresh_battery();
                    if status.battery != before {
                        self.invalidate();
                    }
                }
                return Some(LRESULT(1));
            }
            appbar::CALLBACK_MSG => self.on_appbar_notify(wparam.0 as u32, lparam),
            WM_TIMER if wparam.0 == TIMER_CLOCK => self.tick_clock(),
            WM_TIMER if wparam.0 == TIMER_EXPAND => {
                self.kill_timer(TIMER_EXPAND);
                self.begin_expand();
            }
            WM_TIMER if wparam.0 == TIMER_COLLAPSE => self.on_collapse_timer(),
            WM_TIMER if wparam.0 == TIMER_PULSE => {
                self.kill_timer(TIMER_PULSE);
                self.start_frames();
            }
            morph::WM_APP_FRAME => self.on_frame(),
            media::WM_APP_MEDIA => self.on_media(),
            WM_TIMER if wparam.0 == TIMER_SLEEP => self.on_sleep_timer(),
            WM_TIMER if wparam.0 == TIMER_ZZZ => {
                if !self.mascot.step_z() {
                    self.kill_timer(TIMER_ZZZ);
                }
                self.invalidate();
            }
            WM_TIMER if wparam.0 == TIMER_BARS => self.on_bars_timer(),
            wallpaper::WM_APP_WALLPAPER => {
                let c = lparam.0 as u32;
                self.wallpaper_color = Some(D2D1_COLOR_F {
                    r: (c & 0xff) as f32 / 255.0,
                    g: ((c >> 8) & 0xff) as f32 / 255.0,
                    b: ((c >> 16) & 0xff) as f32 / 255.0,
                    a: 1.0,
                });
                self.invalidate();
            }
            preview::WM_APP_PREVIEW => match wparam.0 {
                preview::ENTER => {
                    self.preview_hover = true;
                    self.kill_timer(TIMER_COLLAPSE);
                }
                preview::LEAVE => {
                    self.preview_hover = false;
                    if !self.cursor_inside() {
                        self.island_pointer(false);
                    }
                }
                preview::PICK => self.pick_window(HWND(lparam.0 as *mut _)),
                _ => {}
            },
            WM_APP_MINIMIZED => {
                let target = HWND(wparam.0 as *mut _);
                debug_log!(
                    "nitrobar: min / restore event {target:?} minimized={}",
                    lparam.0
                );
                if lparam.0 != 0 {
                    // Minimizing: pour the window into the island / its icon.
                    let started = self
                        .min_rect(target)
                        .is_some_and(|rc| self.start_minimize(target, rc));
                    if !started && self.min_collapse {
                        // No animation to wait for: fold the strip right away.
                        self.min_collapse = false;
                        self.collapse_after_click();
                    }
                } else {
                    // Restoring. Ours already animated; others just make the island bounce.
                    let own = self
                        .own_restore
                        .is_some_and(|(h, at)| h == target && at.elapsed() < OWN_RESTORE_GRACE);
                    if !own {
                        self.start_pulse(target);
                    }
                }
            }
            island::WM_APP_EDGE => {
                // The pointer touched the screen edge (not over a fullscreen game / video).
                let entered = wparam.0 != 0;
                if !entered || !self.fullscreen() {
                    self.island_pointer(entered);
                }
            }
            WM_TIMECHANGE => self.tick_clock(),
            WM_SETTINGCHANGE => {
                // Light/dark mode, accent color, locale or time format may have changed.
                if wparam.0 == wallpaper::SPI_SETDESKWALLPAPER {
                    // New wallpaper (also each picture of a slideshow).
                    self.request_wallpaper(true);
                }
                self.reload_theme(false);
                return None;
            }
            WM_THEMECHANGED | WM_DWMCOLORIZATIONCOLORCHANGED => {
                // DWM may reset window attributes on a theme switch: always re-apply.
                self.reload_theme(true);
                return None;
            }
            WM_DISPLAYCHANGE | WM_DPICHANGED => self.reposition(),
            WM_PAINT => self.paint(),
            WM_MOUSEMOVE => {
                let (x, y) = mouse_pos(lparam);
                self.on_mouse_move(x, y);
            }
            WM_MOUSELEAVE => {
                self.tracking_mouse = false;
                self.island_pointer(false);
                self.set_hover(None);
                if self.pressed.take().is_some() {
                    self.invalidate();
                }
            }
            WM_LBUTTONDOWN => {
                let (x, y) = mouse_pos(lparam);
                self.pressed = self.hit_at(x, y);
                self.invalidate();
            }
            WM_LBUTTONUP => {
                let (x, y) = mouse_pos(lparam);
                let pressed = self.pressed.take();
                let hit = self.hit_at(x, y);
                if hit.is_some() && hit == pressed {
                    self.activate(hit);
                }
                self.invalidate();
            }
            WM_MBUTTONUP => {
                // Middle click: start another instance, like Windows.
                let (x, y) = mouse_pos(lparam);
                if let Some(Hit::Task(i)) = self.hit_at(x, y)
                    && let Some(item) = self.items().get(i).copied()
                {
                    self.launch(item);
                }
            }
            WM_QUERYENDSESSION => return Some(LRESULT(1)),
            WM_ENDSESSION => {
                if wparam.0 != 0 {
                    self.cleanup();
                }
            }
            WM_DESTROY => {
                self.cleanup();
                unsafe { PostQuitMessage(0) };
            }
            // WM_CLOSE falls through to DefWindowProc -> DestroyWindow, outside our borrow.
            _ => return None,
        }
        Some(LRESULT(0))
    }

    fn init(&mut self) {
        appbar::register(self.hwnd);
        self.apply_mode();
        self.apply_theme();
        self.apply_native_taskbar();
        self.reposition();
        tasks::install_hooks(self.hwnd);
        unsafe {
            let _ = RegisterShellHookWindow(self.hwnd);
            // WM_WTSSESSION_CHANGE (unlock) for `recover`.
            let _ = WTSRegisterSessionNotification(self.hwnd, NOTIFY_FOR_THIS_SESSION);
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
        self.status = Some(Status::new(self.hwnd));
        self.apply_style();
        self.morph = Morph::new();
        self.pins.load_icons(&self.icons, self.icon_px, false);
        self.refresh_tasks();
        self.tick_clock();
    }

    fn refresh_status(&mut self) {
        let sources = status::take_pending();
        if let Some(status) = &mut self.status {
            let before = status.view(true);
            status.refresh(sources);
            if status.view(true) != before {
                self.invalidate();
            }
        }
    }

    fn wants_native_hidden(&self) -> bool {
        // Island mode means "no permanent bar": the native taskbar would be one.
        self.options.hide_taskbar
            || self.config.bar.hide_native_taskbar
            || self.config.bar.mode == BarMode::Island
    }

    /// Switches between full (AppBar reserving space) and island mode.
    fn apply_mode(&mut self) {
        let island = self.config.bar.mode == BarMode::Island;
        if island == self.island.is_some() {
            return;
        }
        // Re-registering drops any space reserved by a previous ABM_SETPOS. Island
        // mode stays registered without a position, only to hear ABN_FULLSCREENAPP.
        appbar::unregister(self.hwnd);
        appbar::register(self.hwnd);
        for timer in [TIMER_EXPAND, TIMER_COLLAPSE, TIMER_PULSE] {
            self.kill_timer(timer);
        }
        self.island = island.then(|| IslandState {
            anim: Island::collapsed(),
            sensor: EdgeSensor::new(self.hwnd),
            small_window: true,
            pulse: None,
        });
        self.origin = (0.0, 0.0);
        if !island {
            // Drop the island's window region (full mode uses the whole window).
            self.hit_rect = RECT::default();
            unsafe { SetWindowRgn(self.hwnd, None, true) };
        }
        self.apply_style();
    }

    /// Creates / drops the glass window for `island_style`.
    fn apply_style(&mut self) {
        let want = self.island.is_some() && self.config.bar.island_style == IslandStyle::Glass;
        if want && self.glass.is_none() {
            self.glass = Glass::new();
            debug_log!(
                "nitrobar: glass backdrop available: {}",
                self.glass.is_some()
            );
        } else if !want {
            self.glass = None;
        }
        self.request_wallpaper(false);
        if self.island.is_some() {
            if self.media.is_none() {
                self.media = Media::start(self.hwnd);
            }
            self.arm_sleep();
        } else {
            self.kill_timer(TIMER_SLEEP);
            self.kill_timer(TIMER_ZZZ);
            self.kill_timer(TIMER_BARS);
            self.meter = None;
        }
    }

    // ----- mascot and music -----

    /// The active app takes the mascot's place on the resting pill; on the bare
    /// desktop the mascot is back. Windows without a button (flyouts, dialogs of
    /// the shell) keep what was there.
    fn update_rest_app(&mut self) {
        let foreground = self.tasks.foreground;
        let group = self.tasks.groups.iter().find(|g| g.contains(foreground));
        let icon = match group {
            Some(group) => group.icon.clone(),
            None if island::is_desktop(foreground) => None,
            None => return,
        };
        if self.rest_app.now.as_ref().map(|i| i.id) == icon.as_ref().map(|i| i.id) {
            return;
        }
        self.rest_app.prev = std::mem::replace(&mut self.rest_app.now, icon);
        self.rest_app.changed = Some(Instant::now());
        self.start_frames();
        self.invalidate();
    }

    /// Starts over the countdown to the mascot's nap.
    fn arm_sleep(&mut self) {
        let seconds = self.config.bar.island_sleep_seconds;
        if seconds == 0 {
            self.kill_timer(TIMER_SLEEP);
            if self.mascot.wake(Instant::now()) {
                self.invalidate();
            }
        } else if !self.mascot.asleep() {
            self.set_timer(TIMER_SLEEP, seconds * 1000);
        }
    }

    /// One-shot: asleep if there was no input for long enough, else check again
    /// when that could be the case.
    fn on_sleep_timer(&mut self) {
        self.kill_timer(TIMER_SLEEP);
        let needed = self.config.bar.island_sleep_seconds.saturating_mul(1000);
        if needed == 0 {
            return;
        }
        let idle = mascot::idle_millis();
        if idle >= needed {
            self.mascot.sleep();
            self.set_timer(TIMER_ZZZ, mascot::Z_STEP_MS);
            self.invalidate();
        } else {
            self.set_timer(TIMER_SLEEP, needed - idle);
        }
    }

    /// Returns true if it was asleep.
    fn wake_mascot(&mut self) -> bool {
        if !self.mascot.wake(Instant::now()) {
            return false;
        }
        self.kill_timer(TIMER_ZZZ);
        self.arm_sleep();
        self.start_frames();
        true
    }

    fn waking(&self) -> bool {
        self.mascot.waking(Instant::now())
    }

    fn music_showing(&self) -> bool {
        self.track.as_ref().is_some_and(|t| t.playing)
    }

    /// The media watcher has a new snapshot.
    fn on_media(&mut self) {
        let now = Instant::now();
        let track = media::current();
        let playing = track.as_ref().is_some_and(|t| t.playing);
        // A session that went away keeps its last picture while the pill shrinks.
        if let Some(track) = track {
            let art = track
                .art
                .as_ref()
                .and_then(|px| self.icons.icon_from_pixels(px, track.art_px));
            self.art = art;
            self.track = Some(track);
        } else if let Some(old) = &self.track {
            self.track = Some(Arc::new(Track {
                title: old.title.clone(),
                artist: old.artist.clone(),
                playing: false,
                art: None,
                art_px: old.art_px,
                accent: old.accent,
            }));
        }
        if self.music.go(if playing { 1.0 } else { 0.0 }, now) {
            self.start_frames();
        }
        self.update_bars_timer();
        self.invalidate();
    }

    /// The bars move only while music plays and the pill is on screen.
    fn bars_wanted(&self) -> bool {
        self.music_showing()
            && self.island.as_ref().is_some_and(|s| s.small_window)
            && !(self.fullscreen_app || self.foreground_fullscreen)
    }

    fn update_bars_timer(&mut self) {
        if !self.bars_wanted() {
            self.kill_timer(TIMER_BARS);
            self.meter = None;
        } else if self.meter.is_none() {
            // Fetched on every start: follows the default output device.
            self.meter = Meter::new();
            if self.meter.is_some() {
                self.set_timer(TIMER_BARS, BARS_FRAME_MS);
            }
        }
    }

    fn on_bars_timer(&mut self) {
        if !self.bars_wanted() {
            self.update_bars_timer();
            return;
        }
        let peak = self.meter.as_ref().map_or(0.0, Meter::peak);
        let before = self.bars.levels;
        self.bars.step(peak, BARS_FRAME_MS as f32 / 1000.0);
        // Silence (levels settled): nothing to redraw.
        if before
            .iter()
            .zip(self.bars.levels)
            .any(|(a, b)| (a - b).abs() > 0.01)
        {
            self.paint();
        }
    }

    /// The one accent color of the island: from the playing song's cover, else from
    /// `island_color`; bright enough to read on black.
    fn island_accent(&self) -> D2D1_COLOR_F {
        let cover = self
            .track
            .as_ref()
            .filter(|t| t.playing)
            .and_then(|t| t.accent);
        let base = match cover {
            Some((r, g, b)) => D2D1_COLOR_F {
                r: f32::from(r) / 255.0,
                g: f32::from(g) / 255.0,
                b: f32::from(b) / 255.0,
                a: 1.0,
            },
            None => self.island_base(),
        };
        let (h, sat, _) = wallpaper::to_hsv(base.r, base.g, base.b);
        let sat = if sat > 0.08 {
            sat.clamp(0.42, 0.72)
        } else {
            sat
        };
        let (r, g, b) = wallpaper::from_hsv(h, sat, 0.95);
        D2D1_COLOR_F { r, g, b, a: 1.0 }
    }

    /// Looks the wallpaper color up when the island needs it (see `wallpaper::request`).
    fn request_wallpaper(&self, force: bool) {
        if self.glass_style()
            && config::island_color(&self.config.bar.island_color) == config::IslandColor::Wallpaper
        {
            wallpaper::request(self.hwnd, force);
        }
    }

    /// Glass look (blur, or a translucent fill when the blur is unavailable).
    fn glass_style(&self) -> bool {
        self.island.is_some() && self.config.bar.island_style == IslandStyle::Glass
    }

    fn apply_native_taskbar(&mut self) {
        let wanted = self.wants_native_hidden();
        if wanted && !self.native_hidden {
            taskbar::hide();
        } else if !wanted && self.native_hidden {
            taskbar::restore();
        }
        self.native_hidden = wanted;
    }

    /// config.toml changed on disk: re-apply if it parses and differs.
    fn reload_config(&mut self) {
        match config::load() {
            Ok(config) => {
                self.config_error = None;
                if config != self.config {
                    self.config = config;
                    self.metrics = metrics_for(&self.config);
                    self.theme = Theme::from_system(forced_light(&self.config));
                    self.apply_mode();
                    self.apply_style();
                    self.apply_theme();
                    self.apply_native_taskbar();
                    self.reposition();
                }
            }
            Err(err) => {
                debug_log!("nitrobar: config.toml: {err}");
                self.config_error = Some(err);
            }
        }
    }

    /// Theme palette with the `[colors]` overrides from the config.
    fn palette(&self) -> Palette {
        let mut palette = self.theme.palette();
        let colors = &self.config.colors;
        if let Some(c) = config::parse_color(&colors.background) {
            palette.background = c;
        }
        if let Some(c) = config::parse_color(&colors.text) {
            palette.text = c;
        }
        if let Some(c) = config::parse_color(&colors.accent) {
            palette.accent = c;
        }
        palette
    }

    /// Pinned apps first (in pin order, each sharing one button with its running
    /// windows), then running apps that are not pinned.
    fn items(&self) -> Vec<BarItem> {
        let groups = &self.tasks.groups;
        let mut taken = vec![false; groups.len()];
        let mut items = Vec::with_capacity(self.pins.items.len() + groups.len());
        for (i, pin) in self.pins.items.iter().enumerate() {
            let group = (0..groups.len()).find(|&g| !taken[g] && pin.app.same_app(&groups[g].app));
            if let Some(g) = group {
                taken[g] = true;
            }
            items.push(BarItem {
                pin: Some(i),
                group,
            });
        }
        items.extend((0..groups.len()).filter(|&g| !taken[g]).map(|g| BarItem {
            pin: None,
            group: Some(g),
        }));
        items
    }

    /// Starts (another instance of) an item's app.
    fn launch(&self, item: BarItem) {
        match item.pin.and_then(|p| self.pins.items.get(p)) {
            Some(pin) => pins::launch_pin(pin),
            None => pins::launch(&self.item_app(item).1),
        }
    }

    /// Grouping key and identity of an item.
    fn item_app(&self, item: BarItem) -> (String, AppId) {
        if let Some(group) = item.group.and_then(|g| self.tasks.groups.get(g)) {
            return (group.key.clone(), group.app.clone());
        }
        item.pin
            .and_then(|p| self.pins.items.get(p))
            .map(|pin| (pin.app.key(), pin.app.clone()))
            .unwrap_or_default()
    }

    fn on_app_command(&mut self, command: menu::Command, key: &str, app: AppId) {
        match command {
            menu::Command::Pin => {
                self.pins.pin(app);
                self.pins.load_icons(&self.icons, self.icon_px, false);
            }
            menu::Command::Unpin => {
                // Unpin whichever pin this app shares its button with.
                let pinned = self.pins.find(&app).map(|p| self.pins.items[p].app.clone());
                self.pins.unpin(&pinned.unwrap_or(app));
            }
            menu::Command::CloseWindows => {
                if let Some(index) = self.tasks.group_index(key) {
                    self.tasks.close_group(index);
                }
            }
            menu::Command::OpenConfig
            | menu::Command::Exit
            | menu::Command::ColorWallpaper
            | menu::Command::ColorAccent
            | menu::Command::ColorCustom => {}
        }
        self.invalidate();
    }

    /// (Re-)applies the backdrop and menu theme. Cheap and idempotent: also used to
    /// recover after DWM / session / power events that can drop window attributes.
    fn apply_theme(&mut self) {
        // The island window changes shape while animating and a DWM blur can't
        // follow a rounded, moving panel: it is clear, the panel is drawn by us.
        let wanted = if self.island.is_some() {
            Background::Clear
        } else {
            self.config.bar.background
        };
        // The acrylic tint is the palette background of the requested backdrop.
        self.theme.backdrop = wanted;
        let tint = self.palette().background;
        self.theme.backdrop = theme::apply_backdrop(self.hwnd, self.theme.light, wanted, &tint);
        theme::apply_menu_theme(self.theme.light);
        self.invalidate();
    }

    fn reload_theme(&mut self, force: bool) {
        let fresh = Theme::from_system(forced_light(&self.config));
        if force || fresh.light != self.theme.light || fresh.accent != self.theme.accent {
            self.theme = fresh;
            self.apply_theme();
        }
        self.invalidate();
    }

    /// After unlock, resume or a DWM restart: re-apply everything DWM or the
    /// shell may have dropped, and redraw from a fresh render target, so the bar
    /// can never stay black / stale.
    fn recover(&mut self) {
        if let Some(renderer) = &mut self.renderer {
            renderer.reset();
        }
        self.apply_theme();
        self.reposition();
    }

    fn track_mouse(&mut self) {
        let mut track = TRACKMOUSEEVENT {
            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: self.hwnd,
            dwHoverTime: 0,
        };
        // Posts WM_MOUSELEAVE right away if the pointer is not over the window.
        self.tracking_mouse = unsafe { TrackMouseEvent(&raw mut track) }.is_ok();
    }

    fn on_mouse_move(&mut self, x: i32, y: i32) {
        if !self.tracking_mouse {
            self.track_mouse();
        }
        self.island_pointer(true);
        let hit = self.hit_at(x, y);
        self.set_hover(hit);
    }

    fn set_hover(&mut self, hit: Option<Hit>) {
        if hit != self.hover {
            self.hover = hit;
            self.invalidate();
            if self.island.is_some() {
                // The icon under the pointer zooms in, the one it left zooms out.
                self.start_frames();
            }
        }
    }

    fn activate(&mut self, hit: Option<Hit>) {
        match hit {
            Some(Hit::Start) => shell::open_start(),
            Some(Hit::Task(index)) if self.island.is_some() => self.click_island(index),
            Some(Hit::Task(index)) => match self.items().get(index).copied() {
                Some(BarItem {
                    group: Some(group), ..
                }) => {
                    let target = self.tasks.click_target(group);
                    let animated = target.is_some_and(|hwnd| {
                        unsafe { IsIconic(hwnd) }.as_bool() && self.start_restore(hwnd)
                    });
                    if !animated {
                        self.tasks.click(group);
                    }
                }
                Some(item @ BarItem { pin: Some(_), .. }) => self.launch(item),
                _ => {}
            },
            Some(Hit::Status) => shell::open_quick_settings(),
            Some(Hit::Clock) => shell::open_notification_center(),
            None => {}
        }
    }

    fn on_explorer_restarted(&mut self) {
        // Explorer forgets appbars when it restarts.
        appbar::unregister(self.hwnd);
        appbar::register(self.hwnd);
        if self.native_hidden {
            taskbar::rehide();
        }
        self.recover();
        tasks::request_refresh();
    }

    fn on_shell_hook(&mut self, code: usize, lparam: LPARAM) {
        if code == HSHELL_FLASH {
            if self.tasks.flash(HWND(lparam.0 as *mut _)) {
                self.invalidate();
            }
        } else {
            tasks::request_refresh();
        }
    }

    fn on_appbar_notify(&mut self, code: u32, lparam: LPARAM) {
        match code {
            ABN_POSCHANGED => self.reposition(),
            ABN_FULLSCREENAPP if self.island.is_some() => {
                self.fullscreen_app = lparam.0 != 0;
                debug_log!("nitrobar: ABN_FULLSCREENAPP {}", self.fullscreen_app);
                self.on_fullscreen_changed();
            }
            ABN_FULLSCREENAPP => {
                self.fullscreen_app = lparam.0 != 0;
                unsafe {
                    let _ = SetWindowPos(
                        self.hwnd,
                        Some(self.z_order()),
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                    );
                }
            }
            _ => {}
        }
    }

    fn z_order(&self) -> HWND {
        // HWND_BOTTOM also drops topmost status while a fullscreen app runs.
        if self.fullscreen_app {
            HWND_BOTTOM
        } else {
            HWND_TOPMOST
        }
    }

    /// Docks the bar to the top or bottom of the primary monitor at the current DPI.
    fn reposition(&mut self) {
        unsafe {
            let monitor = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
            let mut info = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if !GetMonitorInfoW(monitor, &raw mut info).as_bool() {
                return;
            }
            let (mut dpi_x, mut dpi_y) = (96, 96);
            if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &raw mut dpi_x, &raw mut dpi_y).is_ok()
            {
                self.dpi = dpi_x;
            }
            let bar_dip = if self.island.is_some() {
                self.island_strip_height()
            } else {
                self.metrics.bar_height
            };
            let height = scale(bar_dip, self.dpi);
            self.monitor_rect = info.rcMonitor;
            if self.island.is_some() {
                let m = info.rcMonitor;
                self.bar_rect = if self.metrics.at_top {
                    RECT {
                        bottom: m.top + height,
                        ..m
                    }
                } else {
                    RECT {
                        top: m.bottom - height,
                        ..m
                    }
                };
            } else {
                let rc = appbar::set_pos(self.hwnd, info.rcMonitor, height, self.metrics.at_top);
                self.bar_rect = rc;
                self.origin = (0.0, 0.0);
                let _ = SetWindowPos(
                    self.hwnd,
                    Some(self.z_order()),
                    rc.left,
                    rc.top,
                    rc.right - rc.left,
                    rc.bottom - rc.top,
                    SWP_NOACTIVATE,
                );
            }
        }
        self.place_island();
        MIN_RECT.set(self.default_min_rect().unwrap_or_default());
        WINDOW_ANIMATION.set(self.config.bar.window_animation);
        if let Some(media) = &self.media {
            let (content, _) = island::pill_padding(self.config.bar.island_height);
            media.set_art_size(scale(content, self.dpi) as u32);
        }
        let icon_px = scale(self.metrics.icon_size, self.dpi) as u32;
        if icon_px != self.icon_px {
            self.icon_px = icon_px;
            self.tasks.reload_icons(&self.icons, icon_px);
            self.pins.load_icons(&self.icons, icon_px, true);
        }
        self.invalidate();
    }

    fn refresh_tasks(&mut self) {
        if self.tasks.refresh(&self.icons, self.icon_px) {
            if self.island.as_ref().is_some_and(|s| !s.small_window) {
                // The strip is open: it grows / shrinks with the list.
                self.place_island();
            }
            self.invalidate();
        }
        if self.native_hidden {
            taskbar::foreground_changed(island::is_shell_flyout(self.tasks.foreground));
        }
        if self.island.is_some() {
            if self.tasks.foreground != self.mascot_fg {
                self.mascot_fg = self.tasks.foreground;
                if !self.wake_mascot() {
                    self.arm_sleep();
                }
            }
            self.update_rest_app();
            self.on_foreground_changed();
        }
    }

    /// Repaints the clock and arms a one-shot timer for the next minute boundary.
    fn tick_clock(&mut self) {
        unsafe {
            SetTimer(
                Some(self.hwnd),
                TIMER_CLOCK,
                clock::ms_until_next_minute(),
                None,
            );
        }
        self.invalidate();
    }

    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    /// Width of the full bar (not of the window, which may be just the pill).
    fn bar_width_dip(&self) -> f32 {
        let mut rc = self.bar_rect;
        if rc.right <= rc.left {
            unsafe {
                let _ = GetClientRect(self.hwnd, &raw mut rc);
            }
        }
        (rc.right - rc.left) as f32 * 96.0 / self.dpi as f32
    }

    fn status_view(&self) -> status::StatusView {
        self.status
            .as_ref()
            .map(|s| s.view(self.config.status.show_battery_percent))
            .unwrap_or_default()
    }

    fn layout(&self) -> BarLayout {
        if self.island.is_some() {
            return Self::strip_as_layout(&self.strip());
        }
        let view = self.status_view();
        let width = layout::status_width(view.icons.len(), view.percent.is_some());
        BarLayout::new(
            self.bar_width_dip(),
            self.items().len(),
            width,
            &self.metrics,
        )
    }

    /// The island strip in the shape drawing and hit-testing use (no Start button).
    fn strip_as_layout(strip: &island::StripLayout) -> BarLayout {
        BarLayout {
            start: strip.start,
            tasks: strip.buttons.clone(),
            status: strip.status,
            clock: strip.clock,
        }
    }

    /// Element at a client-area pixel position.
    fn hit_at(&self, x: i32, y: i32) -> Option<Hit> {
        // The collapsed / collapsing island shows no buttons: nothing to hit.
        if let Some(island) = &self.island
            && (!island.anim.expanded() || island.small_window)
        {
            return None;
        }
        let scale = 96.0 / self.dpi as f32;
        self.layout().hit(
            x as f32 * scale + self.origin.0,
            y as f32 * scale + self.origin.1,
        )
    }

    fn pointer(&self, hit: Hit) -> Pointer {
        Pointer {
            hover: self.hover == Some(hit),
            pressed: self.pressed == Some(hit),
        }
    }

    fn scene(&self) -> Scene<'_> {
        let strip = self.island.is_some().then(|| self.strip());
        let layout = match &strip {
            Some(strip) => Self::strip_as_layout(strip),
            None => self.layout(),
        };
        let palette = if strip.is_some() {
            self.island_palette()
        } else {
            self.palette()
        };
        let foreground = self.tasks.foreground;
        // Island: each button, the status area and the clock roll in / out on their own.
        let (reveals, status_reveal, clock_reveal, start_reveal) = match (&self.island, &strip) {
            (Some(state), Some(strip)) => {
                let middle = |r: &D2D_RECT_F| (r.left + r.right) / 2.0;
                let mut centers: Vec<f32> = vec![middle(&strip.start)];
                centers.extend(layout.tasks.iter().map(middle));
                centers.push(middle(&strip.status));
                centers.push(middle(&strip.clock));
                let mut all = island::reveals(
                    &centers,
                    middle(&strip.panel),
                    self.metrics.icon_size + 18.0,
                    &state.anim,
                );
                // Text only fades and slides: turning it would look broken.
                let calm = |r: Option<island::Reveal>| {
                    let r = r.unwrap_or_default();
                    island::Reveal {
                        angle: 0.0,
                        scale: 0.85 + 0.15 * r.scale,
                        ..r
                    }
                };
                let clock = calm(all.pop());
                let status = calm(all.pop());
                let start = all.remove(0);
                (all, status, clock, start)
            }
            _ => (
                Vec::new(),
                Default::default(),
                Default::default(),
                Default::default(),
            ),
        };
        let flying = self.flying_button();
        let flight = flying.as_ref().and_then(|(index, icon)| {
            let state = self.island.as_ref()?;
            let button = layout.tasks.get(*index)?;
            let from = island::rest_layout(&self.island_geometry().pill).app;
            let to = layout::centered_square(button, self.metrics.icon_size, -1.0);
            let t = state.anim.eased().clamp(0.0, 1.05);
            Some((icon.clone(), island::lerp_rect(&from, &to, t)))
        });
        let flying_index = flight.as_ref().and(flying.as_ref()).map(|(i, _)| *i);
        let buttons = layout
            .tasks
            .iter()
            .zip(self.items())
            .enumerate()
            .map(|(i, (rect, item))| {
                let group = item.group.and_then(|g| self.tasks.groups.get(g));
                let pin = item.pin.and_then(|p| self.pins.items.get(p));
                // Only the hovered icon grows.
                let scale = 1.0 + ZOOM_MAX * self.zoom.get(i).copied().unwrap_or(0.0);
                let hop = self
                    .bounce
                    .as_ref()
                    .filter(|(key, _)| group.is_some_and(|g| &g.key == key))
                    .map_or(0.0, |(_, at)| island::bounce_height(at.elapsed()));
                ButtonView {
                    rect: *rect,
                    icon: group
                        .and_then(|g| g.icon.as_ref())
                        .or_else(|| pin.and_then(|p| p.icon.as_ref())),
                    state: match group {
                        None => ButtonState::Pinned,
                        Some(g) if g.contains(foreground) => ButtonState::Active,
                        Some(g) if g.attention => ButtonState::Attention,
                        Some(g) if g.windows.iter().all(|&w| unsafe { IsIconic(w) }.as_bool()) => {
                            ButtonState::Minimized
                        }
                        Some(_) => ButtonState::Running,
                    },
                    pointer: self.pointer(Hit::Task(i)),
                    scale,
                    lift: (scale - 1.0) * 10.0 + hop,
                    reveal: reveals.get(i).copied().unwrap_or_default(),
                    hide_icon: flying_index == Some(i),
                }
            })
            .collect();
        let mut clock = clock::now(self.config.clock.hour_format);
        if !self.config.clock.show_date {
            clock.date.clear();
        }
        Scene {
            metrics: self.metrics,
            palette,
            backdrop: self.theme.backdrop,
            start: (layout.start, self.pointer(Hit::Start)),
            buttons,
            status_rect: layout.status,
            status_pointer: self.pointer(Hit::Status),
            status: self.status_view(),
            clock_rect: layout.clock,
            clock_pointer: self.pointer(Hit::Clock),
            island: self.island_view(&clock),
            clock,
            divider: strip.map(|s| s.divider),
            status_reveal,
            clock_reveal,
            start_reveal,
            flight,
            // The Start button's zoom follows the apps' in the list.
            start_scale: 1.0 + ZOOM_MAX * self.zoom.get(self.items().len()).copied().unwrap_or(0.0),
            origin: self.origin,
        }
    }

    /// Colors for the content of the island: light on its black panel, whatever the theme.
    fn island_palette(&self) -> Palette {
        // Dark ink on light glass, white on dark glass / the black style.
        let dark_ink = self.dark_ink();
        let ink = if dark_ink { 0.06 } else { 1.0 };
        let white = |a: f32| D2D1_COLOR_F {
            r: ink,
            g: ink,
            b: ink,
            a,
        };
        let mut p = Theme {
            light: dark_ink,
            ..self.theme
        }
        .palette();
        p.text = self.rest_text();
        // One accent color; everything else is black, white or grey.
        p.accent = self.island_accent();
        p.attention = p.accent;
        p.hover = white(0.10);
        p.pressed = white(0.16);
        p.active = white(0.12);
        p.active_hover = white(0.16);
        p.indicator = white(0.45);
        p.placeholder = white(0.18);
        p.attention_bg = D2D1_COLOR_F {
            a: 0.22,
            ..p.attention
        };
        p
    }

    fn paint(&mut self) {
        self.measure_strip_text();
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            BeginPaint(self.hwnd, &raw mut ps);
            // The scene borrows `self`, so the renderer is moved out while drawing.
            if let Some(mut renderer) = self.renderer.take() {
                let scene = self.scene();
                if let (Some(glass), Some(view)) = (&self.glass, &scene.island) {
                    // The blur takes the panel's current shape (moving with the animation).
                    let k = self.dpi as f32 / 96.0;
                    let p = view.panel;
                    glass.set_shape(
                        (p.left - self.origin.0) * k,
                        (p.top - self.origin.1) * k,
                        (p.right - p.left) * k,
                        (p.bottom - p.top) * k,
                        view.radius * k,
                    );
                }
                let result = renderer.draw(self.hwnd, self.dpi, &scene);
                self.renderer = Some(renderer);
                if let Err(err) = result {
                    debug_log!("nitrobar: draw failed: {err}");
                }
            }
            let _ = EndPaint(self.hwnd, &raw const ps);
        }
    }

    /// Bar-space DIPs -> screen pixels.
    fn bar_to_screen(&self, r: &D2D_RECT_F) -> RECT {
        let bar = self.bar_rect;
        RECT {
            left: bar.left + scale(r.left, self.dpi),
            top: bar.top + scale(r.top, self.dpi),
            right: bar.left + scale(r.right, self.dpi),
            bottom: bar.top + scale(r.bottom, self.dpi),
        }
    }

    /// Where `target` minimizes to: its app button while the bar is shown in full,
    /// otherwise the island pill. `None` = let Windows decide.
    fn min_rect(&self, target: HWND) -> Option<RECT> {
        let shown = self
            .island
            .as_ref()
            .is_none_or(|s| s.anim.expanded() && !s.small_window);
        if shown {
            let layout = self.layout();
            let button = self.items().iter().position(|item| {
                item.group
                    .and_then(|g| self.tasks.groups.get(g))
                    .is_some_and(|g| g.contains(target))
            });
            if let Some(rect) = button.and_then(|i| layout.tasks.get(i)) {
                return Some(self.bar_to_screen(rect));
            }
        }
        self.default_min_rect()
    }

    /// The pill in island mode, the whole bar in full mode.
    fn default_min_rect(&self) -> Option<RECT> {
        if self.bar_rect.right <= self.bar_rect.left || self.fullscreen() {
            return None;
        }
        Some(match &self.island {
            Some(_) => self.bar_to_screen(&self.island_geometry().pill),
            None => self.bar_rect,
        })
    }

    // ----- island mode -----

    fn kill_timer(&self, id: usize) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), id);
        }
    }

    fn set_timer(&self, id: usize, ms: u32) {
        unsafe {
            SetTimer(Some(self.hwnd), id, ms.max(1), None);
        }
    }

    fn fullscreen(&self) -> bool {
        self.fullscreen_app
            || self.foreground_fullscreen
            || island::foreground_is_fullscreen(&self.monitor_rect)
    }

    fn cursor_inside(&self) -> bool {
        let mut pt = POINT::default();
        let mut rc = RECT::default();
        unsafe {
            let _ = GetCursorPos(&raw mut pt);
            let _ = GetWindowRect(self.hwnd, &raw mut rc);
        }
        // The island window may be larger than its hover area (window region).
        if self.island.is_some() && self.hit_rect.right > self.hit_rect.left {
            rc = self.hit_rect;
        }
        pt.x >= rc.left && pt.x < rc.right && pt.y >= rc.top && pt.y < rc.bottom
    }

    /// Height of the strip along the screen edge that the island lives in (DIPs).
    fn island_strip_height(&self) -> f32 {
        island::strip_height(&self.config.bar, self.metrics.icon_size)
    }

    /// The clock as shown (the date line only if wanted).
    fn clock_text(&self) -> clock::ClockText {
        let mut clock = clock::now(self.config.clock.hour_format);
        if !self.config.clock.show_date {
            clock.date.clear();
        }
        clock
    }

    /// Measures the texts the strip layout depends on (not while painting: the
    /// renderer is moved out then, and the previous measure is still right).
    fn measure_strip_text(&mut self) {
        if self.island.is_none() {
            return;
        }
        let clock = self.clock_text();
        let status = self.status_view();
        if let Some(renderer) = &self.renderer {
            self.strip_text = renderer.strip_text(status.percent.as_deref(), &clock);
        }
    }

    /// The expanded island: panel, icon buttons and the status area.
    fn strip(&self) -> island::StripLayout {
        let view = self.status_view();
        let (percent, clock) = self.strip_text;
        island::strip_layout(
            self.bar_width_dip(),
            self.island_strip_height(),
            &self.island_config(self.music.target),
            self.metrics.icon_size,
            self.metrics.start_logo_size(),
            self.items().len(),
            island::Tail {
                glyphs: view.icons.len(),
                percent: if view.percent.is_some() { percent } else { 0.0 },
                clock,
            },
            self.metrics.at_top,
        )
    }

    fn island_geometry(&self) -> island::Geometry {
        self.geometry_with(self.island_config(self.music.value(Instant::now())))
    }

    fn geometry_with(&self, config: config::BarConfig) -> island::Geometry {
        island::geometry(
            self.bar_width_dip(),
            self.island_strip_height(),
            &config,
            self.metrics.at_top,
        )
    }

    /// `[bar]` with the pill as wide as the share of music it shows asks for.
    fn island_config(&self, music: f32) -> config::BarConfig {
        let mut config = self.config.bar.clone();
        let wide = config.island_width.max(MUSIC_WIDTH);
        config.island_width = island::lerp(config.island_width, wide, music);
        config
    }

    /// The pointer entered (pill, bar or screen edge) or left. Entering arms the
    /// expand delay (or reverses a collapse right away); leaving arms the collapse delay.
    fn island_pointer(&mut self, inside: bool) {
        let Some(state) = &self.island else {
            return;
        };
        let (expanded, animating) = (state.anim.expanded(), state.anim.animating());
        if inside && self.suppress_expand {
            // Just clicked: stay closed until the pointer has left once.
            return;
        }
        if !inside {
            self.suppress_expand = false;
        }
        if inside {
            self.kill_timer(TIMER_COLLAPSE);
            if expanded {
                return;
            }
            // The pointer wakes the mascot; it gets a moment to hop before the
            // strip opens over it.
            let woke = self.wake_mascot();
            let mut delay = self.config.bar.expand_delay_ms;
            if woke && !self.music_showing() {
                delay = delay.max(WAKE_EXPAND_DELAY_MS);
            }
            if animating || delay == 0 {
                self.begin_expand();
            } else {
                // Not re-armed by every mouse move while waking.
                if woke || !self.waking() {
                    self.set_timer(TIMER_EXPAND, delay);
                }
            }
        } else {
            self.kill_timer(TIMER_EXPAND);
            if expanded {
                let mut delay = self.config.bar.collapse_delay_ms;
                if self.preview.is_some() {
                    // Time to cross the gap from the icon to the window previews.
                    delay = delay.max(PREVIEW_GRACE_MS);
                }
                self.set_timer(TIMER_COLLAPSE, delay);
            }
        }
    }

    /// Things that keep the bar open even without the pointer on it.
    fn collapse_blocked(&self) -> bool {
        // Left button held anywhere: a drag (file, window, scrollbar) is going on.
        let dragging = unsafe { GetAsyncKeyState(i32::from(VK_LBUTTON.0)) } as u16 & 0x8000 != 0;
        self.menu_open
            || self.flyout_open
            || self.pressed.is_some()
            || self.preview_hover
            || dragging
    }

    fn on_collapse_timer(&mut self) {
        self.kill_timer(TIMER_COLLAPSE);
        debug_log!(
            "nitrobar: collapse timer: menu={} flyout={} pressed={} inside={}",
            self.menu_open,
            self.flyout_open,
            self.pressed.is_some(),
            self.cursor_inside()
        );
        if self.collapse_blocked() {
            // Check again later (only while expanded and blocked; not in a hurry).
            let again = self.config.bar.collapse_delay_ms.max(BLOCKED_RECHECK_MS);
            self.set_timer(TIMER_COLLAPSE, again);
        } else if self.cursor_inside() {
            // Pointer is back (no WM_MOUSEMOVE yet): wait for it to leave again.
            self.track_mouse();
        } else {
            self.begin_collapse();
        }
    }

    fn begin_expand(&mut self) {
        debug_log!(
            "nitrobar: expand? abn_fullscreen={} fg_fullscreen={} flyout={}",
            self.fullscreen_app,
            self.foreground_fullscreen,
            self.flyout_open
        );
        if self.fullscreen() && !self.flyout_open {
            return;
        }
        let Some(state) = &mut self.island else {
            return;
        };
        if !state.anim.set_target(true, Instant::now()) {
            return;
        }
        if state.small_window {
            state.small_window = false;
            self.place_island();
        }
        self.start_frames();
        // Leave detection for an expansion not started by the pointer (Win key).
        self.track_mouse();
    }

    fn begin_collapse(&mut self) {
        let Some(state) = &mut self.island else {
            return;
        };
        if state.anim.set_target(false, Instant::now()) {
            self.restore_collapse = false;
            self.min_collapse = false;
            self.close_preview();
            self.set_hover(None);
            self.start_frames();
        }
    }

    /// Whether our minimize / restore animation may run.
    fn window_anim_on(&self) -> bool {
        self.config.bar.window_animation && !self.fullscreen() && morph::system_allows()
    }

    fn start_bounce(&mut self, key: String) {
        debug_log!("nitrobar: bounce {key}");
        self.bounce = Some((key, Instant::now()));
        self.start_frames();
    }

    /// Folds the strip back after a click and keeps it closed until the pointer
    /// has left once (it is still over the spot where the icon was).
    fn collapse_after_click(&mut self) {
        self.suppress_expand = true;
        self.kill_timer(TIMER_EXPAND);
        self.kill_timer(TIMER_COLLAPSE);
        self.begin_collapse();
    }

    /// A click on an app's icon in the island strip.
    fn click_island(&mut self, index: usize) {
        debug_log!("nitrobar: click strip button {index}");
        let Some(item) = self.items().get(index).copied() else {
            return;
        };
        self.restore_collapse = false;
        self.min_collapse = false;
        let Some(group) = item.group else {
            // Pinned, not running: start it.
            self.launch(item);
            let key = self.item_app(item).0;
            self.start_bounce(key);
            return;
        };
        let key = self.tasks.groups[group].key.clone();
        let windows = self.tasks.groups[group].windows.clone();
        if windows.len() > 1 {
            self.toggle_preview(group, index);
            return;
        }
        self.close_preview();
        let Some(&window) = windows.first() else {
            return;
        };
        if unsafe { IsIconic(window) }.as_bool() {
            // Minimized: it pours out of its icon, then the strip folds back.
            let animated = self.window_anim_on() && self.start_restore(window);
            debug_log!("nitrobar: restore click {window:?} animated={animated}");
            if animated {
                self.restore_collapse = true;
            } else {
                self.tasks.click(group);
                self.collapse_after_click();
            }
        } else if window == unsafe { GetForegroundWindow() } {
            // Active: minimize it into its icon.
            self.tasks.click(group);
            if self.window_anim_on() {
                self.min_collapse = true;
            } else {
                self.collapse_after_click();
            }
        } else {
            // Open behind others: bring it to the front, the icon hops.
            self.tasks.click(group);
            self.start_bounce(key);
        }
    }

    fn close_preview(&mut self) {
        self.preview = None;
        self.preview_hover = false;
    }

    /// Shows (or hides) the window previews of the app at strip button `index`.
    fn toggle_preview(&mut self, group: usize, index: usize) {
        let key = self.tasks.groups[group].key.clone();
        let same = self.preview.as_ref().is_some_and(|p| p.key == key);
        self.close_preview();
        if same {
            return;
        }
        let targets = self.tasks.groups[group].windows.clone();
        let strip = self.strip();
        let Some(button) = strip.buttons.get(index) else {
            return;
        };
        let at_top = self.metrics.at_top;
        let bar = self.bar_rect;
        let gap = scale(10.0, self.dpi);
        let anchor_x = bar.left + scale((button.left + button.right) / 2.0, self.dpi);
        let edge_y = if at_top {
            bar.top + scale(strip.panel.bottom, self.dpi) + gap
        } else {
            bar.top + scale(strip.panel.top, self.dpi) - gap
        };
        self.preview = Preview::show(
            self.hwnd,
            key,
            &targets,
            anchor_x,
            edge_y,
            at_top,
            &self.monitor_rect,
            self.dpi,
        );
    }

    /// A window was chosen in the previews.
    fn pick_window(&mut self, window: HWND) {
        self.close_preview();
        if unsafe { IsIconic(window) }.as_bool()
            && self.window_anim_on()
            && self.start_restore(window)
        {
            self.restore_collapse = true;
        } else {
            tasks::activate(window);
            self.collapse_after_click();
        }
    }

    fn start_frames(&self) {
        morph::start_clock(self.hwnd);
    }

    /// One display frame while anything moves: island expand / collapse, the
    /// pulse, the minimize / restore overlay. Stops the frame clock when all rest.
    fn on_frame(&mut self) {
        morph::frame_handled();
        let now = Instant::now();
        let mut running = false;
        let mut repaint = false;

        // Only the icon under the pointer zooms; each one eases in and out on its own.
        if self.island.is_some() {
            let open = self.island.as_ref().is_some_and(|s| !s.small_window);
            // One per app button, then one for Start.
            let count = self.items().len();
            self.zoom.resize(count + 1, 0.0);
            let dt = self
                .last_frame
                .map_or(0.016, |at| now.saturating_duration_since(at).as_secs_f32())
                .min(0.05);
            let k = 1.0 - (-dt / ZOOM_EASE).exp();
            let hovered = self.hover;
            for (i, z) in self.zoom.iter_mut().enumerate() {
                let this = if i == count { Hit::Start } else { Hit::Task(i) };
                let target = if open && hovered == Some(this) {
                    1.0
                } else {
                    0.0
                };
                if (*z - target).abs() > 0.002 {
                    *z += (target - *z) * k;
                    if (*z - target).abs() < 0.01 {
                        *z = target;
                    } else {
                        running = true;
                    }
                    repaint = true;
                }
            }
        }

        if let Some(state) = &mut self.island
            && state.anim.animating()
        {
            let moving = state.anim.tick(now);
            if state.anim.is_collapsed() {
                state.small_window = true;
            }
            if moving {
                running = true;
                repaint = true;
            } else if state.small_window {
                // Fully collapsed: shrink the window back (paints too).
                self.place_island();
            } else {
                repaint = true;
            }
        }

        if let Some(state) = &mut self.island
            && let Some(pulse) = &state.pulse
        {
            if pulse.done(now) {
                state.pulse = None;
                if state.small_window {
                    self.place_island();
                }
            } else {
                repaint = true;
                if pulse.moving(now) {
                    running = true;
                } else {
                    // Holding still: sleep until the return bounce.
                    let hold_end = island::PULSE_HOLD
                        .saturating_sub(now.saturating_duration_since(pulse.started()));
                    self.set_timer(TIMER_PULSE, hold_end.as_millis() as u32);
                }
            }
        }

        if let Some((_, at)) = &self.bounce {
            repaint = true;
            if now.saturating_duration_since(*at) < island::BOUNCE_DURATION {
                running = true;
            } else {
                self.bounce = None;
            }
        }

        if self.suppress_expand
            && self.island.as_ref().is_some_and(|s| s.small_window)
            && !self.cursor_inside()
        {
            self.suppress_expand = false;
        }

        if self.island.is_some() {
            if let Some(at) = self.rest_app.changed {
                repaint = true;
                if now.saturating_duration_since(at) < island::ICON_MORPH {
                    running = true;
                } else {
                    self.rest_app.changed = None;
                    self.rest_app.prev = None;
                }
            }
            if self.mascot.waking(now) {
                running = true;
                repaint = true;
            }
            if self.music.start.is_some() {
                if self.music.moving(now) {
                    running = true;
                } else {
                    self.music.start = None;
                }
                // The pill changed width: its window region and hover zone follow.
                if self.island.as_ref().is_some_and(|s| s.small_window) {
                    self.place_island();
                } else {
                    repaint = true;
                }
            }
        }

        if let Some(morph) = &mut self.morph {
            let tick = morph.tick(now);
            running |= tick.running;
            if tick.swallowed
                && let Some((target, _)) = morph.target()
            {
                if self.min_collapse {
                    // Our click minimized it into the icon: the strip folds back.
                    self.min_collapse = false;
                    self.collapse_after_click();
                }
                self.start_pulse(target);
                running = true;
            }
            if let Some(hwnd) = tick.show {
                debug_log!("nitrobar: restore morph done, activating {hwnd:?}");
                tasks::activate(hwnd);
                if self.restore_collapse {
                    self.restore_collapse = false;
                    self.collapse_after_click();
                    running = true;
                }
            }
        }

        self.last_frame = running.then_some(now);
        if repaint {
            self.paint();
        }
        if !running {
            morph::stop_clock();
        }
    }

    /// Work area and DPI of the monitor `hwnd` is (or restores) on.
    fn window_monitor(hwnd: HWND) -> Option<(RECT, u32)> {
        unsafe {
            let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if !GetMonitorInfoW(monitor, &raw mut info).as_bool() {
                return None;
            }
            let (mut x, mut y) = (96, 96);
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &raw mut x, &raw mut y);
            Some((info.rcWork, x))
        }
    }

    /// `target` was just minimized: pour its picture into the island / its button.
    fn start_minimize(&mut self, target: HWND, to: RECT) -> bool {
        if self.fullscreen() || !morph::system_allows() {
            debug_log!("nitrobar: minimize morph skipped (fullscreen / animations off)");
            return false;
        }
        let Some((work, dpi)) = Self::window_monitor(target) else {
            return false;
        };
        let Some((frame, source)) = morph::restored_frame(target, &work, dpi) else {
            debug_log!("nitrobar: minimize morph: no placement");
            return false;
        };
        if frame.right <= frame.left || frame.bottom <= frame.top {
            return false;
        }
        let Some(morph) = &mut self.morph else {
            return false;
        };
        if morph.start(
            morph::Kind::Minimize,
            target,
            frame,
            source,
            to,
            self.hwnd,
            self.dpi,
            &self.monitor_rect,
        ) {
            debug_log!("nitrobar: minimize morph started {frame:?} -> {to:?}");
            self.start_frames();
            true
        } else {
            debug_log!("nitrobar: minimize morph: thumbnail registration failed");
            false
        }
    }

    /// Grows `hwnd` (minimized) out of the island / its button, then restores it.
    fn start_restore(&mut self, hwnd: HWND) -> bool {
        if !self.config.bar.window_animation || self.fullscreen() || !morph::system_allows() {
            return false;
        }
        let Some(from) = self.min_rect(hwnd) else {
            return false;
        };
        let Some((work, dpi)) = Self::window_monitor(hwnd) else {
            return false;
        };
        let Some((frame, source)) = morph::restored_frame(hwnd, &work, dpi) else {
            return false;
        };
        let Some(morph) = &mut self.morph else {
            return false;
        };
        if !morph.start(
            morph::Kind::Restore,
            hwnd,
            frame,
            source,
            from,
            self.hwnd,
            self.dpi,
            &self.monitor_rect,
        ) {
            return false;
        }
        self.own_restore = Some((hwnd, Instant::now()));
        self.start_pulse(hwnd);
        self.start_frames();
        true
    }

    /// The island bounces with `target`'s app icon (only while it is a pill).
    fn start_pulse(&mut self, target: HWND) {
        if self.fullscreen() {
            return;
        }
        let icon = self
            .tasks
            .groups
            .iter()
            .find(|g| g.contains(target))
            .and_then(|g| g.icon.clone());
        let Some(state) = &mut self.island else {
            return;
        };
        if state.anim.expanded() {
            return;
        }
        state.pulse = Some(Pulse::new(icon, Instant::now()));
        let small = state.small_window;
        self.kill_timer(TIMER_PULSE);
        if small {
            self.place_island();
        }
        self.start_frames();
    }

    /// Sizes / shows the island window for its state: the pill area when collapsed,
    /// the full strip otherwise; hidden over fullscreen apps. Paints at once so a
    /// resized window never shows a stale or black frame.
    fn place_island(&mut self) {
        self.measure_strip_text();
        let Some(state) = &self.island else {
            return;
        };
        let small = state.small_window;
        let pulsing = state.pulse.is_some();
        let hidden = self.fullscreen_app || self.foreground_fullscreen;
        let geometry = self.island_geometry();
        let bar = self.bar_rect;
        let to_px = |dip: f32| scale(dip, self.dpi);
        let to_screen = |r: &D2D_RECT_F| RECT {
            left: bar.left + to_px(r.left),
            top: bar.top + to_px(r.top),
            right: bar.left + to_px(r.right),
            bottom: bar.top + to_px(r.bottom),
        };
        // Collapsed, the window always has room for the pulse; a window region
        // limits it to the pill's hover area otherwise. Changing the region keeps
        // the drawn surface in place (resizing / moving the window shows the old
        // frame at the new position for a frame).
        // Sized for the widest pill (music), so that growing the pill only changes
        // the region.
        let widest = self.geometry_with(self.island_config(1.0));
        let rc = if small {
            to_screen(&widest.activity_hit)
        } else {
            // Expanded: the strip panel's window (with room for its spring overshoot).
            let strip = self.strip().window;
            let a = widest.activity_hit;
            to_screen(&D2D_RECT_F {
                left: strip.left.min(a.left),
                top: strip.top.min(a.top),
                right: strip.right.max(a.right),
                bottom: strip.bottom.max(a.bottom),
            })
        };
        let hit = if !small || pulsing {
            rc
        } else {
            to_screen(&geometry.hit)
        };
        self.hit_rect = hit;
        let px_to_dip = 96.0 / self.dpi as f32;
        self.origin = (
            (rc.left - bar.left) as f32 * px_to_dip,
            (rc.top - bar.top) as f32 * px_to_dip,
        );
        if let Some(sensor) = &state.sensor {
            // Only the middle of the edge expands the island: the pill plus a margin.
            let edge_y = if self.metrics.at_top {
                self.monitor_rect.top
            } else {
                self.monitor_rect.bottom - 1
            };
            sensor.place(
                bar.left + to_px(geometry.pill.left - island::EDGE_ZONE_MARGIN),
                bar.left + to_px(geometry.pill.right + island::EDGE_ZONE_MARGIN),
                edge_y,
                small && !hidden,
            );
        }
        unsafe {
            if hidden {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
                if let Some(glass) = &self.glass {
                    glass.place(&rc, self.hwnd, false);
                }
                return;
            }
            let mut current = RECT::default();
            let _ = GetWindowRect(self.hwnd, &raw mut current);
            let resize = current != rc;
            if resize {
                let _ = SetWindowPos(
                    self.hwnd,
                    Some(HWND_TOPMOST),
                    rc.left,
                    rc.top,
                    rc.right - rc.left,
                    rc.bottom - rc.top,
                    SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
            }
            if let Some(glass) = &self.glass {
                glass.place(&rc, self.hwnd, true);
            }
            let region = (hit != rc).then(|| {
                CreateRectRgn(
                    hit.left - rc.left,
                    hit.top - rc.top,
                    hit.right - rc.left,
                    hit.bottom - rc.top,
                )
            });
            // The system owns the region from here on.
            SetWindowRgn(self.hwnd, region, true);
            if !resize {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }
        }
        #[cfg(debug_assertions)]
        if !small {
            self.debug_strip();
        }
        self.update_bars_timer();
        self.paint();
    }

    /// Debug builds: logs where each strip button is (screen pixels), for UI tests.
    #[cfg(debug_assertions)]
    fn debug_strip(&self) {
        let strip = self.strip();
        for (rect, item) in strip.buttons.iter().zip(self.items()) {
            let x = self.bar_rect.left + scale((rect.left + rect.right) / 2.0, self.dpi);
            let y = self.bar_rect.top + scale((rect.top + rect.bottom) / 2.0, self.dpi);
            debug_log!("BTN {} {x} {y}", self.item_app(item).0);
        }
    }

    fn on_fullscreen_changed(&mut self) {
        if self.fullscreen_app || self.foreground_fullscreen {
            for timer in [TIMER_EXPAND, TIMER_COLLAPSE, TIMER_PULSE] {
                self.kill_timer(timer);
            }
            if let Some(state) = &mut self.island {
                state.anim = Island::collapsed();
                state.small_window = true;
                state.pulse = None;
            }
            if let Some(morph) = &mut self.morph {
                morph.finish();
            }
        }
        self.place_island();
    }

    /// Foreground changed: Windows flyouts keep the bar open (Win opens Start, so
    /// pressing Win expands it); a fullscreen game / video hides the pill.
    fn on_foreground_changed(&mut self) {
        let foreground = self.tasks.foreground;
        let fullscreen = island::foreground_is_fullscreen(&self.monitor_rect);
        if fullscreen != self.foreground_fullscreen {
            self.foreground_fullscreen = fullscreen;
            self.on_fullscreen_changed();
        }
        let flyout = island::is_shell_flyout(foreground);
        if flyout == self.flyout_open {
            return;
        }
        self.flyout_open = flyout;
        if flyout {
            self.kill_timer(TIMER_EXPAND);
            self.kill_timer(TIMER_COLLAPSE);
            self.begin_expand();
        } else if !self.cursor_inside() {
            self.island_pointer(false);
        }
    }

    /// The island's tint color (glass style): the wallpaper's dominant color,
    /// Windows' accent color, or the color the user picked.
    fn island_base(&self) -> D2D1_COLOR_F {
        let accent = D2D1_COLOR_F {
            a: 1.0,
            ..self.theme.accent
        };
        match config::island_color(&self.config.bar.island_color) {
            config::IslandColor::Custom(c) => c,
            config::IslandColor::Accent => accent,
            config::IslandColor::Wallpaper => self.wallpaper_color.unwrap_or(accent),
        }
    }

    /// Text and icons are dark on a light tint color, white on a dark one: decided
    /// once from the color (not from what happens to be behind the island).
    fn dark_ink(&self) -> bool {
        let c = self.island_base();
        self.glass_style() && 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b > 0.58
    }

    /// Tint laid over the blur.
    fn glass_tint(&self) -> D2D1_COLOR_F {
        let mut c = self.island_base();
        // Without the blur window there is nothing to see through: denser tint.
        let opacity = self.config.bar.island_opacity;
        c.a = if self.glass.is_some() {
            opacity
        } else {
            (opacity + 0.3).min(1.0)
        };
        c
    }

    /// Text color on the island: dark on a light tint, light otherwise.
    fn rest_text(&self) -> D2D1_COLOR_F {
        if self.dark_ink() {
            D2D1_COLOR_F {
                r: 0.06,
                g: 0.06,
                b: 0.07,
                a: 0.95,
            }
        } else {
            REST_TEXT
        }
    }

    /// While the island opens or closes: the strip button of the app whose icon
    /// rests on the pill, which then travels between the two places.
    fn flying_button(&self) -> Option<(usize, Icon)> {
        let state = self.island.as_ref()?;
        if !state.anim.animating() || self.mascot.asleep() {
            return None;
        }
        let icon = self.rest_app.now.as_ref()?;
        let index = self.items().iter().position(|item| {
            item.group
                .and_then(|g| self.tasks.groups.get(g))
                .and_then(|g| g.icon.as_ref())
                .is_some_and(|i| i.id == icon.id)
        })?;
        Some((index, icon.clone()))
    }

    /// How the island looks right now (`None` in full mode).
    fn island_view(&self, clock: &clock::ClockText) -> Option<IslandView> {
        let state = self.island.as_ref()?;
        let g = self.island_geometry();
        let t = state.anim.eased();
        let t_clamped = t.clamp(0.0, 1.0);
        let attention = self.tasks.groups.iter().any(|g| g.attention);
        let glass = self.glass_style();
        let accent = self.island_accent();
        let pill = if glass { self.glass_tint() } else { REST_FILL };
        let strip = self.strip().panel;
        // An app going in / out: the pill grows (with spring overshoot) and shows the
        // app's icon instead of its usual content.
        let now = Instant::now();
        let pulse = state.pulse.as_ref().map_or(0.0, |p| p.value(now));
        let deep = pulse.clamp(0.0, 1.0);
        let pill_rect = island::pulsed_pill(&g, pulse);
        let squash = if self.config.bar.island_squash {
            state.anim.squash()
        } else {
            0.0
        };
        let panel = island::morph_panel(&pill_rect, &strip, t, squash);
        // No hairline on the black pill (an anti-aliased 1 DIP line along a curve
        // looks uneven); the glass keeps a faint one.
        let mut border = if glass {
            GLASS_BORDER
        } else {
            D2D1_COLOR_F::default()
        };
        // An app asking for attention: the hairline takes the accent color.
        if attention {
            border = D2D1_COLOR_F { a: 0.95, ..accent };
        }

        // Collapsed content: the mascot and the time, or the music.
        let pill_h = g.pill.bottom - g.pill.top;
        let pill_w = g.pill.right - g.pill.left;
        let places = island::rest_layout(&g.pill);
        let rest_opacity = island::rest_opacity(&state.anim) * (1.0 - deep);
        let music_mix = self.music.value(now).clamp(0.0, 1.0);
        let text = self.rest_text();
        let secondary = D2D1_COLOR_F {
            a: text.a * 0.6,
            ..text
        };
        // Asleep (nobody at the machine) the mascot is shown whatever is open.
        let asleep = self.mascot.asleep();
        let (leaving, arriving) = island::icon_morph(self.rest_app.progress(now));
        let settled = self.rest_app.changed.is_none();
        // While the island moves, the active app's icon is the one flying between
        // the pill and the strip: not drawn here too.
        let flying = self.flying_button().is_some();
        let mut apps = Vec::new();
        if !asleep && !flying && pill_h >= 20.0 {
            if let Some(icon) = self.rest_app.prev.as_ref().filter(|_| !settled) {
                apps.push((icon.clone(), leaving));
            }
            if let Some(icon) = &self.rest_app.now {
                let look = if settled {
                    island::Reveal::SHOWN
                } else {
                    arriving
                };
                apps.push((icon.clone(), look));
            }
        }
        // The mascot takes part in the same morph when it comes or goes.
        let mascot_look = if asleep {
            Some(island::Reveal::SHOWN)
        } else {
            match (&self.rest_app.now, &self.rest_app.prev) {
                (None, _) if settled => Some(island::Reveal::SHOWN),
                (None, _) => Some(arriving),
                (Some(_), None) if !settled => Some(leaving),
                _ => None,
            }
        };
        let mascot = mascot_look
            .filter(|_| pill_h >= 20.0)
            .map(|look| MascotView {
                opacity: look.alpha,
                rect: places.mascot,
                asleep,
                scale: self.mascot.scale(now) * look.scale,
                z: self.mascot.z_phase(),
            });
        let clock_text = (self.config.bar.island_show_clock && pill_w >= 100.0)
            .then(|| (clock.time.clone(), places.clock));
        let music = self
            .track
            .as_ref()
            .filter(|_| music_mix > 0.01)
            .map(|track| MusicView {
                art: self.art.clone(),
                art_rect: places.art,
                title: track.title.clone(),
                title_rect: places.title,
                artist: track.artist.clone(),
                artist_rect: places.artist,
                bars: self.bars.levels,
                bars_rect: places.bars,
            });
        Some(IslandView {
            panel,
            // Fully rounded at every size, like the Dynamic Island.
            radius: (panel.bottom - panel.top) / 2.0,
            fill: pill,
            border,
            // Continuous corners; the glass keeps round ones to match its blur clip.
            squircle: !glass,
            // The glass is see-through: a shadow under it would darken the blur.
            shadow: if glass { 0.0 } else { 1.0 },
            content: !state.anim.is_collapsed(),
            clip: state.anim.animating(),
            rest: Some(RestView {
                opacity: rest_opacity,
                music_mix,
                mascot,
                apps,
                app_rect: places.app,
                clock: clock_text,
                music,
                text,
                secondary,
                accent,
            }),
            badge: state
                .pulse
                .as_ref()
                .and_then(|p| p.icon.clone())
                .map(|icon| Badge {
                    icon,
                    rect: layout::centered_square(&pill_rect, self.metrics.icon_size, 0.0),
                    opacity: ((pulse - 0.45) / 0.45).clamp(0.0, 1.0)
                        * (1.0 - 3.0 * t_clamped).max(0.0),
                }),
        })
    }

    fn cleanup(&mut self) {
        if self.cleaned_up {
            return;
        }
        self.cleaned_up = true;
        tasks::remove_hooks();
        self.close_preview();
        self.glass = None;
        morph::stop_clock();
        self.morph = None;
        self.island = None;
        if let Some(mut status) = self.status.take() {
            status.shutdown();
        }
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_CLOCK);
            let _ = DeregisterShellHookWindow(self.hwnd);
            let _ = WTSUnRegisterSessionNotification(self.hwnd);
        }
        safety::emergency_cleanup();
    }
}

fn mouse_pos(lparam: LPARAM) -> (i32, i32) {
    (
        i32::from(lparam.0 as u16 as i16),
        i32::from((lparam.0 >> 16) as u16 as i16),
    )
}
