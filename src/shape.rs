//! Continuous ("squircle") corners: superellipse quarters instead of circular arcs
//! joined to straight edges, so the curvature builds up smoothly (as on Apple
//! hardware). Pure geometry; `render.rs` turns the points into a path.

use std::f32::consts::FRAC_PI_2;

use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;

/// Straight segments per corner: far below a pixel of error at pill sizes.
const STEPS: usize = 32;
/// Superellipse exponent and how far the curve reaches along the straight edge,
/// relative to the half height, for the capsule-shaped island.
const PILL_EXPONENT: f32 = 2.35;
const PILL_REACH: f32 = 1.3;
/// Exponent for small, nearly square plates.
const PLATE_EXPONENT: f32 = 3.4;

/// Outline (clockwise) of `rect` whose corners are superellipse quarters reaching
/// `ax` along the horizontal edges and `ay` along the vertical ones.
pub fn outline(rect: &D2D_RECT_F, ax: f32, ay: f32, exponent: f32) -> Vec<(f32, f32)> {
    let ax = ax.min((rect.right - rect.left) / 2.0).max(0.0);
    let ay = ay.min((rect.bottom - rect.top) / 2.0).max(0.0);
    let power = 2.0 / exponent;
    // One quarter, from the horizontal edge (0, 1) to the vertical one (1, 0).
    let quarter: Vec<(f32, f32)> = (0..=STEPS)
        .map(|i| {
            let t = i as f32 / STEPS as f32 * FRAC_PI_2;
            (t.sin().max(0.0).powf(power), t.cos().max(0.0).powf(power))
        })
        .collect();
    let mut points = Vec::with_capacity(4 * (STEPS + 1));
    // Top right, bottom right, bottom left, top left.
    let (cx, cy) = (rect.right - ax, rect.top + ay);
    points.extend(quarter.iter().map(|&(x, y)| (cx + ax * x, cy - ay * y)));
    let cy = rect.bottom - ay;
    points.extend(
        quarter
            .iter()
            .rev()
            .map(|&(x, y)| (cx + ax * x, cy + ay * y)),
    );
    let cx = rect.left + ax;
    points.extend(quarter.iter().map(|&(x, y)| (cx - ax * x, cy + ay * y)));
    let cy = rect.top + ay;
    points.extend(
        quarter
            .iter()
            .rev()
            .map(|&(x, y)| (cx - ax * x, cy - ay * y)),
    );
    points
}

/// The island's capsule.
pub fn pill(rect: &D2D_RECT_F) -> Vec<(f32, f32)> {
    let half = (rect.bottom - rect.top) / 2.0;
    outline(rect, half * PILL_REACH, half, PILL_EXPONENT)
}

/// A hover plate inside the island.
pub fn plate(rect: &D2D_RECT_F) -> Vec<(f32, f32)> {
    let half = (rect.bottom - rect.top) / 2.0;
    outline(rect, half, half, PLATE_EXPONENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECT: D2D_RECT_F = D2D_RECT_F {
        left: 10.0,
        top: 20.0,
        right: 210.0,
        bottom: 64.0,
    };

    #[test]
    fn outline_fills_the_rect_symmetrically() {
        let points = pill(&RECT);
        let (mut min_x, mut max_x, mut min_y, mut max_y) = (f32::MAX, 0.0f32, f32::MAX, 0.0f32);
        for &(x, y) in &points {
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(y);
            max_y = max_y.max(y);
            // Mirrored left / right and top / bottom.
            let mirrored = |mx: f32, my: f32| {
                points
                    .iter()
                    .any(|&(a, b)| (a - mx).abs() < 0.01 && (b - my).abs() < 0.01)
            };
            assert!(mirrored(220.0 - x, y) && mirrored(x, 84.0 - y), "{x} {y}");
        }
        assert!((min_x - 10.0).abs() < 0.01 && (max_x - 210.0).abs() < 0.01);
        assert!((min_y - 20.0).abs() < 0.01 && (max_y - 64.0).abs() < 0.01);
    }

    #[test]
    fn corners_are_fuller_than_a_circle_and_have_no_jumps() {
        let points = pill(&RECT);
        // Consecutive points stay close (a smooth path), apart from the straight edges.
        for pair in points.windows(2) {
            let (dx, dy) = (pair[1].0 - pair[0].0, pair[1].1 - pair[0].1);
            assert!(dy.abs() < 6.0, "{dx} {dy}");
        }
        // At 45 degrees a superellipse lies outside the ellipse of the same extents.
        let (ax, ay) = (22.0 * PILL_REACH, 22.0);
        let mid = points[STEPS / 2];
        let nx = (mid.0 - (RECT.right - ax)) / ax;
        let ny = ((RECT.top + ay) - mid.1) / ay;
        assert!(nx * nx + ny * ny > 1.02, "{nx} {ny}");
    }
}
