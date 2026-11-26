//! Clock text, formatted with the user's locale (same format as the native taskbar).

use windows::Win32::Globalization::{
    DATE_SHORTDATE, GetDateFormatEx, GetTimeFormatEx, TIME_NOSECONDS,
};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::core::PCWSTR;

pub struct ClockText {
    pub time: Vec<u16>,
    pub date: Vec<u16>,
}

/// Converts a Win32 "chars written, including the terminator" count into a slice.
fn written(buf: &[u16], len: i32) -> Vec<u16> {
    let len = usize::try_from(len).unwrap_or(0).saturating_sub(1);
    buf[..len.min(buf.len())].to_vec()
}

pub fn now() -> ClockText {
    let st = unsafe { GetLocalTime() };
    let mut time = [0u16; 64];
    let mut date = [0u16; 64];
    // A null locale name means LOCALE_NAME_USER_DEFAULT.
    let time_len = unsafe {
        GetTimeFormatEx(
            PCWSTR::null(),
            TIME_NOSECONDS,
            Some(&st),
            PCWSTR::null(),
            Some(&mut time),
        )
    };
    let date_len = unsafe {
        GetDateFormatEx(
            PCWSTR::null(),
            DATE_SHORTDATE,
            Some(&st),
            PCWSTR::null(),
            Some(&mut date),
            PCWSTR::null(),
        )
    };
    ClockText {
        time: written(&time, time_len),
        date: written(&date, date_len),
    }
}

/// Milliseconds until just after the next minute boundary, so the clock ticks once a minute.
pub fn ms_until_next_minute() -> u32 {
    let st = unsafe { GetLocalTime() };
    let elapsed = u32::from(st.wSecond) * 1000 + u32::from(st.wMilliseconds);
    60_000u32.saturating_sub(elapsed) + 50
}
