//! Cached overlay geometry for efficient per-frame rendering.
//!
//! Overlay polygons (NWS alerts, SPC outlooks, mesoscale discussions) require
//! projection, clipping, simplification, and ear-clip triangulation before they
//! can be drawn. Previously all of this ran **every frame** — an O(n²)
//! triangulation per polygon at 60 fps during severe weather caused heavy lag.
//!
//! This module caches the projected + triangulated screen-space meshes, keyed
//! on viewport state (zoom + quantised centre). Triangulation indices are
//! pre-computed in geo-coordinates at fetch time (topology is projection-
//! invariant), so only vertex projection needs to run on cache misses.

use egui::{Mesh, Pos2, Rect, Shape, Stroke};
use rustdar_overlays::types::{GeoBounds, HatchPattern, OverlayFeature};
use walkers::Projector;

use crate::geo;

use crate::hatch::generate_hatch_lines;

/// A viewport fingerprint used to decide whether cached geometry can be reused.
///
/// The cache is keyed on zoom level, exact map position, and screen size.
/// Any change causes a full rebuild, which is cheap because O(n²)
/// triangulation is pre-computed at fetch time — only O(n) vertex
/// projection runs on rebuild.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewportKey {
    /// Rounded integer zoom level.
    pub zoom: u8,
    /// Screen-space X of the geo-origin (0,0) — exact, not quantised.
    /// Used to detect any amount of panning.
    pub origin_x: i32,
    /// Screen-space Y of the geo-origin.
    pub origin_y: i32,
    /// Width of the screen rect (quantised to 8px grid) to detect resizes.
    pub w: i32,
    /// Height of the screen rect (quantised to 8px grid) to detect resizes.
    pub h: i32,
}

impl ViewportKey {
    pub fn from_projector_and_rect(projector: &Projector, zoom: f64, rect: Rect) -> Self {
        // Project the map origin to get a stable reference point that
        // changes 1:1 with any panning.
        let origin = projector.project(walkers::lat_lon(0.0, 0.0)).to_pos2();
        Self {
            zoom: zoom.round() as u8,
            origin_x: origin.x.round() as i32,
            origin_y: origin.y.round() as i32,
            w: (rect.width() / 8.0).round() as i32,
            h: (rect.height() / 8.0).round() as i32,
        }
    }
}

/// Cached screen-space representation of a single polygon ring.
pub struct CachedPolygon {
    /// Screen-space vertices (projected + optionally clipped).
    pub screen_pts: Vec<Pos2>,
    /// Triangle index buffer — either from pre-computed geo-space
    /// triangulation (preferred) or re-triangulated in screen space.
    pub tri_indices: Vec<u32>,
    /// Bounding rect on screen.
    pub poly_rect: Rect,
    /// Pre-computed hatch line segments: (start, end, is_dotted).
    pub hatch_lines: Vec<(Pos2, Pos2, bool)>,
}

/// Cached set of all polygons for one overlay feature.
pub struct CachedFeature {
    pub polygons: Vec<CachedPolygon>,
}

/// Cached geometry for an entire overlay layer (e.g. all NWS warnings,
/// one SPC product, or all mesoscale discussions).
pub struct OverlayLayerCache {
    /// Viewport the cache was built for.
    pub viewport_key: ViewportKey,
    /// Cached features, indexed parallel to the source data.
    pub features: Vec<CachedFeature>,
    /// A monotonically-increasing generation counter. Incremented
    /// whenever the source data is replaced (e.g. new NWS alerts arrive).
    pub data_generation: u64,
}

impl OverlayLayerCache {
    pub fn new() -> Self {
        Self {
            viewport_key: ViewportKey {
                zoom: 0,
                origin_x: 0,
                origin_y: 0,
                w: 0,
                h: 0,
            },
            features: Vec::new(),
            data_generation: 0,
        }
    }

    /// Returns `true` if the cache is valid for the current viewport and data.
    pub fn is_valid(&self, key: &ViewportKey, data_gen: u64) -> bool {
        self.data_generation == data_gen && self.viewport_key == *key
    }

    /// Invalidate by bumping the expected data generation.
    pub fn invalidate(&mut self) {
        self.data_generation = self.data_generation.wrapping_add(1);
    }
}

/// Build cached geometry for a list of `OverlayFeature`s.
///
/// This is the expensive operation that runs on cache misses. It:
/// 1. Culls features by geo-AABB vs viewport bounds
/// 2. Projects vertices to screen coordinates
/// 3. Re-uses pre-computed triangulation indices from geo-space
/// 4. Generates hatch line segments
pub fn build_cached_features(
    features: &[OverlayFeature],
    projector: &Projector,
    screen_rect: Rect,
    include_hatch: bool,
    dark_theme: bool,
) -> Vec<CachedFeature> {
    // Compute geo bounds of the visible viewport by unprojecting screen corners
    let nw = projector.unproject(egui::vec2(screen_rect.left(), screen_rect.top()));
    let se = projector.unproject(egui::vec2(screen_rect.right(), screen_rect.bottom()));
    let viewport_geo = GeoBounds {
        min_lat: nw.y().min(se.y()),
        max_lat: nw.y().max(se.y()),
        min_lon: nw.x().min(se.x()),
        max_lon: nw.x().max(se.x()),
    };

    // Pass 1: build all polygon geometry (triangulation, projection) without hatch
    let mut cached: Vec<CachedFeature> = features
        .iter()
        .map(|feature| {
            // Early geo-AABB cull
            if let Some(ref bounds) = feature.geo_bounds {
                if !bounds.intersects(&viewport_geo) {
                    return CachedFeature {
                        polygons: Vec::new(),
                    };
                }
            }

            let cached_polys = feature
                .polygons
                .iter()
                .enumerate()
                .filter_map(|(poly_idx, polygon)| {
                    build_cached_polygon(
                        polygon,
                        feature.triangulations.get(poly_idx).and_then(|t| t.as_ref()),
                        projector,
                        screen_rect,
                    )
                })
                .collect();

            CachedFeature {
                polygons: cached_polys,
            }
        })
        .collect();

    if !include_hatch {
        return cached;
    }

    // Pass 2: generate hatch lines.
    // For each CIG level, collect screen-space polygons from *higher* CIG
    // features to use as exclusion zones so lower-severity hatching doesn't
    // show through higher-severity regions.

    // Collect all screen-space polygon vertex lists grouped by CIG level
    let mut cig2_polys: Vec<Vec<Pos2>> = Vec::new();
    let mut cig3_polys: Vec<Vec<Pos2>> = Vec::new();
    for (feat_idx, feature) in features.iter().enumerate() {
        match feature.hatch {
            HatchPattern::Cig2 => {
                for cp in &cached[feat_idx].polygons {
                    if cp.screen_pts.len() >= 3 {
                        cig2_polys.push(cp.screen_pts.clone());
                    }
                }
            }
            HatchPattern::Cig3 => {
                for cp in &cached[feat_idx].polygons {
                    if cp.screen_pts.len() >= 3 {
                        cig3_polys.push(cp.screen_pts.clone());
                    }
                }
            }
            _ => {}
        }
    }

    // Now generate hatch for each feature with appropriate exclusions
    for (feat_idx, feature) in features.iter().enumerate() {
        if feature.hatch == HatchPattern::None {
            continue;
        }

        // Build the exclusion list: higher-CIG polygons that should mask this feature's hatch
        let exclusions: Vec<&[Pos2]> = match feature.hatch {
            HatchPattern::Cig1 => {
                // CIG1 is masked by CIG2 and CIG3
                cig2_polys.iter().map(|v| v.as_slice())
                    .chain(cig3_polys.iter().map(|v| v.as_slice()))
                    .collect()
            }
            HatchPattern::Cig2 => {
                // CIG2 is masked by CIG3
                cig3_polys.iter().map(|v| v.as_slice()).collect()
            }
            _ => Vec::new(), // CIG3 is the highest — no masking needed
        };

        for cp in &mut cached[feat_idx].polygons {
            if cp.screen_pts.len() >= 3 {
                cp.hatch_lines = generate_hatch_lines(
                    &cp.screen_pts,
                    feature.hatch,
                    screen_rect,
                    dark_theme,
                    &exclusions,
                );
            }
        }
    }

    cached
}

/// Build cached geometry for a single polygon exterior ring (without hatch — hatch is added in pass 2).
fn build_cached_polygon(
    polygon: &[Vec<(f64, f64)>],
    precomputed_tri: Option<&rustdar_overlays::types::PrecomputedTriangulation>,
    projector: &Projector,
    screen_rect: Rect,
) -> Option<CachedPolygon> {
    let exterior = polygon.first()?;
    if exterior.len() < 3 {
        return None;
    }

    // Strip the GeoJSON closing duplicate (last == first)
    let ring = if exterior.len() > 3 && exterior.first() == exterior.last() {
        &exterior[..exterior.len() - 1]
    } else {
        exterior.as_slice()
    };
    if ring.len() < 3 {
        return None;
    }

    // Project all geo vertices to screen coordinates
    let projected: Vec<Pos2> = ring
        .iter()
        .filter_map(|&(lat, lon)| {
            let p = projector.project(walkers::lat_lon(lat, lon)).to_pos2();
            (p.x.is_finite() && p.y.is_finite() && p.x.abs() < 1e5 && p.y.abs() < 1e5)
                .then_some(p)
        })
        .collect();
    if projected.len() < 3 {
        return None;
    }

    // Compute AABB
    let (min_x, min_y, max_x, max_y) = geo::aabb(&projected);
    let poly_rect = Rect::from_min_max(egui::pos2(min_x, min_y), egui::pos2(max_x, max_y));

    // Cull polygons whose screen AABB doesn't intersect the viewport
    // (with a generous margin for stroke width)
    if !screen_rect.expand(20.0).intersects(poly_rect) {
        return None;
    }

    // Skip polygons too small to meaningfully render
    if (max_x - min_x) < 2.0 && (max_y - min_y) < 2.0 {
        return None;
    }

    // Determine triangle indices.
    // If we have pre-computed triangulation AND all vertices survived projection
    // (same count as ring), re-use the pre-computed indices directly.
    let tri_indices = if let Some(tri) = precomputed_tri {
        if projected.len() == ring.len() {
            // Pre-computed indices map directly to the projected vertices
            tri.indices.clone()
        } else {
            // Vertex count changed (some filtered by is_finite check) — re-triangulate
            triangulate_screen(&projected)
        }
    } else {
        triangulate_screen(&projected)
    };

    Some(CachedPolygon {
        screen_pts: projected,
        tri_indices,
        poly_rect,
        hatch_lines: Vec::new(),
    })
}

/// Ear-clip triangulate in screen coordinates (fallback when pre-computed indices can't be used).
fn triangulate_screen(pts: &[Pos2]) -> Vec<u32> {
    let coords: Vec<f64> = pts.iter().flat_map(|p| [p.x as f64, p.y as f64]).collect();
    earcutr::earcut(&coords, &[], 2)
        .unwrap_or_default()
        .into_iter()
        .map(|i| i as u32)
        .collect()
}

// ── Drawing from cache ───────────────────────────────────────────────────

/// Draw all cached features for a layer as filled polygons with stroke outlines.
///
/// Batches all geometry into a single `egui::Mesh` per call and accumulates
/// stroke segments, dramatically reducing per-frame painter overhead.
pub fn draw_cached_features(
    painter: &egui::Painter,
    cached: &[CachedFeature],
    source_features: &[OverlayFeature],
    screen_rect: Rect,
    hatch_color: egui::Color32,
) {
    let mut fill_mesh = Mesh::default();
    let mut strokes: Vec<Shape> = Vec::new();
    let mut hatch_strokes: Vec<Shape> = Vec::new();

    for (feat_idx, cached_feat) in cached.iter().enumerate() {
        let src = match source_features.get(feat_idx) {
            Some(s) => s,
            None => continue,
        };

        let [r, g, b, a] = src.fill_rgba;
        let fill = egui::Color32::from_rgba_unmultiplied(r, g, b, a);
        let [sr, sg, sb, sa] = src.stroke_rgba;
        let stroke_color = egui::Color32::from_rgba_unmultiplied(sr, sg, sb, sa);

        for cached_poly in &cached_feat.polygons {
            if !screen_rect.intersects(cached_poly.poly_rect) {
                continue;
            }

            // Append filled triangles to the batched mesh
            if !cached_poly.tri_indices.is_empty() {
                let base = fill_mesh.vertices.len() as u32;
                for pt in &cached_poly.screen_pts {
                    fill_mesh.vertices.push(egui::epaint::Vertex {
                        pos: *pt,
                        uv: egui::epaint::WHITE_UV,
                        color: fill,
                    });
                }
                for &idx in &cached_poly.tri_indices {
                    fill_mesh.indices.push(base + idx);
                }
            }

            // Accumulate stroke segments
            if sa > 0 {
                let stroke = Stroke::new(1.5, stroke_color);
                let pts = &cached_poly.screen_pts;
                for i in 0..pts.len() {
                    let j = (i + 1) % pts.len();
                    strokes.push(Shape::line_segment([pts[i], pts[j]], stroke));
                }
            }

            // Accumulate hatch lines
            for &(p1, p2, dotted) in &cached_poly.hatch_lines {
                if dotted {
                    let dash_shapes = geo::dashed_line_shapes(
                        p1,
                        p2,
                        Stroke::new(1.5, hatch_color),
                    );
                    hatch_strokes.extend(dash_shapes);
                } else {
                    hatch_strokes.push(Shape::line_segment(
                        [p1, p2],
                        Stroke::new(1.5, hatch_color),
                    ));
                }
            }
        }
    }

    // Emit batched geometry
    if !fill_mesh.vertices.is_empty() {
        painter.add(Shape::mesh(fill_mesh));
    }
    painter.extend(strokes);
    painter.extend(hatch_strokes);
}

/// Draw cached features for NWS alerts / SPC discussions that support click detection.
///
/// Returns `Some(source_index)` if a polygon was clicked.
pub fn draw_cached_features_clickable(
    ui: &egui::Ui,
    cached: &[CachedFeature],
    source_features_len: usize,
    fill_colors: &[[u8; 4]],
    stroke_colors: &[[u8; 4]],
    stroke_width: f32,
    screen_rect: Rect,
    hatch_color: Option<egui::Color32>,
    // Maps from cached feature index → source alert/discussion index
    cached_to_source: &[usize],
) -> Option<usize> {
    let painter = ui.painter();
    let mut fill_mesh = Mesh::default();
    let mut strokes: Vec<Shape> = Vec::new();
    let mut hatch_strokes: Vec<Shape> = Vec::new();
    let mut clicked_index: Option<usize> = None;

    for (feat_idx, cached_feat) in cached.iter().enumerate() {
        let source_idx = cached_to_source.get(feat_idx).copied().unwrap_or(feat_idx);
        if source_idx >= source_features_len {
            continue;
        }

        let fill_rgba = fill_colors.get(feat_idx).copied().unwrap_or([0; 4]);
        let stroke_rgba = stroke_colors.get(feat_idx).copied().unwrap_or([0; 4]);
        let [r, g, b, a] = fill_rgba;
        let fill = egui::Color32::from_rgba_unmultiplied(r, g, b, a);
        let [sr, sg, sb, sa] = stroke_rgba;
        let stroke_color = egui::Color32::from_rgba_unmultiplied(sr, sg, sb, sa);

        for cached_poly in &cached_feat.polygons {
            if !screen_rect.intersects(cached_poly.poly_rect) {
                continue;
            }

            // Filled triangles
            if !cached_poly.tri_indices.is_empty() {
                let base = fill_mesh.vertices.len() as u32;
                for pt in &cached_poly.screen_pts {
                    fill_mesh.vertices.push(egui::epaint::Vertex {
                        pos: *pt,
                        uv: egui::epaint::WHITE_UV,
                        color: fill,
                    });
                }
                for &idx in &cached_poly.tri_indices {
                    fill_mesh.indices.push(base + idx);
                }
            }

            // Stroke
            if sa > 0 {
                let stroke = Stroke::new(stroke_width, stroke_color);
                let pts = &cached_poly.screen_pts;
                for i in 0..pts.len() {
                    let j = (i + 1) % pts.len();
                    strokes.push(Shape::line_segment([pts[i], pts[j]], stroke));
                }
            }

            // Hatch
            if let Some(hc) = hatch_color {
                for &(p1, p2, dotted) in &cached_poly.hatch_lines {
                    if dotted {
                        hatch_strokes.extend(geo::dashed_line_shapes(
                            p1,
                            p2,
                            Stroke::new(1.5, hc),
                        ));
                    } else {
                        hatch_strokes.push(Shape::line_segment(
                            [p1, p2],
                            Stroke::new(1.5, hc),
                        ));
                    }
                }
            }

            // Click detection (only on the first click found)
            if clicked_index.is_none() {
                let clicked = ui.ctx().input(|i| {
                    i.pointer.any_click()
                        && i.pointer.interact_pos().is_some_and(|p| {
                            cached_poly.poly_rect.contains(p)
                                && geo::point_in_polygon(p, &cached_poly.screen_pts)
                        })
                });
                if clicked {
                    clicked_index = Some(source_idx);
                }
            }
        }
    }

    if !fill_mesh.vertices.is_empty() {
        painter.add(Shape::mesh(fill_mesh));
    }
    painter.extend(strokes);
    painter.extend(hatch_strokes);

    clicked_index
}
