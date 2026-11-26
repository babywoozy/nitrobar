//! Battery level and charging state. Updates arrive as WM_POWERBROADCAST.

use windows::Win32::Foundation::{HANDLE, HWND};
use windows::Win32::System::Power::{
    GetSystemPowerStatus, HPOWERNOTIFY, RegisterPowerSettingNotification, SYSTEM_POWER_STATUS,
    UnregisterPowerSettingNotification,
};
use windows::Win32::System::SystemServices::{
    GUID_ACDC_POWER_SOURCE, GUID_BATTERY_PERCENTAGE_REMAINING,
};
use windows::Win32::UI::WindowsAndMessaging::DEVICE_NOTIFY_WINDOW_HANDLE;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BatteryState {
    pub percent: u8,
    /// On AC power (Windows shows the charging glyph then).
    pub charging: bool,
}

const BATTERY_FLAG_NO_BATTERY: u8 = 128;
const BATTERY_FLAG_UNKNOWN: u8 = 255;
const PERCENT_UNKNOWN: u8 = 255;
const AC_ONLINE: u8 = 1;

/// `None` on machines without a battery (desktops): the icon is hidden then.
pub fn read() -> Option<BatteryState> {
    let mut status = SYSTEM_POWER_STATUS::default();
    unsafe { GetSystemPowerStatus(&raw mut status) }.ok()?;
    if status.BatteryFlag & BATTERY_FLAG_NO_BATTERY != 0
        || status.BatteryFlag == BATTERY_FLAG_UNKNOWN
        || status.BatteryLifePercent == PERCENT_UNKNOWN
    {
        return None;
    }
    Some(BatteryState {
        percent: status.BatteryLifePercent.min(100),
        charging: status.ACLineStatus == AC_ONLINE,
    })
}

/// Subscriptions that make Windows send PBT_POWERSETTINGCHANGE to the bar.
pub struct PowerNotifications {
    handles: Vec<HPOWERNOTIFY>,
}

impl PowerNotifications {
    pub fn register(bar: HWND) -> Self {
        let handles = [GUID_BATTERY_PERCENTAGE_REMAINING, GUID_ACDC_POWER_SOURCE]
            .iter()
            .filter_map(|guid| unsafe {
                RegisterPowerSettingNotification(HANDLE(bar.0), guid, DEVICE_NOTIFY_WINDOW_HANDLE)
                    .ok()
            })
            .collect();
        Self { handles }
    }

    pub fn unregister(&mut self) {
        for handle in self.handles.drain(..) {
            unsafe {
                let _ = UnregisterPowerSettingNotification(handle);
            }
        }
    }
}
