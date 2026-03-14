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

use egui::{Mesh, Pos2, Rect, Shape, Vec2};
use rustdar_overlays::render::geo as overlay_geo;
use rustdar_overlays::render::hatch::generate_hatch_lines;
use rustdar_overlays::types::{GeoBounds, HatchPattern, OverlayFeature, ScreenPoint};
use walkers::Projector;

use crate::geo;

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

    apply_hatch_lines(&mut cached, features);

    cached
}

/// Pass 2 of cache building: generate hatch lines for CIG-hatched features.
///
/// For each CIG level, collects screen-space polygons from higher-CIG features
/// as exclusion zones so lower-severity hatching doesn't show through
/// higher-severity filled regions.
fn apply_hatch_lines(cached: &mut [CachedFeature], features: &[OverlayFeature]) {
    // Collect screen-space polygon vertex lists grouped by CIG level
    let mut cig2_polys: Vec<Vec<ScreenPoint>> = Vec::new();
    let mut cig3_polys: Vec<Vec<ScreenPoint>> = Vec::new();
    for (feat_idx, feature) in features.iter().enumerate() {
        match feature.hatch {
            HatchPattern::Cig2 => {
                for cp in &cached[feat_idx].polygons {
                    if cp.screen_pts.len() >= 3 {
                        cig2_polys.push(geo::slice_to_screen(&cp.screen_pts));
                    }
                }
            }
            HatchPattern::Cig3 => {
                for cp in &cached[feat_idx].polygons {
                    if cp.screen_pts.len() >= 3 {
                        cig3_polys.push(geo::slice_to_screen(&cp.screen_pts));
                    }
                }
            }
            _ => {}
        }
    }

    // Generate hatch for each feature with appropriate exclusions
    for (feat_idx, feature) in features.iter().enumerate() {
        if feature.hatch == HatchPattern::None {
            continue;
        }

        // Build the exclusion list: higher-CIG polygons that should mask this feature's hatch
        let exclusions: Vec<&[ScreenPoint]> = match feature.hatch {
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
                let sp = geo::slice_to_screen(&cp.screen_pts);
                cp.hatch_lines = generate_hatch_lines(&sp, feature.hatch, &exclusions)
                    .into_iter()
                    .map(|(a, b, d)| (geo::to_pos2(a), geo::to_pos2(b), d))
                    .collect();
            }
        }
    }
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

    let (projected, poly_rect) = project_and_cull(ring, projector, screen_rect)?;

    let tri_indices = resolve_tri_indices(&projected, ring.len(), precomputed_tri);

    Some(CachedPolygon {
        screen_pts: projected,
        tri_indices,
        poly_rect,
        hatch_lines: Vec::new(),
    })
}

/// Project geo coordinates to screen space and cull off-screen / too-small polygons.
fn project_and_cull(
    ring: &[(f64, f64)],
    projector: &Projector,
    screen_rect: Rect,
) -> Option<(Vec<Pos2>, Rect)> {
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

    let sp = geo::slice_to_screen(&projected);
    let (min_x, min_y, max_x, max_y) = overlay_geo::aabb(&sp);
    let poly_rect = Rect::from_min_max(egui::pos2(min_x, min_y), egui::pos2(max_x, max_y));

    if !screen_rect.expand(20.0).intersects(poly_rect) {
        return None;
    }
    if (max_x - min_x) < 2.0 && (max_y - min_y) < 2.0 {
        return None;
    }

    Some((projected, poly_rect))
}

/// Resolve triangle indices, preferring pre-computed triangulation when vertex count matches.
fn resolve_tri_indices(
    projected: &[Pos2],
    original_ring_len: usize,
    precomputed: Option<&rustdar_overlays::types::PrecomputedTriangulation>,
) -> Vec<u32> {
    if let Some(tri) = precomputed {
        if projected.len() == original_ring_len {
            return tri.indices.clone();
        }
    }
    triangulate_screen(projected)
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

/// Accumulates batched fill triangles, stroke outlines, and hatch lines into
/// GPU-ready meshes for efficient emission to an `egui::Painter`.
///
/// All geometry is tessellated into `egui::Mesh` instances rather than
/// individual `Shape::line_segment()` calls.  This reduces the per-frame
/// shape count from O(total_edges) to a constant 3 meshes (fill + stroke +
/// hatch), eliminating egui's per-shape tessellation overhead.
pub struct MeshAccumulator {
    pub fill_mesh: Mesh,
    pub stroke_mesh: Mesh,
    pub hatch_mesh: Mesh,
}

impl MeshAccumulator {
    pub fn new() -> Self {
        Self {
            fill_mesh: Mesh::default(),
            stroke_mesh: Mesh::default(),
            hatch_mesh: Mesh::default(),
        }
    }

    /// Append a single polygon's fill triangles and stroke outline.
    pub fn append_polygon(
        &mut self,
        poly: &CachedPolygon,
        fill: egui::Color32,
        stroke_color: egui::Color32,
        stroke_width: f32,
    ) {
        // Filled triangles
        if !poly.tri_indices.is_empty() {
            let base = self.fill_mesh.vertices.len() as u32;
            for pt in &poly.screen_pts {
                self.fill_mesh.vertices.push(egui::epaint::Vertex {
                    pos: *pt,
                    uv: egui::epaint::WHITE_UV,
                    color: fill,
                });
            }
            for &idx in &poly.tri_indices {
                self.fill_mesh.indices.push(base + idx);
            }
        }

        // Stroke outline — tessellated into quads in stroke_mesh
        if stroke_color.a() > 0 {
            let pts = &poly.screen_pts;
            let half = stroke_width * 0.5;
            for i in 0..pts.len() {
                let j = (i + 1) % pts.len();
                push_line_quad(&mut self.stroke_mesh, pts[i], pts[j], half, stroke_color);
            }
        }
    }

    /// Append hatch lines from a polygon.
    pub fn append_hatch(&mut self, poly: &CachedPolygon, hatch_color: egui::Color32) {
        for &(p1, p2, dotted) in &poly.hatch_lines {
            if dotted {
                push_dashed_line_quads(&mut self.hatch_mesh, p1, p2, 1.5, hatch_color);
            } else {
                push_line_quad(&mut self.hatch_mesh, p1, p2, 0.75, hatch_color);
            }
        }
    }

    /// Flush all accumulated geometry to the painter.
    pub fn emit(self, painter: &egui::Painter) {
        if !self.fill_mesh.vertices.is_empty() {
            painter.add(Shape::mesh(self.fill_mesh));
        }
        if !self.stroke_mesh.vertices.is_empty() {
            painter.add(Shape::mesh(self.stroke_mesh));
        }
        if !self.hatch_mesh.vertices.is_empty() {
            painter.add(Shape::mesh(self.hatch_mesh));
        }
    }
}

/// Tessellate a single line segment into a screen-aligned quad (4 vertices, 6 indices)
/// and push it into the given mesh.
#[inline]
fn push_line_quad(mesh: &mut Mesh, p1: Pos2, p2: Pos2, half_width: f32, color: egui::Color32) {
    let d = Vec2::new(p2.x - p1.x, p2.y - p1.y);
    let len_sq = d.x * d.x + d.y * d.y;
    if len_sq < 0.01 {
        return;
    }
    let inv_len = len_sq.sqrt().recip();
    // Normal perpendicular to the line direction
    let n = Vec2::new(-d.y * inv_len * half_width, d.x * inv_len * half_width);

    let base = mesh.vertices.len() as u32;
    let uv = egui::epaint::WHITE_UV;
    mesh.vertices.extend_from_slice(&[
        egui::epaint::Vertex { pos: Pos2::new(p1.x + n.x, p1.y + n.y), uv, color },
        egui::epaint::Vertex { pos: Pos2::new(p1.x - n.x, p1.y - n.y), uv, color },
        egui::epaint::Vertex { pos: Pos2::new(p2.x - n.x, p2.y - n.y), uv, color },
        egui::epaint::Vertex { pos: Pos2::new(p2.x + n.x, p2.y + n.y), uv, color },
    ]);
    mesh.indices.extend_from_slice(&[
        base, base + 1, base + 2,
        base, base + 2, base + 3,
    ]);
}

/// Tessellate a dashed line into quads within a mesh.
fn push_dashed_line_quads(
    mesh: &mut Mesh,
    p1: Pos2,
    p2: Pos2,
    width: f32,
    color: egui::Color32,
) {
    const DASH: f32 = 4.0;
    const GAP: f32 = 4.0;
    let dx = p2.x - p1.x;
    let dy = p2.y - p1.y;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1.0 {
        return;
    }
    let nx = dx / len;
    let ny = dy / len;
    let half = width * 0.5;
    let mut t = 0.0_f32;
    while t < len {
        let end = (t + DASH).min(len);
        let a = Pos2::new(p1.x + nx * t, p1.y + ny * t);
        let b = Pos2::new(p1.x + nx * end, p1.y + ny * end);
        push_line_quad(mesh, a, b, half, color);
        t = end + GAP;
    }
}

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
    let mut acc = MeshAccumulator::new();

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
            acc.append_polygon(cached_poly, fill, stroke_color, 1.5);
            if sa > 0 {
                acc.append_hatch(cached_poly, hatch_color);
            }
        }
    }

    acc.emit(painter);
}
