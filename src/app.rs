//! The bar window: creation, message loop and window procedure.
//!
//! State lives in a `RefCell<App>` behind `GWLP_USERDATA`. Win32 re-enters the
//! window procedure synchronously (e.g. `SetWindowPos` sends `WM_WINDOWPOSCHANGED`),
//! so messages that arrive while `App` is borrowed fall back to stateless
//! handling / `DefWindowProc` instead of aliasing `&mut App`.

use std::cell::RefCell;

use windows::Win32::Foundation::{GetLastError, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetMonitorInfoW, InvalidateRect, MONITOR_DEFAULTTOPRIMARY, MONITORINFO,
    MonitorFromPoint, PAINTSTRUCT, ScreenToClient,
};
use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Shell::{ABN_FULLSCREENAPP, ABN_POSCHANGED};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DeregisterShellHookWindow, DispatchMessageW, FindWindowW,
    GWLP_USERDATA, GetClientRect, GetMessageW, GetWindowLongPtrW, HWND_BOTTOM, HWND_TOPMOST,
    IDC_ARROW, KillTimer, LoadCursorW, MSG, PostMessageW, PostQuitMessage, RegisterClassExW,
    RegisterShellHookWindow, RegisterWindowMessageW, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SendMessageW, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    TranslateMessage, WM_ACTIVATE, WM_APP, WM_CLOSE, WM_CONTEXTMENU, WM_DESTROY, WM_DISPLAYCHANGE,
    WM_DPICHANGED, WM_ENDSESSION, WM_ERASEBKGND, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_NCDESTROY,
    WM_PAINT, WM_QUERYENDSESSION, WM_SETTINGCHANGE, WM_TIMECHANGE, WM_TIMER, WM_WINDOWPOSCHANGED,
    WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{PCWSTR, Result, w};

use crate::icons::IconLoader;
use crate::render::{ButtonState, ButtonView, Renderer, Scene};
use crate::tasks::{self, TaskList};
use crate::{appbar, clock, layout, menu, safety, taskbar};

const CLASS_NAME: PCWSTR = w!("NitrobarWindow");
const TIMER_CLOCK: usize = 1;
/// Sent once after creation to run setup inside the window procedure.
const WM_APP_INIT: u32 = WM_APP + 2;
/// `HSHELL_REDRAW | HSHELL_HIGHBIT`: a window is flashing for attention.
const HSHELL_FLASH: usize = 0x8006;

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
    renderer: Renderer,
    icons: IconLoader,
    icon_px: u32,
    tasks: TaskList,
    /// Button index under the last left-button press.
    pressed: Option<usize>,
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
            renderer,
            icons,
            icon_px: 0,
            tasks: TaskList::new(hwnd),
            pressed: None,
        };
        let cell = Box::into_raw(Box::new(RefCell::new(app)));
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, cell as isize);
        SendMessageW(hwnd, WM_APP_INIT, None, None);

        let mut msg = MSG::default();
        while GetMessageW(&raw mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&raw const msg);
            DispatchMessageW(&raw const msg);
        }
    }
    Ok(())
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

/// The menu runs a nested message loop, so `App` is only borrowed before and after it.
fn show_context_menu(hwnd: HWND, lparam: LPARAM) {
    let screen = menu::menu_point(lparam);
    let mut client = screen;
    unsafe {
        let _ = ScreenToClient(hwnd, &raw mut client);
    }
    let target = with_app(hwnd, |app| {
        app.button_at(client.x, client.y).map(|i| {
            let group = &app.tasks.groups[i];
            (group.key.clone(), group.windows.len())
        })
    })
    .flatten();
    let window_count = target.as_ref().map_or(0, |(_, count)| *count);

    match menu::show(hwnd, screen, window_count) {
        Some(menu::Command::Exit) => unsafe {
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        },
        Some(menu::Command::CloseWindows) => {
            if let Some((key, _)) = target {
                with_app(hwnd, |app| {
                    if let Some(index) = app.tasks.group_index(&key) {
                        app.tasks.close_group(index);
                    }
                });
            }
        }
        None => {}
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
            tasks::WM_APP_REFRESH => self.refresh_tasks(),
            appbar::CALLBACK_MSG => self.on_appbar_notify(wparam.0 as u32, lparam),
            WM_TIMER if wparam.0 == TIMER_CLOCK => self.tick_clock(),
            WM_TIMECHANGE => self.tick_clock(),
            WM_SETTINGCHANGE => {
                // Locale / time format may have changed.
                self.invalidate();
                return None;
            }
            WM_DISPLAYCHANGE | WM_DPICHANGED => self.reposition(),
            WM_PAINT => self.paint(),
            WM_LBUTTONDOWN => {
                let (x, y) = mouse_pos(lparam);
                self.pressed = self.button_at(x, y);
            }
            WM_LBUTTONUP => {
                let (x, y) = mouse_pos(lparam);
                let pressed = self.pressed.take();
                if let Some(index) = self.button_at(x, y)
                    && pressed == Some(index)
                {
                    self.tasks.click(index);
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
        if self.options.hide_taskbar {
            taskbar::hide();
        }
        appbar::register(self.hwnd);
        self.reposition();
        tasks::install_hooks(self.hwnd);
        unsafe {
            let _ = RegisterShellHookWindow(self.hwnd);
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
        self.refresh_tasks();
        self.tick_clock();
    }

    fn on_explorer_restarted(&mut self) {
        // Explorer forgets appbars when it restarts.
        appbar::unregister(self.hwnd);
        appbar::register(self.hwnd);
        if self.options.hide_taskbar {
            taskbar::rehide();
        }
        self.reposition();
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

    /// Docks the bar to the bottom of the primary monitor at the current DPI.
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
            let height = scale(layout::BAR_HEIGHT, self.dpi);
            let rc = appbar::set_pos(self.hwnd, info.rcMonitor, height);
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
        let icon_px = scale(layout::ICON_SIZE, self.dpi) as u32;
        if icon_px != self.icon_px {
            self.icon_px = icon_px;
            self.tasks.reload_icons(&self.icons, icon_px);
        }
        self.invalidate();
    }

    fn refresh_tasks(&mut self) {
        if self.tasks.refresh(&self.icons, self.icon_px) {
            self.invalidate();
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

    fn bar_width_dip(&self) -> f32 {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &raw mut rc);
        }
        (rc.right - rc.left) as f32 * 96.0 / self.dpi as f32
    }

    fn button_rects(&self) -> Vec<D2D_RECT_F> {
        layout::center_buttons(self.bar_width_dip(), self.tasks.groups.len())
    }

    /// Button index at a client-area pixel position.
    fn button_at(&self, x: i32, y: i32) -> Option<usize> {
        let scale = 96.0 / self.dpi as f32;
        layout::hit_test(&self.button_rects(), x as f32 * scale, y as f32 * scale)
    }

    fn scene(tasks: &TaskList, rects: Vec<D2D_RECT_F>) -> Scene<'_> {
        let foreground = tasks.foreground;
        let buttons = rects
            .into_iter()
            .zip(&tasks.groups)
            .map(|(rect, group)| ButtonView {
                rect,
                icon: group.icon.as_ref(),
                state: if group.contains(foreground) {
                    ButtonState::Active
                } else if group.attention {
                    ButtonState::Attention
                } else {
                    ButtonState::Running
                },
            })
            .collect();
        Scene {
            buttons,
            clock: clock::now(),
        }
    }

    fn paint(&mut self) {
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            BeginPaint(self.hwnd, &raw mut ps);
            let scene = Self::scene(&self.tasks, self.button_rects());
            if let Err(err) = self.renderer.draw(self.hwnd, self.dpi, &scene) {
                debug_log!("nitrobar: draw failed: {err}");
            }
            let _ = EndPaint(self.hwnd, &raw const ps);
        }
    }

    fn cleanup(&mut self) {
        if self.cleaned_up {
            return;
        }
        self.cleaned_up = true;
        tasks::remove_hooks();
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_CLOCK);
            let _ = DeregisterShellHookWindow(self.hwnd);
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
