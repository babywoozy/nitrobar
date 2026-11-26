//! Thin wrapper over `SHAppBarMessage` so the shell reserves screen space for the bar.

use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::UI::Shell::{
    ABE_BOTTOM, ABM_ACTIVATE, ABM_NEW, ABM_QUERYPOS, ABM_REMOVE, ABM_SETPOS, ABM_WINDOWPOSCHANGED,
    APPBARDATA, SHAppBarMessage,
};
use windows::Win32::UI::WindowsAndMessaging::WM_APP;

/// Message the shell sends us for appbar notifications (`ABN_*` in wParam).
pub const CALLBACK_MSG: u32 = WM_APP + 1;

fn data(hwnd: HWND) -> APPBARDATA {
    APPBARDATA {
        cbSize: size_of::<APPBARDATA>() as u32,
        hWnd: hwnd,
        ..Default::default()
    }
}

pub fn register(hwnd: HWND) -> bool {
    let mut d = data(hwnd);
    d.uCallbackMessage = CALLBACK_MSG;
    unsafe { SHAppBarMessage(ABM_NEW, &mut d) != 0 }
}

/// Safe to call more than once and from any thread.
pub fn unregister(hwnd: HWND) {
    let mut d = data(hwnd);
    unsafe {
        SHAppBarMessage(ABM_REMOVE, &mut d);
    }
}

/// Docks the bar to the bottom of `monitor` and returns the rectangle the shell granted.
pub fn set_pos(hwnd: HWND, monitor: RECT, height: i32) -> RECT {
    let mut d = data(hwnd);
    d.uEdge = ABE_BOTTOM;
    d.rc = RECT {
        top: monitor.bottom - height,
        ..monitor
    };
    unsafe {
        SHAppBarMessage(ABM_QUERYPOS, &mut d);
        // The shell may move `bottom` up (another appbar on this edge); keep our height.
        d.rc.top = d.rc.bottom - height;
        SHAppBarMessage(ABM_SETPOS, &mut d);
    }
    d.rc
}

pub fn activate(hwnd: HWND) {
    let mut d = data(hwnd);
    d.lParam = LPARAM(1);
    unsafe {
        SHAppBarMessage(ABM_ACTIVATE, &mut d);
    }
}

pub fn window_pos_changed(hwnd: HWND) {
    let mut d = data(hwnd);
    unsafe {
        SHAppBarMessage(ABM_WINDOWPOSCHANGED, &mut d);
    }
}
