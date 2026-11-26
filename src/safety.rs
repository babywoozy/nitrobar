//! Last-resort cleanup: whatever happens, the native taskbar must come back and
//! our appbar reservation must be released.
//!
//! Covered: normal exit, panics (hook runs before abort), unhandled SEH
//! exceptions, Ctrl+C / console close, logoff and shutdown (WM_ENDSESSION in app.rs).
//! Not coverable: TerminateProcess (Task Manager "End task"). For that,
//! `taskbar::recover_after_crash` runs on the next start, and `--restore-taskbar`
//! exists as a manual tool.

use std::ffi::c_void;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicPtr, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Console::SetConsoleCtrlHandler;
use windows::Win32::System::Diagnostics::Debug::{EXCEPTION_POINTERS, SetUnhandledExceptionFilter};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};
use windows::core::BOOL;

use crate::{appbar, taskbar};

/// The bar window, readable from any thread (panic hook, console handler).
static BAR_HWND: AtomicPtr<c_void> = AtomicPtr::new(null_mut());

const EXCEPTION_CONTINUE_SEARCH: i32 = 0;

pub fn set_bar_window(hwnd: HWND) {
    BAR_HWND.store(hwnd.0, Ordering::SeqCst);
}

/// Releases everything that outlives the process. Idempotent and thread-safe.
pub fn emergency_cleanup() {
    let hwnd = BAR_HWND.swap(null_mut(), Ordering::SeqCst);
    if !hwnd.is_null() {
        appbar::unregister(HWND(hwnd));
    }
    taskbar::restore();
}

pub fn install() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        emergency_cleanup();
        default_hook(info);
    }));
    unsafe {
        let _ = SetConsoleCtrlHandler(Some(console_handler), true);
        SetUnhandledExceptionFilter(Some(exception_filter));
    }
}

unsafe extern "system" fn console_handler(_ctrl_type: u32) -> BOOL {
    let hwnd = BAR_HWND.load(Ordering::SeqCst);
    // Clean up right away: for CTRL_CLOSE/LOGOFF/SHUTDOWN the process dies when we return.
    emergency_cleanup();
    if !hwnd.is_null() {
        unsafe {
            let _ = PostMessageW(Some(HWND(hwnd)), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
    true.into()
}

unsafe extern "system" fn exception_filter(_info: *const EXCEPTION_POINTERS) -> i32 {
    emergency_cleanup();
    EXCEPTION_CONTINUE_SEARCH
}
