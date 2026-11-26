//! Right-click context menu.
//!
//! `TrackPopupMenu` runs a nested message loop, so it must never run while `App`
//! is borrowed: the caller gathers what it needs, releases the borrow, shows the
//! menu, then re-borrows to act on the choice.

use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, MF_SEPARATOR, MF_STRING, PostMessageW,
    SetForegroundWindow, TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_NONOTIFY, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, TrackPopupMenu, WM_NULL,
};
use windows::core::{PCWSTR, w};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Command {
    CloseWindows,
    Exit,
}

impl Command {
    fn id(self) -> usize {
        match self {
            Command::CloseWindows => 1,
            Command::Exit => 2,
        }
    }

    fn label(self, window_count: usize) -> PCWSTR {
        match self {
            Command::CloseWindows if window_count > 1 => w!("Đóng tất cả cửa sổ"),
            Command::CloseWindows => w!("Đóng cửa sổ"),
            Command::Exit => w!("Thoát nitrobar"),
        }
    }
}

/// Screen position for a WM_CONTEXTMENU lParam (-1,-1 means "from keyboard").
pub fn menu_point(lparam: LPARAM) -> POINT {
    let x = i32::from(lparam.0 as u16 as i16);
    let y = i32::from((lparam.0 >> 16) as u16 as i16);
    if x == -1 && y == -1 {
        let mut pt = POINT::default();
        unsafe {
            let _ = GetCursorPos(&raw mut pt);
        }
        pt
    } else {
        POINT { x, y }
    }
}

/// Shows the menu and returns the chosen command. `window_count` is the number of
/// windows of the right-clicked app (0 when clicking empty bar space).
pub fn show(owner: HWND, at: POINT, window_count: usize) -> Option<Command> {
    let mut items = Vec::new();
    if window_count > 0 {
        items.push(Some(Command::CloseWindows));
        items.push(None);
    }
    items.push(Some(Command::Exit));

    unsafe {
        let menu = CreatePopupMenu().ok()?;
        for item in &items {
            let _ = match item {
                Some(cmd) => AppendMenuW(menu, MF_STRING, cmd.id(), cmd.label(window_count)),
                None => AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null()),
            };
        }
        // Required so the menu closes when clicking elsewhere (see TrackPopupMenu docs).
        let _ = SetForegroundWindow(owner);
        let chosen = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | TPM_LEFTALIGN,
            at.x,
            at.y,
            None,
            owner,
            None,
        );
        let _ = PostMessageW(Some(owner), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        let id = usize::try_from(chosen.0).ok()?;
        items.into_iter().flatten().find(|cmd| cmd.id() == id)
    }
}
