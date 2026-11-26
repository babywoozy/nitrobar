//! The bar window: creation, message loop and window procedure.
//!
//! State lives in a `RefCell<App>` behind `GWLP_USERDATA`. Win32 re-enters the
//! window procedure synchronously (e.g. `SetWindowPos` sends `WM_WINDOWPOSCHANGED`),
//! so messages that arrive while `App` is borrowed fall back to stateless
//! handling / `DefWindowProc` instead of aliasing `&mut App`.

use std::cell::{Cell, RefCell};
use std::time::Instant;

use windows::Win32::Foundation::{
    GetLastError, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, WAIT_OBJECT_0, WPARAM,
};
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetMonitorInfoW, InvalidateRect, MONITOR_DEFAULTTOPRIMARY, MONITORINFO,
    MonitorFromPoint, PAINTSTRUCT, ScreenToClient,
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
    GetCursorPos, GetWindowRect, MsgWaitForMultipleObjects, PBT_APMPOWERSTATUSCHANGE,
    PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND, PBT_POWERSETTINGCHANGE, PM_REMOVE, PeekMessageW,
    QS_ALLINPUT, SW_HIDE, SWP_SHOWWINDOW, WM_DWMCOLORIZATIONCOLORCHANGED, WM_DWMCOMPOSITIONCHANGED,
    WM_MBUTTONUP, WM_MOUSEMOVE, WM_POWERBROADCAST, WM_QUIT, WM_THEMECHANGED, WM_WTSSESSION_CHANGE,
    WTS_SESSION_UNLOCK,
};
use windows::core::{HSTRING, PCWSTR, Result, w};

use crate::config::{self, Alignment, Background, BarMode, Config, Position, ThemeMode};
use crate::icons::IconLoader;
use crate::island::{self, EdgeSensor, Island};
use crate::layout::{BarLayout, Hit, Metrics};
use crate::pins::{self, Pins};
use crate::render::IslandView;
use crate::render::{ButtonState, ButtonView, Pointer, Renderer, Scene};
use crate::status::{self, Status};
use crate::tasks::AppId;
use crate::tasks::{self, TaskList};
use crate::theme::Palette;
use crate::theme::{self, Theme};
use crate::{appbar, clock, layout, menu, safety, shell, taskbar, winpins};

/// Posted by the message loop when a file in the config directory changed.
const WM_APP_CONFIG: u32 = WM_APP + 5;
/// Posted by the message loop when the native taskbar's pin list changed.
const WM_APP_PINS: u32 = WM_APP + 6;

const CLASS_NAME: PCWSTR = w!("NitrobarWindow");
const TIMER_CLOCK: usize = 1;
/// Island: hover delay before expanding, delay before collapsing, animation frames.
const TIMER_EXPAND: usize = 2;
const TIMER_COLLAPSE: usize = 3;
const TIMER_ANIM: usize = 4;
/// Glass opacity of the expanded island bar with `background = "clear"` (it lies
/// over app windows; the collapsed pill uses the lighter `palette.island`).
const ISLAND_EXPANDED_ALPHA: f32 = 0.85;
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
}

struct IslandState {
    anim: Island,
    sensor: Option<EdgeSensor>,
    /// The window is currently shrunk to the pill.
    small_window: bool,
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
        _ => {
            if let Some(Some(result)) = with_app(hwnd, |app| app.handle(msg, wparam, lparam)) {
                return result;
            }
        }
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
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
    let rc = with_app(hwnd, |app| app.min_rect(info.hwnd))
        .flatten()
        .unwrap_or_else(|| MIN_RECT.get());
    if rc.right <= rc.left {
        return LRESULT(0);
    }
    info.rc = rc;
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
            WM_TIMER if wparam.0 == TIMER_ANIM => self.animate(),
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
        for timer in [TIMER_EXPAND, TIMER_COLLAPSE, TIMER_ANIM] {
            self.kill_timer(timer);
        }
        self.island = island.then(|| IslandState {
            anim: Island::collapsed(),
            sensor: EdgeSensor::new(self.hwnd),
            small_window: true,
        });
        self.origin = (0.0, 0.0);
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
            menu::Command::OpenConfig | menu::Command::Exit => {}
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
        }
    }

    fn activate(&mut self, hit: Option<Hit>) {
        match hit {
            Some(Hit::Start) => shell::open_start(),
            Some(Hit::Task(index)) => match self.items().get(index).copied() {
                Some(BarItem {
                    group: Some(group), ..
                }) => self.tasks.click(group),
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
            let height = scale(self.metrics.bar_height, self.dpi);
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
            self.invalidate();
        }
        if self.native_hidden {
            taskbar::foreground_changed(island::is_shell_flyout(self.tasks.foreground));
        }
        if self.island.is_some() {
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
        let view = self.status_view();
        let width = layout::status_width(view.icons.len(), view.percent.is_some());
        BarLayout::new(
            self.bar_width_dip(),
            self.items().len(),
            width,
            &self.metrics,
        )
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
        let layout = self.layout();
        let foreground = self.tasks.foreground;
        let buttons = layout
            .tasks
            .iter()
            .zip(self.items())
            .enumerate()
            .map(|(i, (rect, item))| {
                let group = item.group.and_then(|g| self.tasks.groups.get(g));
                let pin = item.pin.and_then(|p| self.pins.items.get(p));
                ButtonView {
                    rect: *rect,
                    icon: group
                        .and_then(|g| g.icon.as_ref())
                        .or_else(|| pin.and_then(|p| p.icon.as_ref())),
                    state: match group {
                        None => ButtonState::Pinned,
                        Some(g) if g.contains(foreground) => ButtonState::Active,
                        Some(g) if g.attention => ButtonState::Attention,
                        Some(_) => ButtonState::Running,
                    },
                    pointer: self.pointer(Hit::Task(i)),
                }
            })
            .collect();
        let mut clock = clock::now();
        if !self.config.clock.show_date {
            clock.date.clear();
        }
        Scene {
            metrics: self.metrics,
            palette: self.palette(),
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
            origin: self.origin,
        }
    }

    fn paint(&mut self) {
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            BeginPaint(self.hwnd, &raw mut ps);
            // The scene borrows `self`, so the renderer is moved out while drawing.
            if let Some(mut renderer) = self.renderer.take() {
                let result = renderer.draw(self.hwnd, self.dpi, &self.scene());
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
        pt.x >= rc.left && pt.x < rc.right && pt.y >= rc.top && pt.y < rc.bottom
    }

    fn island_geometry(&self) -> island::Geometry {
        island::geometry(
            self.bar_width_dip(),
            self.metrics.bar_height,
            &self.config.bar,
            self.metrics.at_top,
        )
    }

    /// The pointer entered (pill, bar or screen edge) or left. Entering arms the
    /// expand delay (or reverses a collapse right away); leaving arms the collapse delay.
    fn island_pointer(&mut self, inside: bool) {
        let Some(state) = &self.island else {
            return;
        };
        let (expanded, animating) = (state.anim.expanded(), state.anim.animating());
        if inside {
            self.kill_timer(TIMER_COLLAPSE);
            if expanded {
                return;
            }
            if animating {
                self.begin_expand();
            } else {
                self.set_timer(TIMER_EXPAND, self.config.bar.expand_delay_ms);
            }
        } else {
            self.kill_timer(TIMER_EXPAND);
            if expanded {
                self.set_timer(TIMER_COLLAPSE, self.config.bar.collapse_delay_ms);
            }
        }
    }

    /// Things that keep the bar open even without the pointer on it.
    fn collapse_blocked(&self) -> bool {
        // Left button held anywhere: a drag (file, window, scrollbar) is going on.
        let dragging = unsafe { GetAsyncKeyState(i32::from(VK_LBUTTON.0)) } as u16 & 0x8000 != 0;
        self.menu_open || self.flyout_open || self.pressed.is_some() || dragging
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
            // Check again later (only while expanded and blocked).
            self.set_timer(TIMER_COLLAPSE, self.config.bar.collapse_delay_ms);
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
        self.set_timer(TIMER_ANIM, island::FRAME_MS);
        // Leave detection for an expansion not started by the pointer (Win key).
        self.track_mouse();
    }

    fn begin_collapse(&mut self) {
        let Some(state) = &mut self.island else {
            return;
        };
        if state.anim.set_target(false, Instant::now()) {
            self.set_hover(None);
            self.set_timer(TIMER_ANIM, island::FRAME_MS);
        }
    }

    /// One animation frame (only runs while the island moves).
    fn animate(&mut self) {
        let Some(state) = &mut self.island else {
            self.kill_timer(TIMER_ANIM);
            return;
        };
        let moving = state.anim.tick(Instant::now());
        let collapsed = state.anim.is_collapsed();
        if collapsed {
            state.small_window = true;
        }
        if !moving {
            self.kill_timer(TIMER_ANIM);
            if collapsed {
                self.place_island();
                return;
            }
        }
        self.invalidate();
    }

    /// Sizes / shows the island window for its state: the pill area when collapsed,
    /// the full strip otherwise; hidden over fullscreen apps. Paints at once so a
    /// resized window never shows a stale or black frame.
    fn place_island(&mut self) {
        let Some(state) = &self.island else {
            return;
        };
        let small = state.small_window;
        let hidden = self.fullscreen_app || self.foreground_fullscreen;
        let geometry = self.island_geometry();
        let bar = self.bar_rect;
        let to_px = |dip: f32| scale(dip, self.dpi);
        let rc = if small {
            RECT {
                left: bar.left + to_px(geometry.hit.left),
                top: bar.top + to_px(geometry.hit.top),
                right: bar.left + to_px(geometry.hit.right),
                bottom: bar.top + to_px(geometry.hit.bottom),
            }
        } else {
            bar
        };
        let px_to_dip = 96.0 / self.dpi as f32;
        self.origin = (
            (rc.left - bar.left) as f32 * px_to_dip,
            (rc.top - bar.top) as f32 * px_to_dip,
        );
        if let Some(sensor) = &state.sensor {
            sensor.place(&self.monitor_rect, self.metrics.at_top, small && !hidden);
        }
        unsafe {
            if hidden {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
                return;
            }
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
        self.paint();
    }

    fn on_fullscreen_changed(&mut self) {
        if self.fullscreen_app || self.foreground_fullscreen {
            for timer in [TIMER_EXPAND, TIMER_COLLAPSE, TIMER_ANIM] {
                self.kill_timer(timer);
            }
            if let Some(state) = &mut self.island {
                state.anim = Island::collapsed();
                state.small_window = true;
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

    /// How the island looks right now (`None` in full mode).
    fn island_view(&self, clock: &clock::ClockText) -> Option<IslandView> {
        let state = self.island.as_ref()?;
        let g = self.island_geometry();
        let t = state.anim.eased();
        let palette = self.palette();
        let attention = self.tasks.groups.iter().any(|g| g.attention);
        let mut pill = if attention {
            palette.attention
        } else {
            palette.island
        };
        if attention {
            pill.a = 0.9;
        }
        // The expanded fill follows `[bar] background` (clear = none, solid = opaque).
        let mut full = Theme {
            backdrop: self.config.bar.background,
            ..self.theme
        }
        .palette()
        .background;
        if let Some(c) = config::parse_color(&self.config.colors.background) {
            full = c;
        }
        if full.a == 0.0 {
            // The expanded island lies over app windows (it reserves no space): a
            // fully clear bar would mix its text with theirs. Keep the pill's glass.
            full = palette.island;
            full.a = ISLAND_EXPANDED_ALPHA;
        }
        let mix = |a: f32, b: f32| island::lerp(a, b, t);
        let pill_radius = (g.pill.bottom - g.pill.top) / 2.0;
        let mut border = palette.island_border;
        // The hairline stays (fainter) on the expanded bar as its edge toward the windows.
        border.a *= if attention { 0.0 } else { 1.0 - 0.5 * t };
        Some(IslandView {
            panel: island::lerp_rect(&g.pill, &g.bar, t),
            radius: mix(pill_radius, 0.0),
            fill: D2D1_COLOR_F {
                r: mix(pill.r, full.r),
                g: mix(pill.g, full.g),
                b: mix(pill.b, full.b),
                a: mix(pill.a, full.a),
            },
            border,
            // Icons appear over the last two thirds of the expansion.
            content_opacity: ((t - 0.35) / 0.65).clamp(0.0, 1.0),
            clock: self
                .config
                .bar
                .island_show_clock
                .then(|| clock.time.clone()),
            text: palette.text,
        })
    }

    fn cleanup(&mut self) {
        if self.cleaned_up {
            return;
        }
        self.cleaned_up = true;
        tasks::remove_hooks();
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
