//! Thin egui-specific geometry helpers and `Pos2` ↔ `ScreenPoint` bridging.
//!
//! Pure geometry algorithms (point-in-polygon, AABB, line clipping) have moved
//! to `rustdar_overlays::render::geo`.  This module retains only the helpers
//! that produce egui drawing primitives or convert between coordinate types.

use egui::Pos2;
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
