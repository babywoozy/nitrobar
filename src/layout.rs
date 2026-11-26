//! Bar geometry in DIPs, shared by drawing and hit-testing so they never disagree.

use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;

/// Height of the bar (same as the Windows 11 taskbar).
pub const BAR_HEIGHT: f32 = 48.0;
pub const BUTTON_WIDTH: f32 = 44.0;
pub const BUTTON_HEIGHT: f32 = 40.0;
pub const BUTTON_GAP: f32 = 4.0;
pub const ICON_SIZE: f32 = 24.0;
/// Icon size while the button is held down (Windows 11 "press" feedback).
pub const ICON_SIZE_PRESSED: f32 = 20.0;
pub const START_LOGO_SIZE: f32 = 20.0;
pub const INDICATOR_HEIGHT: f32 = 3.0;
pub const INDICATOR_RUNNING_WIDTH: f32 = 6.0;
pub const INDICATOR_ACTIVE_WIDTH: f32 = 16.0;
pub const CORNER_RADIUS: f32 = 4.0;

pub const CLOCK_WIDTH: f32 = 84.0;
pub const CLOCK_TEXT_PADDING: f32 = 8.0;
pub const CLOCK_LINE_HEIGHT: f32 = 16.0;
pub const RIGHT_MARGIN: f32 = 8.0;

pub const STATUS_PADDING: f32 = 8.0;
pub const STATUS_CELL: f32 = 24.0;
pub const STATUS_GLYPH_SIZE: f32 = 16.0;
pub const STATUS_PERCENT_WIDTH: f32 = 34.0;
pub const STATUS_GAP: f32 = 2.0;

/// Width of the status button for `icons` glyphs (+ battery percentage).
pub fn status_width(icons: usize, percent: bool) -> f32 {
    if icons == 0 {
        return 0.0;
    }
    let text = if percent { STATUS_PERCENT_WIDTH } else { 0.0 };
    STATUS_PADDING * 2.0 + icons as f32 * STATUS_CELL + text
}

/// What the pointer is over.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hit {
    Start,
    Task(usize),
    Status,
    Clock,
}

pub struct BarLayout {
    pub start: D2D_RECT_F,
    pub tasks: Vec<D2D_RECT_F>,
    /// Network / volume / battery button, left of the clock (zero width if empty).
    pub status: D2D_RECT_F,
    pub clock: D2D_RECT_F,
}

fn button_top() -> f32 {
    ((BAR_HEIGHT - BUTTON_HEIGHT) / 2.0).round()
}

impl BarLayout {
    /// Start + task buttons centered like Windows 11; clock on the right.
    /// Task buttons that would run into the right-hand area are dropped.
    pub fn new(bar_width: f32, task_count: usize, status_width: f32) -> Self {
        let top = button_top();
        let clock_right = bar_width - RIGHT_MARGIN;
        let clock = D2D_RECT_F {
            left: clock_right - CLOCK_WIDTH,
            top,
            right: clock_right,
            bottom: top + BUTTON_HEIGHT,
        };

        let status = D2D_RECT_F {
            left: clock.left - STATUS_GAP - status_width,
            top,
            right: clock.left - STATUS_GAP,
            bottom: top + BUTTON_HEIGHT,
        };

        let limit = status.left - BUTTON_GAP;
        let step = BUTTON_WIDTH + BUTTON_GAP;
        let max_fit = ((limit + BUTTON_GAP) / step).max(1.0) as usize;
        let count = (task_count + 1).min(max_fit);
        let total = count as f32 * BUTTON_WIDTH + count.saturating_sub(1) as f32 * BUTTON_GAP;
        let first = ((bar_width - total) / 2.0).round().max(0.0);

        let mut rects = (0..count).map(|i| {
            let left = first + i as f32 * step;
            D2D_RECT_F {
                left,
                top,
                right: left + BUTTON_WIDTH,
                bottom: top + BUTTON_HEIGHT,
            }
        });
        let start = rects.next().unwrap_or_default();
        Self {
            start,
            tasks: rects.collect(),
            status,
            clock,
        }
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        if contains(&self.start, x, y) {
            return Some(Hit::Start);
        }
        if let Some(i) = self.tasks.iter().position(|r| contains(r, x, y)) {
            return Some(Hit::Task(i));
        }
        if self.status.right > self.status.left && contains(&self.status, x, y) {
            return Some(Hit::Status);
        }
        contains(&self.clock, x, y).then_some(Hit::Clock)
    }
}

pub fn contains(rect: &D2D_RECT_F, x: f32, y: f32) -> bool {
    x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom
}

/// Square of `size` centered in `rect`, shifted by `y_offset`.
pub fn centered_square(rect: &D2D_RECT_F, size: f32, y_offset: f32) -> D2D_RECT_F {
    let left = (rect.left + rect.right - size) / 2.0;
    let top = (rect.top + rect.bottom - size) / 2.0 + y_offset;
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
