//! Clock text, formatted with the user's locale (same format as the native taskbar).

use windows::Win32::Globalization::{
    DATE_SHORTDATE, GetDateFormatEx, GetLocaleInfoEx, GetTimeFormatEx, LOCALE_ITIME,
    TIME_FORMAT_FLAGS,
};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::core::{PCWSTR, w};

use crate::config::HourFormat;

pub struct ClockText {
    pub time: Vec<u16>,
    pub date: Vec<u16>,
}

/// Converts a Win32 "chars written, including the terminator" count into a slice.
fn written(buf: &[u16], len: i32) -> Vec<u16> {
    let len = usize::try_from(len).unwrap_or(0).saturating_sub(1);
    buf[..len.min(buf.len())].to_vec()
}

/// Whether the user's locale writes the time with 24 hours.
fn locale_is_24h() -> bool {
    let mut value = [0u16; 4];
    let len = unsafe { GetLocaleInfoEx(PCWSTR::null(), LOCALE_ITIME, Some(&mut value)) };
    len > 0 && value[0] == u16::from(b'1')
}

/// The time as "6:47" / "18:47" (no AM / PM) in the chosen style, and the short date.
pub fn now(hours: HourFormat) -> ClockText {
    let twenty_four = match hours {
        HourFormat::Auto => locale_is_24h(),
        HourFormat::H12 => false,
        HourFormat::H24 => true,
    };
    let format = if twenty_four { w!("H:mm") } else { w!("h:mm") };
    let st = unsafe { GetLocalTime() };
    let mut time = [0u16; 64];
    let mut date = [0u16; 64];
    // A null locale name means LOCALE_NAME_USER_DEFAULT.
    let time_len = unsafe {
        GetTimeFormatEx(
            PCWSTR::null(),
            TIME_FORMAT_FLAGS(0),
            Some(&st),
            format,
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
