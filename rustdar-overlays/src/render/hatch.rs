use tiny_skia::{
    Color, FillRule, LineCap, Mask, Paint, PathBuilder, Pixmap, Stroke, StrokeDash, Transform,
};

use crate::render::rasterize::{MercatorBounds, build_polygon_path, strip_closing_dup};
use crate::types::{HatchPattern, OverlayFeature};

/// Pixels.
const HATCH_SPACING: f32 = 10.0;

fn draw_hatch_lines_clipped(
    pixmap: &mut Pixmap,
    clip: &Mask,
    pattern: HatchPattern,
    hatch_color: [u8; 4],
    poly_path: &tiny_skia::Path,
) {
    let bounds = poly_path.bounds();

    match pattern {
        HatchPattern::Cig1 => {
            draw_directional_hatch(pixmap, clip, 45.0, true, hatch_color, bounds);
        }
        HatchPattern::Cig2 => {
            draw_directional_hatch(pixmap, clip, 135.0, false, hatch_color, bounds);
        }
        HatchPattern::Cig3 => {
            draw_directional_hatch(pixmap, clip, 45.0, false, hatch_color, bounds);
            draw_directional_hatch(pixmap, clip, 135.0, false, hatch_color, bounds);
        }
        HatchPattern::None => {}
    }
}

/// `angle_degrees` is math convention; `dir_y` is negated for screen y-down,
/// so 45° draws as a forward slash and 135° as a backslash.
fn draw_directional_hatch(
    pixmap: &mut Pixmap,
    clip: &Mask,
    angle_degrees: f32,
    dotted: bool,
    color: [u8; 4],
    bounds: tiny_skia::Rect,
) {
    let min_x = bounds.left();
    let min_y = bounds.top();
    let max_x = bounds.right();
    let max_y = bounds.bottom();
    let angle_rad = angle_degrees.to_radians();
    let dir_x = angle_rad.cos();
    let dir_y = -angle_rad.sin();
    let norm_x = -dir_y;
    let norm_y = dir_x;

    let corners = [
        (min_x, min_y),
        (max_x, min_y),
        (min_x, max_y),
        (max_x, max_y),
    ];

    let (mut min_proj, mut max_proj) = (f32::MAX, f32::MIN);
    let (mut min_dir, mut max_dir) = (f32::MAX, f32::MIN);
    for &(cx, cy) in &corners {
        let np = cx * norm_x + cy * norm_y;
        min_proj = min_proj.min(np);
        max_proj = max_proj.max(np);
        let dp = cx * dir_x + cy * dir_y;
        min_dir = min_dir.min(dp);
        max_dir = max_dir.max(dp);
    }
    let half_len = (max_dir - min_dir) * 0.5 + 10.0;
    let center = (min_dir + max_dir) * 0.5;

    let mut paint = Paint::default();
    paint.set_color(Color::from_rgba8(color[0], color[1], color[2], color[3]));
    paint.anti_alias = true;

    let mut stroke = Stroke {
        width: if dotted { 1.5 } else { 1.0 },
        line_cap: LineCap::Butt,
        ..Stroke::default()
    };
    if dotted {
        stroke.dash = StrokeDash::new(vec![4.0, 4.0], 0.0);
    }

    let mut t = min_proj;
    while t <= max_proj {
        let cx = norm_x * t + dir_x * center;
        let cy = norm_y * t + dir_y * center;
        let x1 = cx - dir_x * half_len;
        let y1 = cy - dir_y * half_len;
        let x2 = cx + dir_x * half_len;
        let y2 = cy + dir_y * half_len;

        let mut pb = PathBuilder::new();
        pb.move_to(x1, y1);
        pb.line_to(x2, y2);
        if let Some(line_path) = pb.finish() {
            pixmap.stroke_path(
                &line_path,
                &paint,
                &stroke,
                Transform::identity(),
                Some(clip),
            );
        }

        t += HATCH_SPACING;
    }
}

/// A higher CIG level's area is excluded from lower levels' hatching, so
/// nested outlook areas do not accumulate overlapping line sets.
pub(crate) fn draw_hatch_pass(
    pixmap: &mut Pixmap,
    features: &[OverlayFeature],
    mb: &MercatorBounds,
    w: f32,
    h: f32,
    hatch_color: [u8; 4],
) {
    struct HatchedPolygon {
        feature_idx: usize,
        path: tiny_skia::Path,
    }

    let mut cig2_pts: Vec<Vec<(f32, f32)>> = Vec::new();
    let mut cig3_pts: Vec<Vec<(f32, f32)>> = Vec::new();
    let mut all_hatched: Vec<HatchedPolygon> = Vec::new();

    for (idx, feature) in features.iter().enumerate() {
        if feature.hatch == HatchPattern::None {
            continue;
        }
        for polygon in &feature.polygons {
            let Some(exterior) = polygon.first() else {
                continue;
            };
            let ring = strip_closing_dup(exterior);
            let pts: Vec<(f32, f32)> = ring
                .iter()
                .map(|&(lat, lon)| mb.project(lat, lon, w, h))
                .collect();
            if let Some(path) = build_polygon_path(&pts) {
                match feature.hatch {
                    HatchPattern::Cig2 => cig2_pts.push(pts),
                    HatchPattern::Cig3 => cig3_pts.push(pts),
                    _ => {}
                }
                all_hatched.push(HatchedPolygon {
                    feature_idx: idx,
                    path,
                });
            }
        }
    }

    let pw = pixmap.width();
    let ph = pixmap.height();

    for hp in &all_hatched {
        let hatch = features[hp.feature_idx].hatch;

        let exclusion_pts: Vec<&[(f32, f32)]> = match hatch {
            HatchPattern::Cig1 => cig2_pts
                .iter()
                .chain(cig3_pts.iter())
                .map(|v| v.as_slice())
                .collect(),
            HatchPattern::Cig2 => cig3_pts.iter().map(|v| v.as_slice()).collect(),
            _ => Vec::new(),
        };

        let Some(mask) = hatch_mask_with_exclusions(pw, ph, &hp.path, &exclusion_pts) else {
            continue;
        };

        draw_hatch_lines_clipped(pixmap, &mask, hatch, hatch_color, &hp.path);
    }
}

/// Coverage mask for one hatched ring: the ring's interior minus the union of
/// every exclusion ring's interior.
///
/// Subtraction, not parity: the previous implementation put the hatched ring
/// and all exclusion rings into one path and filled it `EvenOdd`, which is
/// correct only when a point falls inside at most one exclusion ring. In the
/// nested case this pass exists for (CIG1 ⊃ CIG2 ⊃ CIG3, commit b7f2ebd) a
/// point inside all three crosses three rings — odd parity — so CIG1's
/// hatching came back inside CIG3; and a *disjoint* exclusion ring inside the
/// bbox got the lower level's hatching drawn outside the hatched polygon
/// entirely (one crossing, odd). Union-then-subtract holds for any nesting
/// depth and any disjoint arrangement.
fn hatch_mask_with_exclusions(
    pw: u32,
    ph: u32,
    hatched: &tiny_skia::Path,
    exclusions: &[&[(f32, f32)]],
) -> Option<Mask> {
    let mut mask = Mask::new(pw, ph)?;
    mask.fill_path(hatched, FillRule::Winding, false, Transform::identity());

    if exclusions.is_empty() {
        return Some(mask);
    }

    // One `fill_path` per ring: successive fills union onto the mask, and a
    // ring filled alone covers its interior whichever way it winds. A single
    // `Winding` fill over all rings at once would cancel to zero wherever two
    // rings of opposite orientation overlap — SPC does not promise consistent
    // ring orientation — and `EvenOdd` is the parity bug this replaces.
    let mut excl = Mask::new(pw, ph)?;
    for pts in exclusions {
        if let Some(path) = build_polygon_path(pts) {
            excl.fill_path(&path, FillRule::Winding, false, Transform::identity());
        }
    }

    // mask := mask ∧ ¬excl. Coverage is binary (anti-aliasing is off), so
    // `min` against the inverted union is an exact subtraction.
    excl.invert();
    for (m, e) in mask.data_mut().iter_mut().zip(excl.data()) {
        *m = (*m).min(*e);
    }
    Some(mask)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Axis-aligned square ring centred on `(cx, cy)`, counter-clockwise in
    /// screen space.
    fn square(cx: f32, cy: f32, half: f32) -> Vec<(f32, f32)> {
        vec![
            (cx - half, cy - half),
            (cx + half, cy - half),
            (cx + half, cy + half),
            (cx - half, cy + half),
        ]
    }

    /// Mask coverage at pixel `(x, y)`; the mask is one byte per pixel,
    /// row-major, and with anti-aliasing off every value is 0 or 255.
    fn coverage(mask: &Mask, x: u32, y: u32) -> u8 {
        mask.data()[(y * mask.width() + x) as usize]
    }

    fn mask_for(hatched_pts: &[(f32, f32)], exclusions: &[&[(f32, f32)]]) -> Mask {
        let hatched = build_polygon_path(hatched_pts).expect("fixture ring must build");
        hatch_mask_with_exclusions(100, 100, &hatched, exclusions).expect("mask must build")
    }

    /// The nested case the pass exists for: CIG1 ⊃ CIG2 ⊃ CIG3. A point inside
    /// all three rings crossed *three* of them, so the old single `EvenOdd`
    /// fill flipped back to "filled" and drew CIG1's hatching inside CIG3
    /// again.
    #[test]
    fn cig1s_mask_excludes_a_point_inside_doubly_nested_exclusions() {
        let cig1 = square(50.0, 50.0, 40.0);
        let cig2 = square(50.0, 50.0, 25.0);
        let cig3 = square(50.0, 50.0, 10.0);
        let mask = mask_for(&cig1, &[&cig2, &cig3]);

        assert_eq!(
            coverage(&mask, 50, 50),
            0,
            "inside CIG1 ∧ CIG2 ∧ CIG3: three crossings made even-odd parity \
             re-fill this point"
        );
        assert_eq!(
            coverage(&mask, 50, 32),
            0,
            "inside CIG2 only: still excluded"
        );
        // Controls: the exclusion must not eat the ring it is cut from.
        assert_eq!(coverage(&mask, 50, 15), 255, "inside CIG1 only: hatched");
        assert_eq!(coverage(&mask, 50, 5), 0, "outside CIG1: never hatched");
    }

    /// The other parity failure: an exclusion ring disjoint from the hatched
    /// polygon but inside its bbox crossed exactly one ring — odd — so the old
    /// fill hatched *outside* the hatched polygon entirely.
    #[test]
    fn a_disjoint_exclusion_ring_does_not_pick_up_the_hatching() {
        let hatched = square(30.0, 50.0, 15.0);
        let disjoint = square(70.0, 50.0, 10.0);
        let mask = mask_for(&hatched, &[&disjoint]);

        assert_eq!(
            coverage(&mask, 70, 50),
            0,
            "inside the disjoint exclusion ring, outside the hatched polygon: \
             one crossing made even-odd parity fill it"
        );
        assert_eq!(
            coverage(&mask, 30, 50),
            255,
            "the hatched polygon itself still hatches"
        );
    }

    /// SPC does not promise consistent ring orientation, so two overlapping
    /// exclusion rings winding opposite ways must still both exclude — a
    /// single `Winding` fill over both at once cancels to zero where they
    /// overlap.
    #[test]
    fn exclusion_survives_opposite_winding_directions() {
        let cig1 = square(50.0, 50.0, 40.0);
        let cig2 = square(50.0, 50.0, 25.0);
        let mut cig3 = square(50.0, 50.0, 10.0);
        cig3.reverse(); // clockwise, opposite to `square`'s order

        let mask = mask_for(&cig1, &[&cig2, &cig3]);
        assert_eq!(
            coverage(&mask, 50, 50),
            0,
            "a reversed inner ring must not cancel the outer ring's exclusion"
        );
        assert_eq!(
            coverage(&mask, 50, 15),
            255,
            "control: CIG1-only area still hatches"
        );
    }
}
