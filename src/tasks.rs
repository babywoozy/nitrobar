//! Running application windows, grouped per app, kept current by events (no polling).
//!
//! WinEvent hooks and the shell hook only *request* a refresh: requests are
//! coalesced into a single posted `WM_APP_REFRESH`, which re-enumerates
//! top-level windows (cheap) and diffs against the current list.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;

use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
use windows::Win32::Storage::Packaging::Appx::GetApplicationUserModelId;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow};
use windows::Win32::UI::WindowsAndMessaging::{
    CHILDID_SELF, EVENT_OBJECT_CLOAKED, EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE,
    EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_SHOW, EVENT_OBJECT_UNCLOAKED, EVENT_SYSTEM_FOREGROUND,
    EnumWindows, GA_ROOT, GW_OWNER, GWL_EXSTYLE, GetAncestor, GetForegroundWindow, GetWindow,
    GetWindowLongW, GetWindowTextLengthW, GetWindowThreadProcessId, IsHungAppWindow, IsIconic,
    IsWindowVisible, OBJID_WINDOW, PostMessageW, SHOW_WINDOW_CMD, SW_MINIMIZE, SW_RESTORE,
    SetForegroundWindow, ShowWindow, ShowWindowAsync, WINEVENT_OUTOFCONTEXT,
    WINEVENT_SKIPOWNPROCESS, WM_APP, WM_CLOSE, WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};
use windows::core::{BOOL, PWSTR};

use crate::icons::{Icon, IconLoader};

/// Posted to the bar when the window list should be re-read.
pub const WM_APP_REFRESH: u32 = WM_APP + 3;

/// What identifies an application, used for grouping, icons and (later) pinning.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct AppId {
    /// Explicit AppUserModelID (packaged apps and many desktop apps).
    pub aumid: Option<String>,
    /// Full path of the process executable.
    pub exe: Option<String>,
}

impl AppId {
    /// Grouping key: AUMID when present, otherwise the lower-cased executable path.
    pub fn key(&self) -> String {
        match (&self.aumid, &self.exe) {
            (Some(aumid), _) => format!("aumid:{}", aumid.to_lowercase()),
            (None, Some(exe)) => format!("exe:{}", exe.to_lowercase()),
            (None, None) => String::from("unknown"),
        }
    }

    /// Whether a pin and a running app are the same application: equal AUMIDs when
    /// both have one (Chrome profiles share chrome.exe but not their AUMID), else
    /// the same executable.
    pub fn same_app(&self, other: &AppId) -> bool {
        match (&self.aumid, &other.aumid) {
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
            _ => matches!((&self.exe, &other.exe), (Some(a), Some(b)) if a.eq_ignore_ascii_case(b)),
        }
    }
}

/// AUMID of File Explorer windows (they don't report one; the native pin uses it).
const EXPLORER_AUMID: &str = "Microsoft.Windows.Explorer";

pub struct AppGroup {
    pub key: String,
    pub app: AppId,
    pub windows: Vec<HWND>,
    pub icon: Option<Icon>,
    icon_attempts: u8,
    pub last_active: HWND,
    pub attention: bool,
}

impl AppGroup {
    pub fn contains(&self, hwnd: HWND) -> bool {
        self.windows.contains(&hwnd)
    }
}

pub struct TaskList {
    pub groups: Vec<AppGroup>,
    pub foreground: HWND,
    own: HWND,
    ids: HashMap<isize, AppId>,
}

const MAX_ICON_ATTEMPTS: u8 = 3;

thread_local! {
    static BAR: Cell<HWND> = const { Cell::new(HWND(std::ptr::null_mut())) };
    static REFRESH_PENDING: Cell<bool> = const { Cell::new(false) };
    /// Windows currently shown, so hook callbacks can ignore everything else cheaply.
    static TRACKED: RefCell<HashSet<isize>> = RefCell::new(HashSet::new());
    static HOOKS: RefCell<Vec<HWINEVENTHOOK>> = const { RefCell::new(Vec::new()) };
}

/// Coalesces refresh requests into one posted message.
pub fn request_refresh() {
    if !REFRESH_PENDING.replace(true) {
        let bar = BAR.get();
        unsafe {
            let _ = PostMessageW(Some(bar), WM_APP_REFRESH, WPARAM(0), LPARAM(0));
        }
    }
}

fn is_tracked(hwnd: HWND) -> bool {
    TRACKED.with_borrow(|set| set.contains(&(hwnd.0 as isize)))
}

unsafe extern "system" fn on_win_event(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    if id_object != OBJID_WINDOW.0 || id_child != CHILDID_SELF as i32 || hwnd.is_invalid() {
        return;
    }
    let relevant = match event {
        EVENT_SYSTEM_FOREGROUND => true,
        EVENT_OBJECT_DESTROY | EVENT_OBJECT_HIDE | EVENT_OBJECT_CLOAKED => is_tracked(hwnd),
        // A shown / renamed window only matters if it is top-level and not yet listed.
        EVENT_OBJECT_SHOW | EVENT_OBJECT_UNCLOAKED | EVENT_OBJECT_NAMECHANGE => {
            !is_tracked(hwnd) && unsafe { GetAncestor(hwnd, GA_ROOT) } == hwnd
        }
        _ => false,
    };
    if relevant {
        request_refresh();
    }
}

/// Installs narrow WinEvent hooks (no LOCATIONCHANGE etc., which fire constantly).
pub fn install_hooks(bar: HWND) {
    BAR.set(bar);
    let ranges = [
        (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
        (EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE),
        (EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_NAMECHANGE),
        (EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED),
    ];
    HOOKS.with_borrow_mut(|hooks| {
        for (min, max) in ranges {
            let hook = unsafe {
                SetWinEventHook(
                    min,
                    max,
                    None,
                    Some(on_win_event),
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
                )
            };
            if !hook.is_invalid() {
                hooks.push(hook);
            }
        }
    });
}

pub fn remove_hooks() {
    HOOKS.with_borrow_mut(|hooks| {
        for hook in hooks.drain(..) {
            unsafe {
                let _ = UnhookWinEvent(hook);
            }
        }
    });
}

/// Same rules Explorer uses for "does this window get a taskbar button".
fn is_task_window(hwnd: HWND) -> bool {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            return false;
        }
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        let app_window = ex & WS_EX_APPWINDOW.0 != 0;
        if !app_window {
            if ex & (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0) != 0 {
                return false;
            }
            if GetWindow(hwnd, GW_OWNER).is_ok_and(|owner| !owner.is_invalid()) {
                return false;
            }
        }
        // Cloaked = on another virtual desktop, or a suspended UWP frame.
        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&raw mut cloaked).cast::<c_void>(),
            size_of::<u32>() as u32,
        );
        if cloaked != 0 {
            return false;
        }
        GetWindowTextLengthW(hwnd) > 0
    }
}

unsafe extern "system" fn collect_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let list = unsafe { &mut *(lparam.0 as *mut Vec<HWND>) };
    if is_task_window(hwnd) {
        list.push(hwnd);
    }
    true.into()
}

fn window_aumid(hwnd: HWND) -> Option<String> {
    unsafe {
        let store: IPropertyStore = SHGetPropertyStoreForWindow(hwnd).ok()?;
        let value = store.GetValue(&PKEY_AppUserModel_ID).ok()?;
        let text = PropVariantToStringAlloc(&value).ok()?;
        let result = text.to_string().ok();
        CoTaskMemFree(Some(text.0 as *const c_void));
        result.filter(|s| !s.is_empty())
    }
}

/// Executable path of the window's process, and the process's package AUMID if it
/// is a packaged (MSIX / Store) app. Packaged desktop apps (e.g. Electron apps from
/// the Store) often set no AUMID on their windows; Windows then groups them by the
/// package identity, which is also what their pins refer to.
/// Full path of the executable that owns `hwnd`.
pub fn process_exe(hwnd: HWND) -> Option<String> {
    window_process(hwnd).0
}

fn window_process(hwnd: HWND) -> (Option<String>, Option<String>) {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&raw mut pid));
        let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return (None, None);
        };
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let exe = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &raw mut len,
        )
        .ok()
        .map(|()| String::from_utf16_lossy(&buf[..len as usize]));
        // APPMODEL_ERROR_NO_APPLICATION for ordinary (unpackaged) processes.
        let mut len = buf.len() as u32;
        let package =
            GetApplicationUserModelId(process, &raw mut len, Some(PWSTR(buf.as_mut_ptr())))
                .is_ok()
                .then(|| String::from_utf16_lossy(&buf[..(len as usize).saturating_sub(1)]))
                .filter(|s| !s.is_empty());
        let _ = CloseHandle(process);
        (exe, package)
    }
}

pub fn app_id_for(hwnd: HWND) -> AppId {
    let (exe, package) = window_process(hwnd);
    let aumid = window_aumid(hwnd).or(package).or_else(|| {
        exe.as_deref()
            .filter(|e| e.to_ascii_lowercase().ends_with("\\explorer.exe"))
            .map(|_| EXPLORER_AUMID.to_string())
    });
    AppId { aumid, exe }
}

/// Minimizes / restores `hwnd`. `ShowWindowAsync` (a posted message) is ignored by
/// some apps: VS Code (Chromium) never minimized or restored from it, so a
/// minimized VS Code could not be reopened from the bar. A plain `ShowWindow`
/// works, but would block on an app that is not responding: those get the async one.
fn show(hwnd: HWND, command: SHOW_WINDOW_CMD) {
    unsafe {
        if IsHungAppWindow(hwnd).as_bool() {
            let _ = ShowWindowAsync(hwnd, command);
        } else {
            let _ = ShowWindow(hwnd, command);
        }
    }
}

pub fn activate(hwnd: HWND) {
    unsafe {
        if IsIconic(hwnd).as_bool() {
            show(hwnd, SW_RESTORE);
        }
        let _ = SetForegroundWindow(hwnd);
    }
}

fn minimize(hwnd: HWND) {
    show(hwnd, SW_MINIMIZE);
}

impl TaskList {
    pub fn new(own: HWND) -> Self {
        Self {
            groups: Vec::new(),
            foreground: HWND::default(),
            own,
            ids: HashMap::new(),
        }
    }

    /// Re-reads the window list. Returns true if anything visible changed.
    pub fn refresh(&mut self, icons: &IconLoader, icon_px: u32) -> bool {
        REFRESH_PENDING.set(false);
        let before = self.signature();

        let mut found: Vec<HWND> = Vec::with_capacity(64);
        unsafe {
            let _ = EnumWindows(Some(collect_window), LPARAM((&raw mut found) as isize));
        }
        found.retain(|&h| h != self.own);
        // EnumWindows is top of Z-order first; add oldest-looking windows first.
        found.reverse();

        let alive: HashSet<isize> = found.iter().map(|h| h.0 as isize).collect();
        self.ids.retain(|k, _| alive.contains(k));
        for group in &mut self.groups {
            group.windows.retain(|h| alive.contains(&(h.0 as isize)));
        }

        for &hwnd in &found {
            if self.groups.iter().any(|g| g.contains(hwnd)) {
                continue;
            }
            let app = self
                .ids
                .entry(hwnd.0 as isize)
                .or_insert_with(|| app_id_for(hwnd))
                .clone();
            let key = app.key();
            match self.groups.iter_mut().find(|g| g.key == key) {
                Some(group) => group.windows.push(hwnd),
                None => self.groups.push(AppGroup {
                    key,
                    app,
                    windows: vec![hwnd],
                    icon: None,
                    icon_attempts: 0,
                    last_active: hwnd,
                    attention: false,
                }),
            }
        }
        self.groups.retain(|g| !g.windows.is_empty());

        self.foreground = unsafe { GetForegroundWindow() };
        let foreground = self.foreground;
        for group in &mut self.groups {
            if group.contains(foreground) {
                group.last_active = foreground;
                group.attention = false;
            }
            if group.icon.is_none() && group.icon_attempts < MAX_ICON_ATTEMPTS {
                group.icon_attempts += 1;
                group.icon = icons.load(group.windows[0], &group.app, icon_px);
            }
        }

        TRACKED.with_borrow_mut(|set| {
            set.clear();
            set.extend(alive);
        });
        before != self.signature()
    }

    /// Reloads every icon, e.g. after a DPI change.
    pub fn reload_icons(&mut self, icons: &IconLoader, icon_px: u32) {
        for group in &mut self.groups {
            group.icon = icons.load(group.windows[0], &group.app, icon_px);
            group.icon_attempts = 1;
        }
    }

    /// Cheap fingerprint of everything that affects drawing.
    fn signature(&self) -> Vec<isize> {
        let mut sig = vec![self.foreground.0 as isize];
        for group in &self.groups {
            sig.push(-1);
            sig.push(group.icon.as_ref().map_or(0, |i| i.id as isize));
            sig.push(isize::from(group.attention));
            sig.extend(group.windows.iter().map(|h| h.0 as isize));
        }
        sig
    }

    pub fn group_index(&self, key: &str) -> Option<usize> {
        self.groups.iter().position(|g| g.key == key)
    }

    /// The window a click on group `index` would bring up (`None`: it would minimize).
    pub fn click_target(&self, index: usize) -> Option<HWND> {
        let group = self.groups.get(index)?;
        let foreground = unsafe { GetForegroundWindow() };
        match group.windows.iter().position(|&h| h == foreground) {
            Some(_) if group.windows.len() == 1 => None,
            Some(pos) => Some(group.windows[(pos + 1) % group.windows.len()]),
            None if group.contains(group.last_active) => Some(group.last_active),
            None => group.windows.first().copied(),
        }
    }

    /// Taskbar click: focus the app, minimize it if already focused, cycle if it has several windows.
    pub fn click(&mut self, index: usize) {
        let Some(group) = self.groups.get(index) else {
            return;
        };
        let foreground = unsafe { GetForegroundWindow() };
        if let Some(pos) = group.windows.iter().position(|&h| h == foreground) {
            if group.windows.len() == 1 {
                minimize(foreground);
            } else {
                activate(group.windows[(pos + 1) % group.windows.len()]);
            }
        } else if group.contains(group.last_active) {
            activate(group.last_active);
        } else {
            activate(group.windows[0]);
        }
    }

    /// HSHELL_FLASH: a window asks for attention.
    pub fn flash(&mut self, hwnd: HWND) -> bool {
        if hwnd == self.foreground {
            return false;
        }
        match self.groups.iter_mut().find(|g| g.contains(hwnd)) {
            Some(group) if !group.attention => {
                group.attention = true;
                true
            }
            _ => false,
        }
    }

    pub fn close_group(&self, index: usize) {
        if let Some(group) = self.groups.get(index) {
            for &hwnd in &group.windows {
                unsafe {
                    let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
            }
        }
    }
}
