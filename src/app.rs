//! The bar window: creation, message loop and window procedure.
//!
//! State lives in a `RefCell<App>` behind GWLP_USERDATA. Win32 re-enters the
//! window procedure synchronously (e.g. SetWindowPos sends WM_WINDOWPOSCHANGED),
//! so messages that arrive while `App` is borrowed fall back to stateless
//! handling / DefWindowProc instead of aliasing `&mut App`.

use std::cell::RefCell;

use windows::Win32::Foundation::{GetLastError, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetMonitorInfoW, InvalidateRect, MONITOR_DEFAULTTOPRIMARY, MONITORINFO,
    MonitorFromPoint, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Shell::{ABN_FULLSCREENAPP, ABN_POSCHANGED};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, FindWindowW, GWLP_USERDATA, GetMessageW,
    GetWindowLongPtrW, HWND_BOTTOM, HWND_TOPMOST, IDC_ARROW, KillTimer, LoadCursorW, MSG,
    PostMessageW, PostQuitMessage, RegisterClassExW, RegisterWindowMessageW, SW_SHOWNOACTIVATE,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SendMessageW, SetTimer, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, TranslateMessage, WM_ACTIVATE, WM_APP, WM_CLOSE, WM_DESTROY,
    WM_DISPLAYCHANGE, WM_DPICHANGED, WM_ENDSESSION, WM_ERASEBKGND, WM_NCDESTROY, WM_PAINT,
    WM_QUERYENDSESSION, WM_SETTINGCHANGE, WM_TIMECHANGE, WM_TIMER, WM_WINDOWPOSCHANGED,
    WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{PCWSTR, Result, w};

use crate::render::Renderer;
use crate::{appbar, clock, safety, taskbar};

const CLASS_NAME: PCWSTR = w!("NitrobarWindow");
/// Same height as the Windows 11 taskbar.
const BAR_HEIGHT_DIP: i32 = 48;
const TIMER_CLOCK: usize = 1;
/// Posted once after creation to run setup inside the window procedure.
const WM_APP_INIT: u32 = WM_APP + 2;

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
    renderer: Renderer,
}

fn scale(dip: i32, dpi: u32) -> i32 {
    (dip * dpi as i32 + 48) / 96
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
    let renderer = Renderer::new()?;
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
        if RegisterClassExW(&class) == 0 {
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
            renderer,
        };
        let cell = Box::into_raw(Box::new(RefCell::new(app)));
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, cell as isize);
        SendMessageW(hwnd, WM_APP_INIT, None, None);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_ERASEBKGND => return LRESULT(1),
            // These only need the HWND, so they also work while App is borrowed.
            WM_ACTIVATE => appbar::activate(hwnd),
            WM_WINDOWPOSCHANGED => appbar::window_pos_changed(hwnd),
            WM_NCDESTROY => {
                let ptr = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) as *mut RefCell<App>;
                if !ptr.is_null() {
                    drop(Box::from_raw(ptr));
                }
            }
            _ => {
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<App>;
                if let Some(cell) = ptr.as_ref()
                    && let Ok(mut app) = cell.try_borrow_mut()
                    && let Some(result) = app.handle(msg, wparam, lparam)
                {
                    return result;
                }
            }
        }
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }
}

impl App {
    /// Returns `None` to let DefWindowProc handle the message.
    fn handle(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        if msg != 0 && msg == self.msg_taskbar_created {
            self.on_explorer_restarted();
            return Some(LRESULT(0));
        }
        match msg {
            WM_APP_INIT => self.init(),
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
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
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
            if !GetMonitorInfoW(monitor, &mut info).as_bool() {
                return;
            }
            let (mut dpi_x, mut dpi_y) = (96, 96);
            if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y).is_ok() {
                self.dpi = dpi_x;
            }
            let height = scale(BAR_HEIGHT_DIP, self.dpi);
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
        self.invalidate();
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

    fn paint(&mut self) {
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            BeginPaint(self.hwnd, &mut ps);
            if let Err(err) = self.renderer.draw(self.hwnd, self.dpi, &clock::now()) {
                debug_log!("nitrobar: draw failed: {err}");
            }
            let _ = EndPaint(self.hwnd, &ps);
        }
    }

    fn cleanup(&mut self) {
        if self.cleaned_up {
            return;
        }
        self.cleaned_up = true;
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_CLOCK);
        }
        safety::emergency_cleanup();
    }
}
