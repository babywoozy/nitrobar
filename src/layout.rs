//! Bar geometry in DIPs, shared by drawing and hit-testing so they never disagree.

use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;

pub const BUTTON_GAP: f32 = 4.0;
pub const INDICATOR_HEIGHT: f32 = 3.0;
pub const INDICATOR_RUNNING_WIDTH: f32 = 6.0;
pub const INDICATOR_ACTIVE_WIDTH: f32 = 16.0;
pub const CORNER_RADIUS: f32 = 4.0;
pub const EDGE_MARGIN: f32 = 8.0;

pub const CLOCK_WIDTH: f32 = 84.0;
pub const CLOCK_TEXT_PADDING: f32 = 8.0;
/// The island strip's clock sits closer to the panel's end.
pub const ISLAND_CLOCK_PADDING: f32 = 5.0;
pub const CLOCK_LINE_HEIGHT: f32 = 16.0;

pub const STATUS_PADDING: f32 = 8.0;
pub const STATUS_CELL: f32 = 24.0;
pub const STATUS_GLYPH_SIZE: f32 = 16.0;
pub const STATUS_PERCENT_WIDTH: f32 = 34.0;
pub const STATUS_GAP: f32 = 2.0;

/// Sizes that come from the user config (`[bar]` height / icon_size / alignment / position).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Metrics {
    pub bar_height: f32,
    pub icon_size: f32,
    pub button_width: f32,
    pub button_height: f32,
    pub align_left: bool,
    /// Bar docked at the top: indicators sit at the top edge of buttons.
    pub at_top: bool,
}

impl Metrics {
    pub fn new(bar_height: f32, icon_size: f32, align_left: bool, at_top: bool) -> Self {
        Self {
            bar_height,
            icon_size,
            // Windows 11 proportions: 24 DIP icon in a 44 x 40 button on a 48 DIP bar.
            button_width: icon_size + 20.0,
            button_height: (bar_height - 8.0).max(icon_size + 8.0).min(bar_height),
            align_left,
            at_top,
        }
    }

    /// Icon size while the button is held down (Windows 11 "press" feedback).
    pub fn pressed_icon_size(&self) -> f32 {
        (self.icon_size * 0.83).round()
    }

    pub fn start_logo_size(&self) -> f32 {
        (self.icon_size * 0.83).round()
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new(48.0, 24.0, false, false)
    }
}

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
    /// Index into the bar items (pinned + running apps).
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

impl BarLayout {
    /// Start + app buttons (centered like Windows 11, or left-aligned); status and
    /// clock on the right. App buttons that would run into the right side are dropped.
    pub fn new(bar_width: f32, task_count: usize, status_width: f32, m: &Metrics) -> Self {
        let top = ((m.bar_height - m.button_height) / 2.0).round();
        let bottom = top + m.button_height;
        let clock_right = bar_width - EDGE_MARGIN;
        let clock = D2D_RECT_F {
            left: clock_right - CLOCK_WIDTH,
            top,
            right: clock_right,
            bottom,
        };
        let status = D2D_RECT_F {
            left: clock.left - STATUS_GAP - status_width,
            top,
            right: clock.left - STATUS_GAP,
            bottom,
        };

        let step = m.button_width + BUTTON_GAP;
        // Buttons must end before the status area.
        let limit = status.left - BUTTON_GAP;
        let max_fit = ((limit - EDGE_MARGIN + BUTTON_GAP) / step).max(1.0) as usize;
        let count = (task_count + 1).min(max_fit);
        let total = count as f32 * m.button_width + count.saturating_sub(1) as f32 * BUTTON_GAP;
        let first = if m.align_left {
            EDGE_MARGIN
        } else {
            // Centered on the bar while that fits; otherwise slide left to stay clear
            // of the status area.
            let centered = ((bar_width - total) / 2.0).round();
            centered.min(limit - total).max(EDGE_MARGIN)
        };

        let mut rects = (0..count).map(|i| {
            let left = first + i as f32 * step;
            D2D_RECT_F {
                left,
                top,
                right: left + m.button_width,
                bottom,
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

/// Square of exactly `size_px` device pixels centered in `rect` (DIPs), with its
/// edges on whole pixels so a bitmap of that size is drawn 1:1, without blur.
pub fn pixel_square(rect: &D2D_RECT_F, size_px: f32, y_offset: f32, dpi: f32) -> D2D_RECT_F {
    let scale = dpi / 96.0;
    let cx = (rect.left + rect.right) / 2.0 * scale;
    let cy = ((rect.top + rect.bottom) / 2.0 + y_offset) * scale;
    let left = (cx - size_px / 2.0).round();
    let top = (cy - size_px / 2.0).round();
    D2D_RECT_F {
        left: left / scale,
        top: top / scale,
        right: (left + size_px) / scale,
        bottom: (top + size_px) / scale,
    }
}

/// The small pill on the screen-edge side of a button.
pub fn indicator(rect: &D2D_RECT_F, width: f32, at_top: bool) -> D2D_RECT_F {
    let left = (rect.left + rect.right - width) / 2.0;
    let top = if at_top {
        rect.top + 1.0
    } else {
        rect.bottom - INDICATOR_HEIGHT - 1.0
    };
    D2D_RECT_F {
        left,
        top,
        right: left + width,
        bottom: top + INDICATOR_HEIGHT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_layout_matches_windows_11() {
        let m = Metrics::default();
        let layout = BarLayout::new(1920.0, 3, 0.0, &m);
        assert_eq!(layout.tasks.len(), 3);
        assert_eq!(layout.start.right - layout.start.left, 44.0);
        assert_eq!(layout.start.bottom - layout.start.top, 40.0);
        // Start + 3 apps = 4 buttons centered on 1920.
        let total = 4.0 * 44.0 + 3.0 * BUTTON_GAP;
        assert_eq!(layout.start.left, ((1920.0 - total) / 2.0_f32).round());
    }

    #[test]
    fn left_alignment_starts_at_margin() {
        let m = Metrics::new(48.0, 24.0, true, false);
        let layout = BarLayout::new(1920.0, 2, 100.0, &m);
        assert_eq!(layout.start.left, EDGE_MARGIN);
    }

    #[test]
    fn overflowing_buttons_are_dropped() {
        let m = Metrics::default();
        let layout = BarLayout::new(600.0, 100, 130.0, &m);
        let last = layout.tasks.last().unwrap();
        assert!(last.right <= layout.status.left);
    }

    #[test]
    fn icons_land_on_whole_pixels() {
        let m = Metrics::default();
        let layout = BarLayout::new(1536.0, 5, 130.0, &m);
        for dpi in [96.0_f32, 120.0, 144.0, 168.0] {
            let scale = dpi / 96.0;
            let px = (m.icon_size * scale).round();
            for rect in &layout.tasks {
                let icon = pixel_square(rect, px, -1.0, dpi);
                for edge in [icon.left, icon.top, icon.right, icon.bottom] {
                    let device = edge * scale;
                    assert!((device - device.round()).abs() < 1e-3, "{dpi}: {device}");
                }
                assert!(((icon.right - icon.left) * scale - px).abs() < 1e-3);
            }
        }
    }

    #[test]
    fn hit_testing() {
        let m = Metrics::default();
        let layout = BarLayout::new(1920.0, 2, 130.0, &m);
        let mid = |r: &D2D_RECT_F| ((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0);
        let (x, y) = mid(&layout.start);
        assert_eq!(layout.hit(x, y), Some(Hit::Start));
        let (x, y) = mid(&layout.tasks[1]);
        assert_eq!(layout.hit(x, y), Some(Hit::Task(1)));
        let (x, y) = mid(&layout.status);
        assert_eq!(layout.hit(x, y), Some(Hit::Status));
        let (x, y) = mid(&layout.clock);
        assert_eq!(layout.hit(x, y), Some(Hit::Clock));
        assert_eq!(layout.hit(10.0, 24.0), None);
    }
}
