//! Network state: internet connectivity (IP Helper hint) + Wi-Fi signal (WLAN API).
//!
//! Nothing here runs on the UI thread except reading the cached state: the WLAN
//! service can take seconds to answer (e.g. right after boot), so setup happens on
//! a short-lived worker thread and state is recomputed inside the OS callbacks,
//! which already run on thread-pool threads. The UI only reads `current()`.

use std::ffi::c_void;
use std::ptr::null_mut;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use windows::Win32::Foundation::{HANDLE, WIN32_ERROR};
use windows::Win32::NetworkManagement::IpHelper::{
    CancelMibChangeNotify2, GetNetworkConnectivityHint, NotifyNetworkConnectivityHintChange,
};
use windows::Win32::NetworkManagement::WiFi::{
    L2_NOTIFICATION_DATA, WLAN_CONNECTION_ATTRIBUTES, WLAN_INTERFACE_INFO_LIST,
    WLAN_NOTIFICATION_SOURCE_ACM, WLAN_NOTIFICATION_SOURCE_MSM, WlanCloseHandle,
    WlanEnumInterfaces, WlanFreeMemory, WlanOpenHandle, WlanQueryInterface,
    WlanRegisterNotification, wlan_interface_state_connected, wlan_intf_opcode_current_connection,
};
use windows::Win32::Networking::WinSock::{
    NL_NETWORK_CONNECTIVITY_HINT, NetworkConnectivityLevelHintConstrainedInternetAccess,
    NetworkConnectivityLevelHintInternetAccess, NetworkConnectivityLevelHintLocalAccess,
};

use super::{SOURCE_NETWORK, notify};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NetState {
    /// Not known yet (worker still starting).
    Unknown,
    Offline,
    Ethernet {
        internet: bool,
    },
    /// `bars` is 1..=4.
    Wifi {
        bars: u8,
        internet: bool,
    },
}

const WLAN_CLIENT_VERSION_2: u32 = 2;

static STATE: Mutex<NetState> = Mutex::new(NetState::Unknown);
static WLAN: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
static HINT: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
static STOPPED: AtomicBool = AtomicBool::new(false);

/// Last computed state (cheap, UI thread).
pub fn current() -> NetState {
    STATE.lock().map_or(NetState::Unknown, |s| *s)
}

/// Recomputes the state; notifies the UI only if it changed. Any thread.
fn update() {
    if STOPPED.load(Ordering::SeqCst) {
        return;
    }
    let fresh = compute();
    let changed = match STATE.lock() {
        Ok(mut state) if *state != fresh => {
            *state = fresh;
            true
        }
        _ => false,
    };
    if changed {
        notify(SOURCE_NETWORK);
    }
}

unsafe extern "system" fn on_connectivity(
    _context: *const c_void,
    _hint: NL_NETWORK_CONNECTIVITY_HINT,
) {
    update();
}

unsafe extern "system" fn on_wlan(_data: *mut L2_NOTIFICATION_DATA, _context: *mut c_void) {
    update();
}

/// Registers for notifications and computes the first state, off the UI thread.
pub fn start() {
    STOPPED.store(false, Ordering::SeqCst);
    let spawned = std::thread::Builder::new()
        .name("nitrobar-network".into())
        .stack_size(64 * 1024)
        .spawn(|| {
            unsafe {
                let mut hint = HANDLE::default();
                if NotifyNetworkConnectivityHintChange(
                    Some(on_connectivity),
                    None,
                    false,
                    &raw mut hint,
                ) == WIN32_ERROR(0)
                {
                    HINT.store(hint.0, Ordering::SeqCst);
                }
                // No WLAN service (desktop without Wi-Fi) is fine: Wi-Fi is just never shown.
                let mut negotiated = 0u32;
                let mut wlan = HANDLE::default();
                if WlanOpenHandle(
                    WLAN_CLIENT_VERSION_2,
                    None,
                    &raw mut negotiated,
                    &raw mut wlan,
                ) == 0
                {
                    WLAN.store(wlan.0, Ordering::SeqCst);
                    let _ = WlanRegisterNotification(
                        wlan,
                        WLAN_NOTIFICATION_SOURCE_ACM | WLAN_NOTIFICATION_SOURCE_MSM,
                        true,
                        Some(on_wlan),
                        None,
                        None,
                        None,
                    );
                }
            }
            if STOPPED.load(Ordering::SeqCst) {
                stop();
            } else {
                update();
            }
        });
    if spawned.is_err() {
        debug_log!("nitrobar: could not start network thread");
    }
}

/// Cancels notifications and closes handles. Safe to call more than once.
pub fn stop() {
    STOPPED.store(true, Ordering::SeqCst);
    unsafe {
        let hint = HINT.swap(null_mut(), Ordering::SeqCst);
        if !hint.is_null() {
            let _ = CancelMibChangeNotify2(HANDLE(hint));
        }
        let wlan = WLAN.swap(null_mut(), Ordering::SeqCst);
        if !wlan.is_null() {
            // Closing the handle also unregisters the WLAN notification callback.
            WlanCloseHandle(HANDLE(wlan), None);
        }
    }
}

/// Signal quality (0..=100) of the first connected Wi-Fi interface.
fn wifi_quality() -> Option<u32> {
    let wlan = WLAN.load(Ordering::SeqCst);
    if wlan.is_null() {
        return None;
    }
    let wlan = HANDLE(wlan);
    unsafe {
        let mut list: *mut WLAN_INTERFACE_INFO_LIST = null_mut();
        if WlanEnumInterfaces(wlan, None, &raw mut list) != 0 || list.is_null() {
            return None;
        }
        let count = (*list).dwNumberOfItems as usize;
        let interfaces = std::slice::from_raw_parts((*list).InterfaceInfo.as_ptr(), count);
        let mut quality = None;
        for info in interfaces {
            if info.isState != wlan_interface_state_connected {
                continue;
            }
            let mut size = 0u32;
            let mut data: *mut c_void = null_mut();
            if WlanQueryInterface(
                wlan,
                &raw const info.InterfaceGuid,
                wlan_intf_opcode_current_connection,
                None,
                &raw mut size,
                &raw mut data,
                None,
            ) == 0
                && !data.is_null()
            {
                let attrs = &*data.cast::<WLAN_CONNECTION_ATTRIBUTES>();
                quality = Some(attrs.wlanAssociationAttributes.wlanSignalQuality);
                WlanFreeMemory(data);
                break;
            }
        }
        WlanFreeMemory(list.cast());
        quality
    }
}

fn compute() -> NetState {
    let mut hint = NL_NETWORK_CONNECTIVITY_HINT::default();
    let ok = unsafe { GetNetworkConnectivityHint(&raw mut hint) } == WIN32_ERROR(0);
    let level = hint.ConnectivityLevel;
    let internet = ok
        && (level == NetworkConnectivityLevelHintInternetAccess
            || level == NetworkConnectivityLevelHintConstrainedInternetAccess);
    let local = internet || (ok && level == NetworkConnectivityLevelHintLocalAccess);

    if let Some(quality) = wifi_quality() {
        let bars = match quality {
            0..25 => 1,
            25..50 => 2,
            50..75 => 3,
            _ => 4,
        };
        NetState::Wifi { bars, internet }
    } else if local {
        NetState::Ethernet { internet }
    } else {
        NetState::Offline
    }
}
