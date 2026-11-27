//! Hiding and restoring the native Explorer taskbar.
//!
//! Hiding = switch the taskbar to auto-hide (so Explorer stops reserving work
//! area) + `ShowWindow(SW_HIDE)` on every taskbar window. The original
//! auto-hide state is kept in memory and in a small state file, so it can be
//! put back even after a hard kill (see `recover_after_crash`).
//!
//! Everything here is callable from any thread (panic hook, console handler).

use std::ffi::c_void;
use std::path::PathBuf;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::Shell::{
    ABM_GETSTATE, ABM_SETSTATE, ABS_ALWAYSONTOP, ABS_AUTOHIDE, APPBARDATA, SHAppBarMessage,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EVENT_OBJECT_SHOW, FindWindowExW, GetClassNameW, GetForegroundWindow, GetWindowThreadProcessId,
    IsWindowVisible, OBJID_WINDOW, SW_HIDE, SW_SHOW, ShowWindow, WINEVENT_OUTOFCONTEXT,
};
use windows::core::{PCWSTR, w};

const PRIMARY_CLASS: PCWSTR = w!("Shell_TrayWnd");
const SECONDARY_CLASS: PCWSTR = w!("Shell_SecondaryTrayWnd");

static HIDDEN: AtomicBool = AtomicBool::new(false);
static ORIGINAL_STATE: AtomicU32 = AtomicU32::new(ABS_ALWAYSONTOP);
static SHOW_HOOK: AtomicPtr<c_void> = AtomicPtr::new(null_mut());

fn primary_taskbar() -> Option<HWND> {
    // Not `FindWindowW`: our tray stand-in has the same class name.
    crate::tray::real_taskbar()
}

/// Calls `f` for the primary taskbar and every secondary-monitor taskbar.
fn for_each_taskbar(mut f: impl FnMut(HWND)) {
    if let Some(primary) = primary_taskbar() {
        f(primary);
    }
    let mut after = None;
    while let Ok(hwnd) = unsafe { FindWindowExW(None, after, SECONDARY_CLASS, PCWSTR::null()) } {
        f(hwnd);
        after = Some(hwnd);
    }
}

fn set_visible(visible: bool) {
    let cmd = if visible { SW_SHOW } else { SW_HIDE };
    for_each_taskbar(|hwnd| unsafe {
        let _ = ShowWindow(hwnd, cmd);
    });
}

fn appbar_data() -> APPBARDATA {
    APPBARDATA {
        cbSize: size_of::<APPBARDATA>() as u32,
        hWnd: primary_taskbar().unwrap_or_default(),
        ..Default::default()
    }
}

fn autohide_state() -> u32 {
    let mut d = appbar_data();
    unsafe { SHAppBarMessage(ABM_GETSTATE, &mut d) as u32 }
}

fn set_autohide_state(state: u32) {
    let mut d = appbar_data();
    d.lParam = LPARAM(state as isize);
    unsafe {
        SHAppBarMessage(ABM_SETSTATE, &mut d);
    }
}

fn state_file() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(base).join("nitrobar").join("taskbar-state"))
}

fn save_state_file(state: u32) {
    if let Some(path) = state_file() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, state.to_string());
    }
}

fn remove_state_file() {
    if let Some(path) = state_file() {
        let _ = std::fs::remove_file(path);
    }
}

/// Hides the native taskbar. Call from the UI thread (installs a WinEvent hook).
pub fn hide() {
    if !HIDDEN.swap(true, Ordering::SeqCst) {
        let original = autohide_state();
        ORIGINAL_STATE.store(original, Ordering::SeqCst);
        save_state_file(original);
        set_autohide_state(original | ABS_AUTOHIDE);
    }
    set_visible(false);
    install_show_hook();
}

/// Re-applies hiding after Explorer restarted (new windows, new process id).
pub fn rehide() {
    if HIDDEN.load(Ordering::SeqCst) {
        set_autohide_state(ORIGINAL_STATE.load(Ordering::SeqCst) | ABS_AUTOHIDE);
        set_visible(false);
        install_show_hook();
    }
}

/// Brings the native taskbar back if we hid it. Idempotent, any thread.
pub fn restore() {
    if HIDDEN.swap(false, Ordering::SeqCst) {
        remove_show_hook();
        set_autohide_state(ORIGINAL_STATE.load(Ordering::SeqCst));
        set_visible(true);
        remove_state_file();
    }
}

/// If a previous run died without restoring, put the saved state back.
pub fn recover_after_crash() -> bool {
    let Some(path) = state_file() else {
        return false;
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return false;
    };
    if let Ok(state) = text.trim().parse::<u32>() {
        set_autohide_state(state);
    }
    set_visible(true);
    let _ = std::fs::remove_file(path);
    true
}

/// `--restore-taskbar`: recover saved state if any, and show the taskbar regardless.
pub fn force_restore() {
    recover_after_crash();
    set_visible(true);
}

/// Explorer re-shows its taskbar on some events (e.g. opening Start); hide it again.
fn install_show_hook() {
    remove_show_hook();
    let Some(primary) = primary_taskbar() else {
        return;
    };
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(primary, Some(&mut pid)) };
    let hook = unsafe {
        SetWinEventHook(
            EVENT_OBJECT_SHOW,
            EVENT_OBJECT_SHOW,
            None,
            Some(on_object_show),
            pid,
            0,
            WINEVENT_OUTOFCONTEXT,
        )
    };
    SHOW_HOOK.store(hook.0, Ordering::SeqCst);
}

fn remove_show_hook() {
    let hook = SHOW_HOOK.swap(null_mut(), Ordering::SeqCst);
    if !hook.is_null() {
        // Fails harmlessly if called off the installing thread (e.g. from a panic hook);
        // the callback checks HIDDEN anyway.
        unsafe {
            let _ = UnhookWinEvent(HWINEVENTHOOK(hook));
        }
    }
}

fn is_taskbar_window(hwnd: HWND) -> bool {
    let mut buf = [0u16; 32];
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    let name = &buf[..usize::try_from(len).unwrap_or(0)];
    [PRIMARY_CLASS, SECONDARY_CLASS]
        .iter()
        .any(|class| unsafe { class.as_wide() } == name)
}

unsafe extern "system" fn on_object_show(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    if id_object == OBJID_WINDOW.0 && HIDDEN.load(Ordering::SeqCst) && is_taskbar_window(hwnd) {
        // Start / search / Quick Settings are anchored to the taskbar: hiding it
        // while one is open closes it instantly. Wait until it closes
        // (`foreground_changed`).
        if crate::island::is_shell_flyout(unsafe { GetForegroundWindow() }) {
            return;
        }
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

/// Called on every foreground change: once no Windows flyout is open any more,
/// hide the taskbar again if Explorer showed it meanwhile.
pub fn foreground_changed(flyout_open: bool) {
    if HIDDEN.load(Ordering::SeqCst) && !flyout_open {
        for_each_taskbar(|hwnd| unsafe {
            if IsWindowVisible(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
        });
    }
}
