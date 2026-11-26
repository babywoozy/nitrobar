//! Opening Windows' own UI (Start, Quick Settings, notification center).
//!
//! There is no public API for these, so we synthesize the documented keyboard
//! shortcuts. This only runs in response to a click on the bar, so our process
//! owns the last input event and `SendInput` is allowed.

use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput,
    VIRTUAL_KEY, VK_LWIN, VK_N,
};

fn key(vk: VIRTUAL_KEY, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                dwFlags: flags,
                ..Default::default()
            },
        },
    }
}

/// Presses `keys` in order, then releases them in reverse order.
fn chord(keys: &[VIRTUAL_KEY]) {
    let mut inputs: Vec<INPUT> = keys
        .iter()
        .map(|&vk| key(vk, KEYBD_EVENT_FLAGS(0)))
        .collect();
    inputs.extend(keys.iter().rev().map(|&vk| key(vk, KEYEVENTF_KEYUP)));
    unsafe {
        SendInput(&inputs, size_of::<INPUT>() as i32);
    }
}

/// Toggles the Start menu (Win).
pub fn open_start() {
    chord(&[VK_LWIN]);
}

/// Toggles the notification center + calendar (Win+N).
pub fn open_notification_center() {
    chord(&[VK_LWIN, VK_N]);
}
