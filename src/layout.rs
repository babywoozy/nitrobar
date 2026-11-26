//! Bar geometry in DIPs, shared by drawing and hit-testing so they never disagree.

use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;

/// Height of the bar (same as the Windows 11 taskbar).
pub const BAR_HEIGHT: f32 = 48.0;
pub const BUTTON_WIDTH: f32 = 44.0;
pub const BUTTON_HEIGHT: f32 = 40.0;
pub const BUTTON_GAP: f32 = 4.0;
pub const ICON_SIZE: f32 = 24.0;
pub const INDICATOR_HEIGHT: f32 = 3.0;
pub const INDICATOR_RUNNING_WIDTH: f32 = 6.0;
pub const INDICATOR_ACTIVE_WIDTH: f32 = 16.0;
pub const CORNER_RADIUS: f32 = 4.0;

pub const CLOCK_WIDTH: f32 = 120.0;
pub const CLOCK_RIGHT_PADDING: f32 = 12.0;
pub const CLOCK_LINE_HEIGHT: f32 = 16.0;

/// Rectangles of `count` buttons, centered horizontally like the Windows 11 taskbar.
/// Buttons that would run into the clock area are dropped.
pub fn center_buttons(bar_width: f32, count: usize) -> Vec<D2D_RECT_F> {
    if count == 0 {
        return Vec::new();
    }
    let limit = bar_width - CLOCK_WIDTH - CLOCK_RIGHT_PADDING;
    let max_fit = ((limit + BUTTON_GAP) / (BUTTON_WIDTH + BUTTON_GAP)).max(0.0) as usize;
    let count = count.min(max_fit);
    let total = count as f32 * BUTTON_WIDTH + count.saturating_sub(1) as f32 * BUTTON_GAP;
    let mut left = ((bar_width - total) / 2.0).round().max(0.0);
    let top = ((BAR_HEIGHT - BUTTON_HEIGHT) / 2.0).round();
    (0..count)
        .map(|_| {
            let rect = D2D_RECT_F {
                left,
                top,
                right: left + BUTTON_WIDTH,
                bottom: top + BUTTON_HEIGHT,
            };
            left += BUTTON_WIDTH + BUTTON_GAP;
            rect
        })
        .collect()
}

pub fn contains(rect: &D2D_RECT_F, x: f32, y: f32) -> bool {
    x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom
}

/// Index of the button under (x, y), if any.
pub fn hit_test(rects: &[D2D_RECT_F], x: f32, y: f32) -> Option<usize> {
    rects.iter().position(|r| contains(r, x, y))
}

/// Square of `size` centered in `rect`.
pub fn centered_square(rect: &D2D_RECT_F, size: f32, y_offset: f32) -> D2D_RECT_F {
    let left = ((rect.left + rect.right - size) / 2.0).round();
    let top = ((rect.top + rect.bottom - size) / 2.0).round() + y_offset;
    D2D_RECT_F {
        left,
        top,
        right: left + size,
        bottom: top + size,
    }
}

/// The small pill under a button.
pub fn indicator(rect: &D2D_RECT_F, width: f32) -> D2D_RECT_F {
    let left = (rect.left + rect.right - width) / 2.0;
    D2D_RECT_F {
        left,
        top: rect.bottom - INDICATOR_HEIGHT - 1.0,
        right: left + width,
        bottom: rect.bottom - 1.0,
    }
}
