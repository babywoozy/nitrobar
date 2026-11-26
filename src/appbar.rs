//! Thin wrapper over `SHAppBarMessage` so the shell reserves screen space for the bar.

use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::UI::Shell::{
    ABE_BOTTOM, ABE_TOP, ABM_ACTIVATE, ABM_NEW, ABM_QUERYPOS, ABM_REMOVE, ABM_SETPOS,
    ABM_WINDOWPOSCHANGED, APPBARDATA, SHAppBarMessage,
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

/// Docks the bar to the top or bottom of `monitor`; returns the rectangle the shell granted.
pub fn set_pos(hwnd: HWND, monitor: RECT, height: i32, top: bool) -> RECT {
    let mut d = data(hwnd);
    d.uEdge = if top { ABE_TOP } else { ABE_BOTTOM };
    d.rc = if top {
        RECT {
            bottom: monitor.top + height,
            ..monitor
        }
    } else {
        RECT {
            top: monitor.bottom - height,
            ..monitor
        }
    };
    unsafe {
        SHAppBarMessage(ABM_QUERYPOS, &raw mut d);
        // The shell may move our edge (another appbar there); keep our height.
        if top {
            d.rc.bottom = d.rc.top + height;
        } else {
            d.rc.top = d.rc.bottom - height;
        }
        SHAppBarMessage(ABM_SETPOS, &raw mut d);
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
