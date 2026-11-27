//! The apps' tray icons, collected the way the taskbar gets them.
//!
//! `Shell_NotifyIcon` sends its data as `WM_COPYDATA` to the first top-level
//! window of class `Shell_TrayWnd`. A thread of our own keeps such a window
//! (never shown) in front of Explorer's, reads what passes through, and forwards
//! every message to the real taskbar unchanged. Explorer therefore stays fully
//! informed: appbars keep working, and when nitrobar goes away (however it does)
//! nothing is lost. After the window is up, `TaskbarCreated` is sent around so
//! apps register their icons again.
//!
//! Undocumented, as every shell replacement does it. Not reachable this way:
//! Windows' own indicators (location, microphone), and apps that run elevated.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread::JoinHandle;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory,
    WICBitmapDitherTypeNone, WICBitmapPaletteTypeCustom,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, ChangeWindowMessageFilterEx, CreateWindowExW, DefWindowProcW,
    DestroyWindow, DispatchMessageW, EVENT_OBJECT_REORDER, EVENT_OBJECT_SHOW,
    EVENT_SYSTEM_FOREGROUND, EnumWindows, FindWindowExW, FindWindowW, GetMessageW, GetWindowRect,
    GetWindowThreadProcessId, HICON, HWND_TOPMOST, InSendMessage, IsWindow, MSG, MSGFLT_ALLOW,
    PostMessageW, PostQuitMessage, RegisterClassExW, RegisterWindowMessageW, SMTO_ABORTIFHUNG,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SendMessageTimeoutW, SetWindowPos, TranslateMessage,
    WINEVENT_OUTOFCONTEXT, WM_APP, WM_CLOSE, WM_COMMAND, WM_CONTEXTMENU, WM_COPYDATA, WM_DESTROY,
    WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_USER,
    WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{BOOL, PCWSTR, w};

/// Posted to the bar when the icons changed.
pub const WM_APP_TRAY: u32 = WM_APP + 13;
/// Posted to the tray window: Explorer restarted.
const WM_REFRESH: u32 = WM_APP + 1;

const CLASS: PCWSTR = w!("Shell_TrayWnd");
/// `COPYDATASTRUCT::dwData` of a `Shell_NotifyIcon` call.
const COPYDATA_NOTIFY: usize = 1;
const FORWARD_TIMEOUT_MS: u32 = 2000;

const NIM_ADD: u32 = 0;
const NIM_MODIFY: u32 = 1;
const NIM_DELETE: u32 = 2;
const NIM_SETVERSION: u32 = 4;
const NIF_MESSAGE: u32 = 0x01;
const NIF_ICON: u32 = 0x02;
const NIF_TIP: u32 = 0x04;
const NIF_STATE: u32 = 0x08;
const NIF_GUID: u32 = 0x20;
const NIS_HIDDEN: u32 = 0x01;
const NIN_SELECT: u32 = WM_USER;

/// `NOTIFYICONDATAW` as it crosses processes: handles are 32 bits wide.
#[repr(C)]
#[derive(Clone, Copy)]
struct WireIcon {
    size: u32,
    hwnd: u32,
    uid: u32,
    flags: u32,
    callback: u32,
    icon: u32,
    tip: [u16; 128],
    state: u32,
    state_mask: u32,
    info: [u16; 256],
    version: u32,
    info_title: [u16; 64],
    info_flags: u32,
    guid: [u8; 16],
}

/// Signature and `NIM_*` code in front of the icon data.
const WIRE_HEADER: usize = 8;

/// A picture in premultiplied BGRA.
#[derive(PartialEq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

#[derive(Clone)]
pub struct TrayIcon {
    hwnd: isize,
    uid: u32,
    guid: Option<[u8; 16]>,
    callback: u32,
    version: u32,
    hidden: bool,
    pub image: Option<Arc<Image>>,
    pub tip: Vec<u16>,
}

impl TrayIcon {
    fn new(wire: &WireIcon) -> Self {
        Self {
            hwnd: handle(wire.hwnd),
            uid: wire.uid,
            guid: None,
            callback: 0,
            version: 0,
            hidden: false,
            image: None,
            tip: Vec::new(),
        }
    }

    /// A made-up icon for the offscreen shots.
    #[cfg(test)]
    pub fn sample(width: u32, height: u32, pixels: Vec<u8>, tip: &str) -> Self {
        // SAFETY: all zeros is a value of this plain struct.
        let wire: WireIcon = unsafe { std::mem::zeroed() };
        Self {
            image: Some(Arc::new(Image {
                width,
                height,
                pixels,
            })),
            tip: tip.encode_utf16().collect(),
            ..Self::new(&wire)
        }
    }

    /// Whether `wire` talks about this icon: by GUID when it carries one.
    fn is(&self, wire: &WireIcon) -> bool {
        if wire.flags & NIF_GUID != 0 && self.guid.is_some() {
            return self.guid == Some(wire.guid);
        }
        self.hwnd == handle(wire.hwnd) && self.uid == wire.uid
    }

    /// Takes over what `wire` provides (`image`: its icon, already converted).
    fn update(&mut self, wire: &WireIcon, image: Option<Arc<Image>>) {
        self.hwnd = handle(wire.hwnd);
        self.uid = wire.uid;
        if wire.flags & NIF_GUID != 0 {
            self.guid = Some(wire.guid);
        }
        if wire.flags & NIF_MESSAGE != 0 {
            self.callback = wire.callback;
        }
        if wire.flags & NIF_ICON != 0 && image.is_some() {
            self.image = image;
        }
        if wire.flags & NIF_TIP != 0 {
            let end = wire.tip.iter().position(|&c| c == 0).unwrap_or(128);
            self.tip = wire.tip[..end].to_vec();
        }
        if wire.flags & NIF_STATE != 0 && wire.state_mask & NIS_HIDDEN != 0 {
            self.hidden = wire.state & NIS_HIDDEN != 0;
        }
    }
}

/// Handles are sign-extended from their 32 bits.
fn handle(value: u32) -> isize {
    value as i32 as isize
}

/// Applies one `Shell_NotifyIcon` call to the list. True if the list changed.
fn apply(
    list: &mut Vec<TrayIcon>,
    message: u32,
    wire: &WireIcon,
    image: Option<Arc<Image>>,
) -> bool {
    let at = list.iter().position(|icon| icon.is(wire));
    match (message, at) {
        (NIM_ADD | NIM_MODIFY, Some(i)) => {
            let before = list[i].clone();
            list[i].update(wire, image);
            let now = &list[i];
            before.hidden != now.hidden
                || before.tip != now.tip
                || before.image.as_deref() != now.image.as_deref()
        }
        // A change to an icon registered before we listened counts as new.
        (NIM_ADD | NIM_MODIFY, None) => {
            let mut icon = TrayIcon::new(wire);
            icon.update(wire, image);
            list.push(icon);
            true
        }
        (NIM_DELETE, Some(i)) => {
            list.remove(i);
            true
        }
        (NIM_SETVERSION, Some(i)) => {
            list[i].version = wire.version;
            false
        }
        _ => false,
    }
}

/// Reads the call from the message bytes (older callers send a shorter struct).
fn parse(data: &[u8]) -> Option<(u32, WireIcon)> {
    if data.len() < WIRE_HEADER + 24 {
        return None;
    }
    let message = u32::from_le_bytes(data[4..8].try_into().ok()?);
    // SAFETY: plain integers and arrays, for which all zeros is a value.
    let mut wire: WireIcon = unsafe { std::mem::zeroed() };
    let len = (data.len() - WIRE_HEADER).min(size_of::<WireIcon>());
    // SAFETY: `len` fits both the source slice and the struct.
    unsafe {
        std::ptr::copy_nonoverlapping(
            data[WIRE_HEADER..].as_ptr(),
            (&raw mut wire).cast::<u8>(),
            len,
        );
    }
    Some((message, wire))
}

static ICONS: Mutex<Vec<TrayIcon>> = Mutex::new(Vec::new());
/// The window to tell about changes, and our tray window (0 = none).
static BAR: AtomicIsize = AtomicIsize::new(0);
static WINDOW: AtomicIsize = AtomicIsize::new(0);
/// A `WM_APP_TRAY` is on its way.
static PENDING: AtomicBool = AtomicBool::new(false);
static THREAD: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

thread_local! {
    static WIC: Cell<Option<&'static IWICImagingFactory>> = const { Cell::new(None) };
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The icons to show: visible ones of windows that still exist.
pub fn icons() -> Vec<TrayIcon> {
    PENDING.store(false, Ordering::SeqCst);
    let mut list = lock(&ICONS);
    list.retain(|i| unsafe { IsWindow(Some(HWND(i.hwnd as *mut _))) }.as_bool());
    list.iter()
        .filter(|i| !i.hidden && i.image.is_some() && !i.guid.is_some_and(is_system))
        .cloned()
        .collect()
}

/// Windows' own volume / network / power icons: `{7820AE7x-23E3-4229-82C1-
/// E41CB67D5B9C}`. The strip shows these states itself.
fn is_system(guid: [u8; 16]) -> bool {
    const TAIL: [u8; 15] = [
        0xAE, 0x20, 0x78, 0xE3, 0x23, 0x29, 0x42, 0x82, 0xC1, 0xE4, 0x1C, 0xB6, 0x7D, 0x5B, 0x9C,
    ];
    guid[0] & 0xF0 == 0x70 && guid[1..] == TAIL
}

/// Explorer's own taskbar, not a stand-in (ours or a running instance's): it is
/// the one with the notification area inside.
pub fn real_taskbar() -> Option<HWND> {
    let mut after = None;
    while let Ok(hwnd) = unsafe { FindWindowExW(None, after, CLASS, PCWSTR::null()) } {
        let area = w!("TrayNotifyWnd");
        if unsafe { FindWindowExW(Some(hwnd), None, area, PCWSTR::null()) }.is_ok() {
            return Some(hwnd);
        }
        after = Some(hwnd);
    }
    None
}

/// Starts listening (once). Changes are posted to `bar`.
pub fn start(bar: HWND) {
    BAR.store(bar.0 as isize, Ordering::SeqCst);
    let mut thread = lock(&THREAD);
    if thread.is_some() {
        return;
    }
    let (ready, wait) = mpsc::channel();
    *thread = std::thread::Builder::new()
        .name("nitrobar-tray".into())
        .spawn(move || run(&ready))
        .ok();
    let _ = wait.recv();
}

pub fn stop() {
    let Some(thread) = lock(&THREAD).take() else {
        return;
    };
    post_to_window(WM_CLOSE);
    let _ = thread.join();
    lock(&ICONS).clear();
}

/// Explorer restarted: get in front of the new taskbar and ask for the icons again.
pub fn refresh() {
    post_to_window(WM_REFRESH);
}

fn post_to_window(msg: u32) {
    let hwnd = WINDOW.load(Ordering::SeqCst);
    if hwnd != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(hwnd as *mut _)), msg, WPARAM(0), LPARAM(0));
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Click {
    Left,
    Double,
    Right,
}

/// Tells the icon's app it was clicked, at screen position `at` (its menu opens there).
pub fn click(icon: &TrayIcon, click: Click, at: POINT) {
    let hwnd = HWND(icon.hwnd as *mut _);
    if icon.callback == 0 {
        return;
    }
    let messages: &[u32] = match click {
        Click::Left if icon.version >= 3 => &[WM_LBUTTONDOWN, WM_LBUTTONUP, NIN_SELECT],
        Click::Left => &[WM_LBUTTONDOWN, WM_LBUTTONUP],
        Click::Double => &[WM_LBUTTONDBLCLK, WM_LBUTTONUP],
        Click::Right if icon.version >= 3 => &[WM_RBUTTONDOWN, WM_RBUTTONUP, WM_CONTEXTMENU],
        Click::Right => &[WM_RBUTTONDOWN, WM_RBUTTONUP],
    };
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&raw mut pid));
        // The app brings its own menu / window to the front.
        let _ = AllowSetForegroundWindow(pid);
        for &message in messages {
            let (wparam, lparam) = callback_params(icon.version, icon.uid, message, at);
            let _ = PostMessageW(Some(hwnd), icon.callback, WPARAM(wparam), LPARAM(lparam));
        }
    }
}

/// Version 4 icons get the position and (event, id); older ones (id, event).
fn callback_params(version: u32, uid: u32, message: u32, at: POINT) -> (usize, isize) {
    let word = |low: u32, high: u32| ((high & 0xffff) << 16) | (low & 0xffff);
    if version > 3 {
        (
            word(at.x as u32, at.y as u32) as usize,
            word(message, uid) as i32 as isize,
        )
    } else {
        (uid as usize, message as isize)
    }
}

// ----- the thread -----

fn run(ready: &mpsc::Sender<()>) {
    unsafe {
        let com = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        let factory: Option<IWICImagingFactory> =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok();
        // Lives as long as the thread.
        WIC.set(factory.map(|f| &*Box::leak(Box::new(f))));
        let hwnd = create_window();
        let _ = ready.send(());
        if let Some(hwnd) = hwnd {
            WINDOW.store(hwnd.0 as isize, Ordering::SeqCst);
            // Stay in front of Explorer's taskbar: it moves up when the foreground
            // changes or when it shows itself.
            let mut hooks = vec![hook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND, 0)];
            hooks.push(explorer_hook());
            raise();
            announce();
            let mut msg = MSG::default();
            while GetMessageW(&raw mut msg, None, 0, 0).as_bool() {
                if msg.message == WM_REFRESH {
                    if let Some(old) = hooks.pop() {
                        let _ = UnhookWinEvent(old);
                    }
                    hooks.push(explorer_hook());
                    raise();
                    announce();
                    continue;
                }
                let _ = TranslateMessage(&raw const msg);
                DispatchMessageW(&raw const msg);
            }
            for h in hooks {
                let _ = UnhookWinEvent(h);
            }
            WINDOW.store(0, Ordering::SeqCst);
        }
        if com {
            CoUninitialize();
        }
    }
}

fn create_window() -> Option<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None).ok()?.into();
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(tray_proc),
            hInstance: instance,
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassExW(&raw const class);
        // Apps that ask where the taskbar is get the real one's place.
        let mut rc = RECT::default();
        if let Some(real) = real_taskbar() {
            let _ = GetWindowRect(real, &raw mut rc);
        }
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            CLASS,
            w!(""),
            WS_POPUP,
            rc.left,
            rc.top,
            rc.right - rc.left,
            rc.bottom - rc.top,
            None,
            None,
            Some(instance),
            None,
        )
        .ok()?;
        let _ = ChangeWindowMessageFilterEx(hwnd, WM_COPYDATA, MSGFLT_ALLOW, None);
        Some(hwnd)
    }
}

unsafe fn hook(first: u32, last: u32, process: u32) -> HWINEVENTHOOK {
    unsafe {
        SetWinEventHook(
            first,
            last,
            None,
            Some(on_event),
            process,
            0,
            WINEVENT_OUTOFCONTEXT,
        )
    }
}

fn explorer_hook() -> HWINEVENTHOOK {
    let mut pid = 0u32;
    if let Some(real) = real_taskbar() {
        unsafe { GetWindowThreadProcessId(real, Some(&raw mut pid)) };
    }
    // Without Explorer there is nothing to watch (process 0 would mean everyone).
    let pid = if pid == 0 {
        unsafe { GetCurrentProcessId() }
    } else {
        pid
    };
    unsafe { hook(EVENT_OBJECT_SHOW, EVENT_OBJECT_REORDER, pid) }
}

unsafe extern "system" fn on_event(
    _hook: HWINEVENTHOOK,
    _event: u32,
    _hwnd: HWND,
    id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    if id_object == 0 {
        raise();
    }
}

/// Moves our window in front, if another `Shell_TrayWnd` got there.
fn raise() {
    let own = WINDOW.load(Ordering::SeqCst);
    if own == 0 {
        return;
    }
    let first = unsafe { FindWindowW(CLASS, PCWSTR::null()) };
    if first.is_ok_and(|first| first.0 as isize != own) {
        unsafe {
            let _ = SetWindowPos(
                HWND(own as *mut _),
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }
}

/// Tells every other process' windows that the taskbar is new, so they add
/// their icons again.
fn announce() {
    unsafe extern "system" fn tell(hwnd: HWND, message: LPARAM) -> BOOL {
        let mut pid = 0u32;
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&raw mut pid));
            if pid != GetCurrentProcessId() {
                let _ = PostMessageW(Some(hwnd), message.0 as u32, WPARAM(0), LPARAM(0));
            }
        }
        true.into()
    }
    unsafe {
        let message = RegisterWindowMessageW(w!("TaskbarCreated"));
        if message != 0 {
            let _ = EnumWindows(Some(tell), LPARAM(message as isize));
        }
    }
}

extern "system" fn tray_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CLOSE => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        WM_COPYDATA => {
            read_copy_data(lparam);
            forward(msg, wparam, lparam)
        }
        // Whatever else is meant for the taskbar goes to the real one. Registered
        // messages (0xC000 up) are broadcasts Explorer gets by itself.
        m if m == WM_COMMAND || (WM_USER..WM_APP).contains(&m) => forward(msg, wparam, lparam),
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn forward(msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let Some(real) = real_taskbar() else {
        return LRESULT(0);
    };
    unsafe {
        if !InSendMessage().as_bool() && msg != WM_COPYDATA {
            let _ = PostMessageW(Some(real), msg, wparam, lparam);
            return LRESULT(0);
        }
        let mut result = 0usize;
        SendMessageTimeoutW(
            real,
            msg,
            wparam,
            lparam,
            SMTO_ABORTIFHUNG,
            FORWARD_TIMEOUT_MS,
            Some(&raw mut result),
        );
        LRESULT(result as isize)
    }
}

fn read_copy_data(lparam: LPARAM) {
    // SAFETY: the system keeps the structure and its data alive for this message.
    let Some(copy) = (unsafe { (lparam.0 as *const COPYDATASTRUCT).as_ref() }) else {
        return;
    };
    if copy.dwData != COPYDATA_NOTIFY || copy.lpData.is_null() {
        return;
    }
    let data =
        unsafe { std::slice::from_raw_parts(copy.lpData.cast::<u8>(), copy.cbData as usize) };
    let Some((message, wire)) = parse(data) else {
        return;
    };
    // The app may destroy its icon right after this call: convert it now.
    let image = (message != NIM_DELETE && wire.flags & NIF_ICON != 0 && wire.icon != 0)
        .then(|| convert(HICON(handle(wire.icon) as *mut _)))
        .flatten();
    let changed = apply(&mut lock(&ICONS), message, &wire, image);
    if changed {
        debug_log!(
            "nitrobar: tray icon message {message} uid {} flags {:#x}",
            { wire.uid },
            { wire.flags }
        );
        let bar = BAR.load(Ordering::SeqCst);
        if bar != 0 && !PENDING.swap(true, Ordering::SeqCst) {
            unsafe {
                let _ = PostMessageW(Some(HWND(bar as *mut _)), WM_APP_TRAY, WPARAM(0), LPARAM(0));
            }
        }
    }
}

fn convert(icon: HICON) -> Option<Arc<Image>> {
    let wic = WIC.get()?;
    unsafe {
        let source = wic.CreateBitmapFromHICON(icon).ok()?;
        let converter = wic.CreateFormatConverter().ok()?;
        converter
            .Initialize(
                &source,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeCustom,
            )
            .ok()?;
        let (mut width, mut height) = (0, 0);
        converter.GetSize(&raw mut width, &raw mut height).ok()?;
        if width == 0 || height == 0 || width > 256 || height > 256 {
            return None;
        }
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        converter
            .CopyPixels(std::ptr::null(), width * 4, &mut pixels)
            .ok()?;
        Some(Arc::new(Image {
            width,
            height,
            pixels,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire(uid: u32, flags: u32, tip: &str) -> WireIcon {
        // SAFETY: all zeros is a value of this plain struct.
        let mut w: WireIcon = unsafe { std::mem::zeroed() };
        w.hwnd = 0x1234;
        w.uid = uid;
        w.flags = flags;
        w.callback = 0x8001;
        for (d, s) in w.tip.iter_mut().zip(tip.encode_utf16()) {
            *d = s;
        }
        w
    }

    #[test]
    fn calls_add_change_and_remove_icons() {
        let mut list = Vec::new();
        let add = wire(1, NIF_MESSAGE | NIF_TIP, "one");
        assert!(apply(&mut list, NIM_ADD, &add, None));
        // The same again changes nothing.
        assert!(!apply(&mut list, NIM_MODIFY, &add, None));
        assert!(apply(&mut list, NIM_MODIFY, &wire(1, NIF_TIP, "two"), None));
        assert_eq!(list[0].tip, "two".encode_utf16().collect::<Vec<_>>());
        assert_eq!(list[0].callback, 0x8001);
        let mut version = wire(1, 0, "");
        version.version = 4;
        apply(&mut list, NIM_SETVERSION, &version, None);
        assert_eq!(list[0].version, 4);
        assert!(apply(&mut list, NIM_DELETE, &wire(1, 0, ""), None));
        assert!(list.is_empty());
    }

    #[test]
    fn only_windows_status_icons_are_system() {
        let volume = [
            0x73, 0xAE, 0x20, 0x78, 0xE3, 0x23, 0x29, 0x42, 0x82, 0xC1, 0xE4, 0x1C, 0xB6, 0x7D,
            0x5B, 0x9C,
        ];
        assert!(is_system(volume));
        assert!(!is_system([0; 16]));
    }

    #[test]
    fn short_and_full_messages_parse() {
        assert!(parse(&[0; 8]).is_none());
        let mut data = vec![0u8; WIRE_HEADER + 40];
        data[4] = 1;
        data[WIRE_HEADER + 8] = 7;
        let (message, wire) = parse(&data).unwrap();
        assert_eq!((message, { wire.uid }), (NIM_MODIFY, 7));
    }

    #[test]
    fn callbacks_follow_the_icon_version() {
        let at = POINT { x: 10, y: 20 };
        assert_eq!(callback_params(0, 5, WM_RBUTTONUP, at), (5, 0x205));
        assert_eq!(
            callback_params(4, 5, WM_RBUTTONUP, at),
            (0x0014_000A, 0x0005_0205)
        );
    }
}
