//! Thin egui-specific geometry helpers and `Pos2` ↔ `ScreenPoint` bridging.
//!
//! Pure geometry algorithms (point-in-polygon, AABB, line clipping) have moved
//! to `rustdar_overlays::render::geo`.  This module retains only the helpers
//! that produce egui drawing primitives or convert between coordinate types.

use egui::{Pos2, Shape, Stroke};
use rustdar_overlays::types::ScreenPoint;

/// Convert an `egui::Pos2` to a framework-agnostic `ScreenPoint`.
#[inline]
pub(crate) fn to_screen(p: Pos2) -> ScreenPoint {
    ScreenPoint::new(p.x, p.y)
}

/// Convert a `ScreenPoint` back to an `egui::Pos2`.
#[inline]
pub(crate) fn to_pos2(p: ScreenPoint) -> Pos2 {
    Pos2::new(p.x, p.y)
}

/// Convert a slice of `Pos2` to a `Vec<ScreenPoint>`.
#[inline]
pub(crate) fn slice_to_screen(pts: &[Pos2]) -> Vec<ScreenPoint> {
    pts.iter().copied().map(to_screen).collect()
}

/// Generate dashed line shapes from `p1` to `p2`.
pub(crate) fn dashed_line_shapes(p1: Pos2, p2: Pos2, stroke: Stroke) -> Vec<Shape> {
    const DASH: f32 = 4.0;
    const GAP: f32 = 4.0;
    let dx = p2.x - p1.x;
    let dy = p2.y - p1.y;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1.0 {
        return Vec::new();
    }
    let nx = dx / len;
    let ny = dy / len;
    let mut shapes = Vec::new();
    let mut t = 0.0_f32;
    while t < len {
        let end = (t + DASH).min(len);
        shapes.push(Shape::line_segment(
            [
                Pos2::new(p1.x + nx * t, p1.y + ny * t),
                Pos2::new(p1.x + nx * end, p1.y + ny * end),
            ],
            stroke,
        ));
        t = end + GAP;
    }
    shapes
}
