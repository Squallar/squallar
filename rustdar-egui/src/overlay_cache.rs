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
use rustdar_radar::types::RadarProduct;

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

/// Overdraw the renderer *asks* for, as a fraction of the viewport dimension,
/// on each side of the viewport.
///
/// This is a request, not a promise. A texture wide enough for `1.0` on both
/// sides is three viewports across, and no adapter is obliged to allocate one:
/// WebGL2 only guarantees `max_texture_dimension_2d == 2048`, which a viewport
/// wider than 682 points already blows past. [`plan_overlay_texture`] cuts the
/// fraction back to whatever the adapter can actually hold, and the reduced
/// value — never this constant — is what the rest of the pipeline works from.
pub const OVERDRAW_FRACTION: f32 = 1.0;

/// When the accumulated pan exceeds this fraction of the overdraw margin,
/// a fresh render is triggered so the texture stays ahead of the viewport.
const PAN_REBUILD_THRESHOLD: f32 = 0.7;

/// Latitude beyond which Web Mercator stops being finite. Bounds are clamped to it
/// rather than allowed to run to the pole.
const MERCATOR_LAT_LIMIT: f64 = 85.05;

/// The texture an overlay render should actually allocate.
///
/// Produced by [`plan_overlay_texture`], which is the single place that reconciles
/// [`OVERDRAW_FRACTION`] with the adapter's texture-size limit.
///
/// The ground the texture covers is [`Self::coverage`], a method rather than a free
/// function taking a fraction, and deliberately so: the fraction and
/// [`OVERDRAW_FRACTION`] are both `f32`, both in scope wherever a render is
/// dispatched, and both entirely plausible there — but passing the constant claims
/// ground the pixels do not cover, which is the whole failure this module exists to
/// prevent. Reading the fraction off `self` means the wrong one cannot be handed
/// over, in the same spirit as `BroadcastSweep` in `pane.rs`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverlayTexturePlan {
    /// Texture width in pixels. Never exceeds the adapter's `max_texture_side`.
    pub width: u32,
    /// Texture height in pixels. Never exceeds the adapter's `max_texture_side`.
    pub height: u32,
    /// Overdraw actually afforded, per side, as a fraction of the viewport
    /// dimension. `<= OVERDRAW_FRACTION`, and `0.0` when the viewport alone
    /// already fills the adapter's limit.
    ///
    /// Read through [`Self::coverage`] rather than passed around loose.
    pub overdraw: f32,
}

impl OverlayTexturePlan {
    /// The geographic ground a texture built to this plan covers, when it is
    /// rasterised for `viewport`.
    ///
    /// This is what gets stored as the texture's `geo_bounds`, so it is also what
    /// [`pan_exceeds_coverage`] later measures the overdraw band from. The two must
    /// describe the same rectangle: the pixel count and the coverage both come from
    /// `self.overdraw`, and there is no parameter here through which they could be
    /// made to disagree.
    ///
    /// Latitude is clamped to [`MERCATOR_LAT_LIMIT`]; longitude is not, because the
    /// map wraps and a texture may legitimately straddle the antimeridian.
    pub fn coverage(&self, viewport: &GeoBounds) -> GeoBounds {
        let lat_range = viewport.max_lat - viewport.min_lat;
        let lon_range = viewport.max_lon - viewport.min_lon;
        let overdraw = self.overdraw as f64;
        GeoBounds {
            min_lat: (viewport.min_lat - lat_range * overdraw).max(-MERCATOR_LAT_LIMIT),
            max_lat: (viewport.max_lat + lat_range * overdraw).min(MERCATOR_LAT_LIMIT),
            min_lon: viewport.min_lon - lon_range * overdraw,
            max_lon: viewport.max_lon + lon_range * overdraw,
        }
    }
}

/// Size the overlay texture for `screen_rect`, giving up overdraw rather than
/// exceeding `max_texture_side`.
///
/// `max_texture_side` is the adapter's `max_texture_dimension_2d`, which reaches
/// here through egui: `egui_winit::State::new` is handed `device.limits()`, and
/// `ui.ctx().input(|i| i.max_texture_side)` reads it back. On a desktop adapter it
/// is 8192–32768 and nothing is given up; the WebGL2 floor of 2048 is the case
/// this exists for.
///
/// Overdraw is the free variable because the alternative — keeping the full
/// three-viewport coverage and shrinking the pixels — makes the overlay blurrier
/// the wider the window gets, which is exactly backwards. Cutting overdraw keeps
/// one texel per point and costs only re-render frequency.
///
/// The returned `overdraw` is load-bearing, not diagnostic: it is what the geo
/// bounds get expanded by, so the texture's coverage and its pixel count describe
/// the same rectangle. Expanding by [`OVERDRAW_FRACTION`] after the pixels were
/// clamped would claim ground the texture does not cover, and
/// [`pan_exceeds_coverage`] would then hold off re-rendering over that gap.
pub fn plan_overlay_texture(screen_rect: egui::Rect, max_texture_side: u32) -> OverlayTexturePlan {
    let screen_w = screen_rect.width().max(0.0);
    let screen_h = screen_rect.height().max(0.0);
    let max_side = max_texture_side.max(1);

    // Largest overdraw this axis can afford: `side * (1 + 2f) == max_side`.
    // Negative when the viewport alone overflows the limit, hence the `max(0.0)`.
    // A zero side divides to `inf`, which `min` discards — no special case needed.
    let affordable = |side: f32| (max_side as f32 / side - 1.0) / 2.0;
    let overdraw = OVERDRAW_FRACTION
        .min(affordable(screen_w))
        .min(affordable(screen_h))
        .max(0.0);

    let scale = 1.0 + 2.0 * overdraw;
    // `min(max_side)` is load-bearing, not defensive. It is the *only* thing keeping
    // the primary WebGL2 case legal: once `max(0.0)` has floored the overdraw at
    // zero — a pane at least as wide as the whole limit — `scale` is 1.0 and the
    // arithmetic above no longer targets `max_side` at all. A 3000-point pane
    // against a 2048 limit computes 3000 here and is cut to 2048 by this call.
    OverlayTexturePlan {
        width: ((screen_w * scale) as u32).min(max_side),
        height: ((screen_h * scale) as u32).min(max_side),
        overdraw,
    }
}

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
    /// The product these pixels depict.
    ///
    /// Not the pane's `selected_product`: that is what the user has *asked* for,
    /// and the two differ for as long as a render takes. Kept here, alongside the
    /// texture rather than beside it, because the pair has to be replaced and
    /// dropped together — a field on the pane could outlive the image it
    /// described, and would then be a confident lie. See
    /// [`crate::pane::PaneState::stale_image_on_screen`].
    pub product: RadarProduct,
    /// The sweep angle these pixels depict — the *snapped* elevation the
    /// renderer was given, which is what
    /// [`crate::pane::PaneState::get_rendering_params`] resolves and what the
    /// selection is compared against.
    pub elevation: f32,
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
///
/// The overdraw band is *measured*, not assumed: it is half of whatever the
/// texture covers beyond the viewport. That is the whole point — once
/// [`plan_overlay_texture`] is free to cut the overdraw back to fit the adapter's
/// texture limit, no constant in this file describes the band any more, and a
/// check written against [`OVERDRAW_FRACTION`] would credit a clamped texture with
/// coverage it never had and sit on a stale image while the viewport panned off it.
/// Reading the band off the bounds the render actually used cannot drift from them.
///
/// Both ranges come from the same zoom: [`OverlayTextureCache::needs_rerender`]
/// returns early when the quantised zoom differs, so `viewport_bounds` spans the
/// same ground per pixel as it did at render time. A pane that has since *grown*
/// yields a negative band, and hence a negative margin, which trips the comparison
/// immediately — correct, because the texture no longer covers the viewport.
///
/// # `false` implies containment
///
/// Measuring the band rather than assuming it buys a guarantee stronger than the
/// tests check, and it holds by construction rather than by fixture. Per axis,
/// write `D = tex_range - view_range` and `m = h·D/2` with
/// `h = 1 - PAN_REBUILD_THRESHOLD`. Returning `false` requires both
/// `view_min >= tex_min + m` and `view_max <= tex_max - m`; subtracting gives
/// `view_range <= tex_range - 2m`, and substituting `m` reduces that to
/// `D·(1 - h) >= 0`. For any `h` in `(0, 1)` — which any sane threshold is — that
/// is exactly `D >= 0`, so the texture is at least as wide as the viewport and,
/// with the two endpoint inequalities, contains it.
///
/// So this function cannot report "still covered" about a texture that does not in
/// fact contain the viewport. The stale-overlay failure mode is unrepresentable,
/// not merely untested — and it stays that way for any threshold anyone picks,
/// which is why `PAN_REBUILD_THRESHOLD` is safe to tune.
fn pan_exceeds_coverage(texture_bounds: &GeoBounds, viewport_bounds: &GeoBounds) -> bool {
    let tex_lat_range = texture_bounds.max_lat - texture_bounds.min_lat;
    let tex_lon_range = texture_bounds.max_lon - texture_bounds.min_lon;
    let view_lat_range = viewport_bounds.max_lat - viewport_bounds.min_lat;
    let view_lon_range = viewport_bounds.max_lon - viewport_bounds.min_lon;

    // Overdraw actually present on each side of the viewport.
    let band_lat = (tex_lat_range - view_lat_range) / 2.0;
    let band_lon = (tex_lon_range - view_lon_range) / 2.0;

    // Headroom left when the pan has consumed PAN_REBUILD_THRESHOLD of the band.
    // Crossing into it is what triggers the rebuild, leaving the rest of the band
    // to cover the viewport while the new texture rasterises.
    let headroom = 1.0 - PAN_REBUILD_THRESHOLD as f64;
    let margin_lat = band_lat * headroom;
    let margin_lon = band_lon * headroom;

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
        .project(walkers::lat_lon(
            tex.geo_bounds.max_lat,
            tex.geo_bounds.min_lon,
        ))
        .to_pos2();
    let se = projector
        .project(walkers::lat_lon(
            tex.geo_bounds.min_lat,
            tex.geo_bounds.max_lon,
        ))
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
/// Each polygon's first ring is its exterior; the rest are holes (see
/// `GeoPolygonRing`). Under the even-odd rule a point inside a hole is
/// *outside* that polygon, so an exterior hit only counts when no hole of the
/// same polygon contains the point — otherwise a click in the cut-out of an
/// SPC/NWS donut opens the surrounding feature's popup.
///
/// Uses Web Mercator Y for the vertical axis so that comparisons are
/// consistent with the rendered projection.
pub fn geo_point_in_feature(lat: f64, lon: f64, feature: &OverlayFeature) -> bool {
    let merc_y = lat_rad_to_mercator_y(lat.to_radians());
    let point = ScreenPoint::new(lon as f32, merc_y as f32);
    let ring_contains = |ring: &[(f64, f64)]| {
        if ring.len() < 3 {
            return false;
        }
        let projected: Vec<ScreenPoint> = ring
            .iter()
            .map(|&(rlat, rlon)| {
                ScreenPoint::new(rlon as f32, lat_rad_to_mercator_y(rlat.to_radians()) as f32)
            })
            .collect();
        overlay_geo::point_in_polygon(point, &projected)
    };
    for polygon in &feature.polygons {
        let Some(exterior) = polygon.first() else {
            continue;
        };
        if !ring_contains(exterior) {
            continue;
        }
        // Inside the exterior — but inside any interior ring means this
        // polygon has a hole here, and the point is outside it. Another
        // polygon of the same feature may still contain the point.
        if !polygon[1..].iter().any(|hole| ring_contains(hole)) {
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

#[cfg(test)]
mod geo_click_tests;

#[cfg(test)]
mod texture_budget_tests;
