use tiny_skia::{
    Color, FillRule, LineCap, Mask, Paint, PathBuilder, Pixmap, Stroke, StrokeDash, Transform,
};

use crate::render::rasterize::{
    MercatorBounds, ProjectedPolygon, build_filled_polygon_path, project_polygon,
};
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
///
/// The polygon this pass hatches is the same polygon `draw_feature`
/// fills, holes and all — both go through [`project_polygon`] and
/// [`build_filled_polygon_path`] so that they cannot read one shape as two.
/// They did: the fill honoured interior rings while this pass took only
/// `polygon.first()`, so a hole the fill had just cut got hatched straight
/// across.
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
        rule: FillRule,
    }

    let mut cig2: Vec<ProjectedPolygon> = Vec::new();
    let mut cig3: Vec<ProjectedPolygon> = Vec::new();
    let mut all_hatched: Vec<HatchedPolygon> = Vec::new();

    for (idx, feature) in features.iter().enumerate() {
        if feature.hatch == HatchPattern::None {
            continue;
        }
        for polygon in &feature.polygons {
            let Some(projected) = project_polygon(polygon, mb, w, h) else {
                continue;
            };
            if let Some((path, rule)) =
                build_filled_polygon_path(&projected.exterior, &projected.holes)
            {
                all_hatched.push(HatchedPolygon {
                    feature_idx: idx,
                    path,
                    rule,
                });
                match feature.hatch {
                    HatchPattern::Cig2 => cig2.push(projected),
                    HatchPattern::Cig3 => cig3.push(projected),
                    _ => {}
                }
            }
        }
    }

    let pw = pixmap.width();
    let ph = pixmap.height();

    for hp in &all_hatched {
        let hatch = features[hp.feature_idx].hatch;

        let exclusions: Vec<&ProjectedPolygon> = match hatch {
            HatchPattern::Cig1 => cig2.iter().chain(cig3.iter()).collect(),
            HatchPattern::Cig2 => cig3.iter().collect(),
            _ => Vec::new(),
        };

        let Some(mask) = hatch_mask_with_exclusions(pw, ph, &hp.path, hp.rule, &exclusions) else {
            continue;
        };

        draw_hatch_lines_clipped(pixmap, &mask, hatch, hatch_color, &hp.path);
    }
}

/// Coverage mask for one hatched polygon: the polygon's interior — holes
/// already cut out of it by `rule` — minus the union of every exclusion
/// polygon's interior, holes cut out of those too.
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
///
/// `rule` comes from [`build_filled_polygon_path`], which is also what chose
/// the path: a hole-free polygon still fills `Winding`, exactly as this
/// function has always filled it.
fn hatch_mask_with_exclusions(
    pw: u32,
    ph: u32,
    hatched: &tiny_skia::Path,
    rule: FillRule,
    exclusions: &[&ProjectedPolygon],
) -> Option<Mask> {
    let mut mask = Mask::new(pw, ph)?;
    mask.fill_path(hatched, rule, false, Transform::identity());

    if exclusions.is_empty() {
        return Some(mask);
    }

    // One `fill_path` per polygon: successive fills union onto the mask, and a
    // polygon filled alone covers its interior whichever way its rings wind. A
    // single `Winding` fill over all of them at once would cancel to zero
    // wherever two rings of opposite orientation overlap — SPC does not promise
    // consistent ring orientation — and `EvenOdd` over all of them at once is
    // the parity bug this replaces. Unioning is also what makes an exclusion's
    // *own* hole work: the fill never writes the hole, so whatever another
    // exclusion put there survives.
    let mut excl = Mask::new(pw, ph)?;
    for poly in exclusions {
        if let Some((path, poly_rule)) = build_filled_polygon_path(&poly.exterior, &poly.holes) {
            excl.fill_path(&path, poly_rule, false, Transform::identity());
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

    /// A hole-free projected polygon, the shape most of these fixtures need.
    fn solid(pts: Vec<(f32, f32)>) -> ProjectedPolygon {
        ProjectedPolygon {
            exterior: pts,
            holes: Vec::new(),
        }
    }

    fn mask_for(hatched: &ProjectedPolygon, exclusions: &[&ProjectedPolygon]) -> Mask {
        let (path, rule) = build_filled_polygon_path(&hatched.exterior, &hatched.holes)
            .expect("fixture polygon must build");
        hatch_mask_with_exclusions(100, 100, &path, rule, exclusions).expect("mask must build")
    }

    /// The nested case the pass exists for: CIG1 ⊃ CIG2 ⊃ CIG3. A point inside
    /// all three rings crossed *three* of them, so the old single `EvenOdd`
    /// fill flipped back to "filled" and drew CIG1's hatching inside CIG3
    /// again.
    #[test]
    fn cig1s_mask_excludes_a_point_inside_doubly_nested_exclusions() {
        let cig1 = solid(square(50.0, 50.0, 40.0));
        let cig2 = solid(square(50.0, 50.0, 25.0));
        let cig3 = solid(square(50.0, 50.0, 10.0));
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
        let hatched = solid(square(30.0, 50.0, 15.0));
        let disjoint = solid(square(70.0, 50.0, 10.0));
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
        let cig1 = solid(square(50.0, 50.0, 40.0));
        let cig2 = solid(square(50.0, 50.0, 25.0));
        let mut cig3_pts = square(50.0, 50.0, 10.0);
        cig3_pts.reverse(); // clockwise, opposite to `square`'s order
        let cig3 = solid(cig3_pts);

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

    /// The disagreement honouring holes in the fill would otherwise have
    /// opened: `draw_feature` cuts the hole, and this pass used to hatch
    /// straight across it, so the cut-out came back as a patch of bare lines.
    #[test]
    fn a_holed_polygon_is_not_hatched_inside_its_hole() {
        let hatched = ProjectedPolygon {
            exterior: square(50.0, 50.0, 40.0),
            holes: vec![square(50.0, 50.0, 15.0)],
        };
        let mask = mask_for(&hatched, &[]);

        assert_eq!(
            coverage(&mask, 50, 50),
            0,
            "the hatch mask covers the polygon's hole: this pass is reading \
             only the exterior ring again while the fill reads all of them"
        );
        assert_eq!(
            coverage(&mask, 50, 20),
            255,
            "control: the solid ring between exterior and hole still hatches"
        );
        assert_eq!(coverage(&mask, 50, 5), 0, "control: outside the exterior");
    }

    /// **The whole path, on SPC's own bytes: feed → parse → raster → ink.**
    ///
    /// Every other test in this module drives `hatch_mask_with_exclusions`
    /// directly with synthetic squares, which proves the geometry is right and
    /// proves nothing about whether anything ever calls it. That question is
    /// worth a test of its own, because it was once answered wrongly in both
    /// directions: an audit concluded the subsystem was unreachable in
    /// production because SPC's live product that day had no significant-severe
    /// area in it at all.
    ///
    /// So this one starts from a real archived product
    /// (`day1otlk_20260425_1300_hail.lyr.geojson`, saved verbatim), runs it
    /// through the parser and the public rasterizer the handler calls, and
    /// asserts hatch-coloured pixels landed on the texture. The hatch colour
    /// is passed in by the caller — it is theme-dependent — so a colour SPC
    /// never uses makes the ink unambiguous.
    #[test]
    fn a_real_spc_product_puts_hatch_lines_on_the_texture() {
        use crate::spc::outlook::{OutlookDay, OutlookProduct, parse_geojson};
        use crate::types::{GeoBounds, HatchPattern};

        let raw = include_str!("../../testdata/day1otlk_20260425_1300_hail.lyr.geojson");
        let json: serde_json::Value = serde_json::from_str(raw).expect("SPC's own JSON");
        let outlook = parse_geojson(&json, OutlookDay::Day1, OutlookProduct::Hail)
            .expect("a real product must parse");
        assert!(
            outlook
                .features
                .iter()
                .any(|f| f.hatch != HatchPattern::None),
            "premise: this product carries a significant-severe area",
        );

        // The product's own extent, so the hatched area is on the texture.
        let (mut min_lat, mut max_lat) = (f64::MAX, f64::MIN);
        let (mut min_lon, mut max_lon) = (f64::MAX, f64::MIN);
        for feature in &outlook.features {
            for polygon in &feature.polygons {
                for ring in polygon {
                    for &(lat, lon) in ring {
                        min_lat = min_lat.min(lat);
                        max_lat = max_lat.max(lat);
                        min_lon = min_lon.min(lon);
                        max_lon = max_lon.max(lon);
                    }
                }
            }
        }
        let bounds = GeoBounds {
            min_lat,
            max_lat,
            min_lon,
            max_lon,
        };

        // Pure blue at full alpha: SPC's palette here is greys, greens,
        // yellows and pinks, so any blue pixel came from the hatch pass.
        let hatch_color = [0u8, 0, 255, 255];
        let rgba = crate::render::rasterize::rasterize_spc_outlooks(
            &outlook.features,
            &bounds,
            512,
            512,
            hatch_color,
            1.0,
        );
        let hatch_pixels = rgba
            .chunks_exact(4)
            .filter(|p| p[2] > 200 && p[0] < 60 && p[1] < 60)
            .count();
        assert!(
            hatch_pixels > 100,
            "the hatch pass drew {hatch_pixels} pixels on a product with a \
             significant-severe area in it: nothing is reaching draw_hatch_pass",
        );
    }

    /// A hole in an *exclusion* polygon is not excluded — the lower level's
    /// hatching shows through it, because the higher level does not cover it.
    #[test]
    fn a_hole_in_an_exclusion_polygon_lets_the_lower_level_hatch_through() {
        let cig1 = solid(square(50.0, 50.0, 45.0));
        let cig2 = ProjectedPolygon {
            exterior: square(50.0, 50.0, 30.0),
            holes: vec![square(50.0, 50.0, 12.0)],
        };
        let mask = mask_for(&cig1, &[&cig2]);

        assert_eq!(
            coverage(&mask, 50, 50),
            255,
            "CIG2's hole is not CIG2's area, so CIG1 must still hatch there"
        );
        assert_eq!(
            coverage(&mask, 50, 30),
            0,
            "control: CIG2's solid part still excludes CIG1's hatching"
        );
        assert_eq!(
            coverage(&mask, 50, 10),
            255,
            "control: CIG1 outside CIG2 still hatches"
        );
    }
}
