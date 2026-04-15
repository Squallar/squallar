//! Texture-based overlay rendering cache.
//!
//! Overlay polygons (SPC outlooks, NWS alerts, mesoscale discussions) are
//! rasterized to RGBA textures on a background thread using tiny-skia, then
//! displayed as geo-positioned images on the map — the same approach used
//! for radar images.  This makes per-frame overlay rendering a single
//! `painter.image()` call per overlay type: truly near-zero cost.

use std::f64::consts::PI;
use std::sync::Arc;

use rustdar_overlays::render::geo as overlay_geo;
use rustdar_overlays::render::rasterize::HitMap;
use rustdar_overlays::types::{GeoBounds, OverlayFeature, ScreenPoint};

// ── Viewport state (reused for render-trigger detection) ─────────────────

/// Multiplier for zoom-level quantization.
///
/// Overlay textures are re-rasterized only when the quantized zoom changes,
/// so this value controls the trade-off between render frequency and visual
/// freshness.  32 (= 2^5) gives ~0.031 zoom-unit granularity per step:
///
/// - **Finer** (e.g. 64): triggers excessive rerenders during smooth zoom
///   gestures, wasting CPU on nearly-identical textures.
/// - **Coarser** (e.g. 16): misses visible zoom changes, leaving stale
///   textures on screen until the next quantization boundary.
///
/// Used in [`quantize_zoom`] to encode and in `rustdar-platform` to decode
/// back to `f64`.
pub const ZOOM_QUANTIZATION_FACTOR: f64 = 32.0;

/// Quantised zoom level for detecting when a re-render is needed.
fn quantize_zoom(zoom: f64) -> i32 {
    (zoom * ZOOM_QUANTIZATION_FACTOR).round() as i32
}

/// Fraction of the texture dimension used as overdraw margin.
pub const OVERDRAW_FRACTION: f32 = 1.0;

/// When the accumulated pan exceeds this fraction of the overdraw margin,
/// a fresh render is triggered so the texture stays ahead of the viewport.
const PAN_REBUILD_THRESHOLD: f32 = 0.7;

// ── Texture cache ────────────────────────────────────────────────────────

/// Radar-specific metadata stored alongside the overlay texture.
///
/// Non-radar overlays set `radar_meta: None`. Radar overlays carry hover
/// value data, site coordinates, and range for per-frame range ring + tooltip.
pub struct RadarTextureMeta {
    /// Per-pixel values for hover tooltip lookup.
    pub value_data: Arc<Vec<f32>>,
    /// Radar site latitude.
    pub lat: f64,
    /// Radar site longitude.
    pub lon: f64,
    /// Maximum range in km (for range ring).
    pub max_range_km: f64,
}

/// A rendered overlay texture and the geo bounds it covers.
pub struct OverlayTextureData {
    /// The egui texture containing the rasterised overlay.
    pub texture: egui::TextureHandle,
    /// Geographic (lat/lon) extent of this texture.
    pub geo_bounds: GeoBounds,
    /// Data generation at render time (detects stale results).
    pub data_generation: u64,
    /// Quantised zoom at render time (`zoom * 32`).
    pub render_zoom: i32,
    /// Pixel dimensions of the texture.
    pub width: u32,
    pub height: u32,
    /// Radar-specific metadata (None for non-radar overlays).
    pub radar_meta: Option<RadarTextureMeta>,
    /// Optional hit buffer for pixel-perfect click detection on point overlays.
    pub hit_map: Option<HitMap>,
}

/// Per-overlay-type texture cache for a single pane.
pub struct OverlayTextureCache {
    /// Currently displayed texture (if any).
    pub current: Option<OverlayTextureData>,
    /// Whether a background render is in progress for this cache.
    pub render_in_flight: bool,
    /// Generation counter incremented each time a render is dispatched.
    pub render_generation: u64,
}

impl Default for OverlayTextureCache {
    fn default() -> Self {
        Self::new()
    }
}

impl OverlayTextureCache {
    pub fn new() -> Self {
        Self {
            current: None,
            render_in_flight: false,
            render_generation: 0,
        }
    }

    /// Check whether a re-render is needed for this overlay.
    ///
    /// Triggers on: data generation change, zoom change, or pan exceeding
    /// the overdraw margin.
    pub fn needs_rerender(
        &self,
        data_gen: u64,
        current_zoom: i32,
        viewport_bounds: &GeoBounds,
    ) -> bool {
        let Some(ref tex) = self.current else {
            return true;
        };
        if tex.data_generation != data_gen {
            return true;
        }
        if tex.render_zoom != current_zoom {
            return true;
        }
        // Check if the viewport has panned outside the texture coverage
        pan_exceeds_coverage(&tex.geo_bounds, viewport_bounds)
    }

    /// Increment the generation and return the new value.
    pub fn next_generation(&mut self) -> u64 {
        self.render_generation += 1;
        self.render_generation
    }
}

/// Returns `true` if the viewport has panned far enough outside the texture's
/// geo bounds that a re-render is warranted (PAN_REBUILD_THRESHOLD of margin).
fn pan_exceeds_coverage(texture_bounds: &GeoBounds, viewport_bounds: &GeoBounds) -> bool {
    let tex_lat_range = texture_bounds.max_lat - texture_bounds.min_lat;
    let tex_lon_range = texture_bounds.max_lon - texture_bounds.min_lon;
    let margin_lat = tex_lat_range * OVERDRAW_FRACTION as f64 * PAN_REBUILD_THRESHOLD as f64;
    let margin_lon = tex_lon_range * OVERDRAW_FRACTION as f64 * PAN_REBUILD_THRESHOLD as f64;

    // If viewport extends beyond texture bounds minus the margin threshold, re-render
    viewport_bounds.min_lat < texture_bounds.min_lat + margin_lat
        || viewport_bounds.max_lat > texture_bounds.max_lat - margin_lat
        || viewport_bounds.min_lon < texture_bounds.min_lon + margin_lon
        || viewport_bounds.max_lon > texture_bounds.max_lon - margin_lon
}

// ── Drawing ──────────────────────────────────────────────────────────────

/// Compute the screen-space rectangle for an overlay texture.
pub fn overlay_texture_rect(
    projector: &walkers::Projector,
    tex: &OverlayTextureData,
) -> egui::Rect {
    let nw = projector
        .project(walkers::lat_lon(tex.geo_bounds.max_lat, tex.geo_bounds.min_lon))
        .to_pos2();
    let se = projector
        .project(walkers::lat_lon(tex.geo_bounds.min_lat, tex.geo_bounds.max_lon))
        .to_pos2();
    egui::Rect::from_two_pos(nw, se)
}

/// Draw an overlay texture as a geo-positioned image on the map.
///
/// This is the per-frame draw call — projects the texture's NW/SE corners
/// to screen space and emits a single `painter.image()`.
pub fn draw_overlay_texture(
    painter: &egui::Painter,
    projector: &walkers::Projector,
    tex: &OverlayTextureData,
    screen_rect: egui::Rect,
) {
    let rect = overlay_texture_rect(projector, tex);

    // Skip if entirely off-screen
    if !screen_rect.intersects(rect) {
        return;
    }

    painter.image(
        tex.texture.id(),
        rect,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
}

// ── Geo-coordinate click detection ───────────────────────────────────────

/// Convert latitude (radians) to Web Mercator Y.
#[inline]
fn lat_rad_to_mercator_y(lat_rad: f64) -> f64 {
    (PI / 4.0 + lat_rad / 2.0).tan().ln()
}

/// Test whether a geographic point (lat, lon) falls inside any polygon of an
/// overlay feature, using the even-odd rule on geo-coordinate rings.
///
/// Uses Web Mercator Y for the vertical axis so that comparisons are
/// consistent with the rendered projection.
pub fn geo_point_in_feature(lat: f64, lon: f64, feature: &OverlayFeature) -> bool {
    let merc_y = lat_rad_to_mercator_y(lat.to_radians());
    for polygon in &feature.polygons {
        let Some(exterior) = polygon.first() else { continue };
        if exterior.len() < 3 {
            continue;
        }
        let ring: Vec<ScreenPoint> = exterior
            .iter()
            .map(|&(rlat, rlon)| {
                ScreenPoint::new(
                    rlon as f32,
                    lat_rad_to_mercator_y(rlat.to_radians()) as f32,
                )
            })
            .collect();
        let point = ScreenPoint::new(lon as f32, merc_y as f32);
        if overlay_geo::point_in_polygon(point, &ring) {
            return true;
        }
    }
    false
}

// ── Viewport bounds helper ───────────────────────────────────────────────

/// Extract the geographic bounds of the current map viewport.
pub fn viewport_geo_bounds(projector: &walkers::Projector, screen_rect: egui::Rect) -> GeoBounds {
    let nw = projector.unproject(egui::vec2(screen_rect.left(), screen_rect.top()));
    let se = projector.unproject(egui::vec2(screen_rect.right(), screen_rect.bottom()));
    GeoBounds {
        min_lat: nw.y().min(se.y()),
        max_lat: nw.y().max(se.y()),
        min_lon: nw.x().min(se.x()),
        max_lon: nw.x().max(se.x()),
    }
}

/// Compute the quantised zoom level for render-trigger comparisons.
pub fn current_quantized_zoom(zoom: f64) -> i32 {
    quantize_zoom(zoom)
}
