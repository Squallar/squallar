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
/// tests below check, and it holds by construction rather than by fixture. Per axis,
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
/// Uses Web Mercator Y for the vertical axis so that comparisons are
/// consistent with the rendered projection.
pub fn geo_point_in_feature(lat: f64, lon: f64, feature: &OverlayFeature) -> bool {
    let merc_y = lat_rad_to_mercator_y(lat.to_radians());
    for polygon in &feature.polygons {
        let Some(exterior) = polygon.first() else {
            continue;
        };
        if exterior.len() < 3 {
            continue;
        }
        let ring: Vec<ScreenPoint> = exterior
            .iter()
            .map(|&(rlat, rlon)| {
                ScreenPoint::new(rlon as f32, lat_rad_to_mercator_y(rlat.to_radians()) as f32)
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

#[cfg(test)]
mod texture_budget_tests {
    use super::*;

    /// `max_texture_dimension_2d` on a desktop adapter. wgpu's `Limits::default()`
    /// promises 8192; real GPUs report 16384 or more. Either way, nothing here is
    /// allowed to shrink at that size.
    const DESKTOP_LIMIT: u32 = 8192;
    /// WebGL2's guaranteed floor, and the whole reason clamping exists.
    const WEBGL2_LIMIT: u32 = 2048;

    fn pane(w: f32, h: f32) -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(w, h))
    }

    /// A plan with the given overdraw. Dimensions are irrelevant to `coverage`, so
    /// they stand in at 1x1 rather than pretending to a size.
    fn plan_with_overdraw(overdraw: f32) -> OverlayTexturePlan {
        OverlayTexturePlan {
            width: 1,
            height: 1,
            overdraw,
        }
    }

    /// Right after a render: the texture covers `viewport ± overdraw` on all four
    /// sides, and the viewport has not moved.
    ///
    /// Deliberately routed through the production [`OverlayTexturePlan::coverage`]
    /// rather than repeating its arithmetic. A fixture that computed the expansion
    /// itself would agree with a broken `coverage` and hide it — the same shadowing
    /// that let a wrong fraction at the call site go unnoticed.
    fn freshly_rendered(viewport: &GeoBounds, overdraw: f32) -> GeoBounds {
        plan_with_overdraw(overdraw).coverage(viewport)
    }

    /// The reference viewport: **10° of latitude by 16° of longitude**.
    ///
    /// Non-square on purpose. With a square viewport the latitude and longitude
    /// bands are equal, and every per-axis mistake in `pan_exceeds_coverage` — most
    /// obviously computing one axis's band from the other's ranges — produces
    /// identical answers and survives every assertion here.
    fn viewport() -> GeoBounds {
        GeoBounds {
            min_lat: 30.0,
            max_lat: 40.0,
            min_lon: -100.0,
            max_lon: -84.0,
        }
    }

    /// The viewport's own extent per axis, which is also the overdraw band at
    /// `overdraw == 1.0`. Derived from [`viewport`] so the two cannot drift apart.
    fn viewport_ranges() -> (f64, f64) {
        let vp = viewport();
        (vp.max_lat - vp.min_lat, vp.max_lon - vp.min_lon)
    }

    /// Slide a viewport south (negative) or north (positive) by `d` degrees.
    fn panned_lat(viewport: &GeoBounds, d: f64) -> GeoBounds {
        GeoBounds {
            min_lat: viewport.min_lat + d,
            max_lat: viewport.max_lat + d,
            ..*viewport
        }
    }

    fn panned_lon(viewport: &GeoBounds, d: f64) -> GeoBounds {
        GeoBounds {
            min_lon: viewport.min_lon + d,
            max_lon: viewport.max_lon + d,
            ..*viewport
        }
    }

    // ── plan_overlay_texture ─────────────────────────────────────────────

    /// The constraint being ported for: a pane only 683 points wide already asks for
    /// 2049 px at the full overdraw, one past WebGL2's guarantee. egui only
    /// `debug_assert!`s that bound, so a release wasm build would sail into
    /// `Device::create_texture` and fail there instead.
    #[test]
    fn a_pane_that_would_overflow_the_limit_gives_up_overdraw_instead() {
        let unclamped = 683.0 * (1.0 + 2.0 * OVERDRAW_FRACTION);
        assert!(
            unclamped as u32 > WEBGL2_LIMIT,
            "fixture must actually cross the limit: {unclamped} vs {WEBGL2_LIMIT}"
        );

        let plan = plan_overlay_texture(pane(683.0, 400.0), WEBGL2_LIMIT);
        assert!(
            plan.width <= WEBGL2_LIMIT,
            "width {} exceeds the limit",
            plan.width
        );
        assert!(
            plan.height <= WEBGL2_LIMIT,
            "height {} exceeds the limit",
            plan.height
        );
        assert!(
            plan.overdraw < OVERDRAW_FRACTION,
            "overdraw {} should have been cut back from {OVERDRAW_FRACTION}",
            plan.overdraw
        );
        // The dimensions and the overdraw describe the same rectangle.
        assert_eq!(plan.width, (683.0 * (1.0 + 2.0 * plan.overdraw)) as u32);
        assert_eq!(plan.height, (400.0 * (1.0 + 2.0 * plan.overdraw)) as u32);
    }

    /// A realistic browser window: 1440 x 900 points against WebGL2's floor.
    #[test]
    fn a_full_size_browser_pane_stays_within_the_limit() {
        let plan = plan_overlay_texture(pane(1440.0, 900.0), WEBGL2_LIMIT);
        assert!(plan.width <= WEBGL2_LIMIT && plan.height <= WEBGL2_LIMIT);
        // Width is the binding axis, so it lands on the limit exactly.
        assert_eq!(plan.width, WEBGL2_LIMIT);
        // ...and the *same* fraction sizes the other axis, so the texture stays
        // proportional to the pane rather than stretching.
        assert_eq!(plan.height, (900.0 * (1.0 + 2.0 * plan.overdraw)) as u32);
        assert!(plan.overdraw > 0.0 && plan.overdraw < OVERDRAW_FRACTION);
    }

    /// The binding axis is whichever needs the most texels, not always width.
    #[test]
    fn the_taller_axis_can_be_the_binding_one() {
        let plan = plan_overlay_texture(pane(400.0, 1400.0), WEBGL2_LIMIT);
        assert!(plan.width <= WEBGL2_LIMIT && plan.height <= WEBGL2_LIMIT);
        assert_eq!(plan.height, WEBGL2_LIMIT);
        assert!(plan.overdraw < OVERDRAW_FRACTION);
    }

    /// Desktop must not change. A desktop adapter's limit is far above anything a
    /// window can demand, so the plan is bit-for-bit what the old constant produced.
    #[test]
    fn a_desktop_adapter_limit_changes_nothing() {
        for (w, h) in [
            (683.0, 400.0),
            (1920.0, 1080.0),
            (2560.0, 1440.0),
            (100.0, 100.0),
        ] {
            let plan = plan_overlay_texture(pane(w, h), DESKTOP_LIMIT);
            assert_eq!(
                plan.overdraw, OVERDRAW_FRACTION,
                "{w}x{h} should keep the full overdraw on a desktop adapter"
            );
            assert_eq!(plan.width, (w * (1.0 + 2.0 * OVERDRAW_FRACTION)) as u32);
            assert_eq!(plan.height, (h * (1.0 + 2.0 * OVERDRAW_FRACTION)) as u32);
        }
    }

    /// Past the point where the viewport alone fills the limit there is no overdraw
    /// left to give up, and the dimensions must still not overflow. Both axes are
    /// oversized here so the clamp on each is genuinely exercised.
    #[test]
    fn a_pane_wider_than_the_limit_falls_back_to_zero_overdraw() {
        let rect = pane(3000.0, 2500.0);
        assert!(
            rect.width().min(rect.height()) > WEBGL2_LIMIT as f32,
            "fixture must overflow on both axes for both clamps to be reached"
        );
        let plan = plan_overlay_texture(rect, WEBGL2_LIMIT);
        assert_eq!(plan.overdraw, 0.0, "nothing left to give up");
        assert_eq!(plan.width, WEBGL2_LIMIT);
        assert_eq!(plan.height, WEBGL2_LIMIT);
    }

    /// Only the axis that actually overflows is truncated; the other keeps its
    /// natural size. Clamping both to the limit would stretch the overlay.
    #[test]
    fn only_the_overflowing_axis_is_truncated() {
        let plan = plan_overlay_texture(pane(3000.0, 900.0), WEBGL2_LIMIT);
        assert_eq!(plan.overdraw, 0.0);
        assert_eq!(
            plan.width, WEBGL2_LIMIT,
            "the overflowing axis is cut to the limit"
        );
        assert_eq!(plan.height, 900, "the axis that fits keeps its own size");
    }

    /// A zero-area pane must not leak the `inf` its own division produces into the
    /// fraction or the dimensions. Nothing special-cases the zero — `min` discards
    /// the `inf` and the cast floors the product — so this pins that the general
    /// arithmetic really does stay finite rather than that a guard branch exists.
    #[test]
    fn a_degenerate_pane_produces_a_finite_plan() {
        let plan = plan_overlay_texture(pane(0.0, 0.0), WEBGL2_LIMIT);
        assert!(plan.overdraw.is_finite(), "got {}", plan.overdraw);
        assert_eq!(
            plan.overdraw, OVERDRAW_FRACTION,
            "a zero side constrains nothing"
        );
        assert_eq!((plan.width, plan.height), (0, 0));
    }

    /// One zero axis, one real one: the real axis still has to be sized and clamped
    /// normally rather than being dragged to zero or to `inf` by its neighbour.
    #[test]
    fn a_pane_with_one_zero_axis_still_sizes_the_other() {
        let plan = plan_overlay_texture(pane(0.0, 3000.0), WEBGL2_LIMIT);
        assert!(plan.overdraw.is_finite());
        assert_eq!(
            plan.overdraw, 0.0,
            "the 3000pt axis alone exhausts the limit"
        );
        assert_eq!(plan.width, 0);
        assert_eq!(plan.height, WEBGL2_LIMIT);
    }

    // ── pan_exceeds_coverage ─────────────────────────────────────────────

    /// The check the cache exists for. Before this was measured from the bounds it
    /// compared `tex_range * OVERDRAW_FRACTION * PAN_REBUILD_THRESHOLD`, and with a
    /// three-viewport texture that margin (2.1 viewports) swallowed the whole
    /// overdraw band — so this returned `true` the instant the render landed and
    /// every overlay re-rasterised on every frame.
    #[test]
    fn a_texture_that_just_rendered_covers_its_own_viewport() {
        let vp = viewport();
        let tex = freshly_rendered(&vp, OVERDRAW_FRACTION);
        assert!(
            !pan_exceeds_coverage(&tex, &vp),
            "a texture rendered for this very viewport cannot already be out of coverage"
        );
    }

    /// Past `PAN_REBUILD_THRESHOLD` of the band, on every edge.
    ///
    /// Each axis is measured against **its own** band. The viewport is 10° tall and
    /// 16° wide, so a pan that overruns the latitude band is comfortably inside the
    /// longitude one — which is what makes this fail if the two are ever crossed.
    #[test]
    fn panning_most_of_the_way_across_the_band_triggers_a_rebuild() {
        let vp = viewport();
        let (band_lat, band_lon) = viewport_ranges(); // at overdraw 1.0 the band is the range
        let tex = freshly_rendered(&vp, OVERDRAW_FRACTION);
        let past = |band: f64| band * (PAN_REBUILD_THRESHOLD as f64 + 0.05);
        let short = |band: f64| band * (PAN_REBUILD_THRESHOLD as f64 - 0.05);

        for d in [past(band_lat), -past(band_lat)] {
            assert!(
                pan_exceeds_coverage(&tex, &panned_lat(&vp, d)),
                "lat pan {d} must rebuild"
            );
        }
        for d in [past(band_lon), -past(band_lon)] {
            assert!(
                pan_exceeds_coverage(&tex, &panned_lon(&vp, d)),
                "lon pan {d} must rebuild"
            );
        }
        for d in [short(band_lat), -short(band_lat)] {
            assert!(
                !pan_exceeds_coverage(&tex, &panned_lat(&vp, d)),
                "lat pan {d} is still covered"
            );
        }
        for d in [short(band_lon), -short(band_lon)] {
            assert!(
                !pan_exceeds_coverage(&tex, &panned_lon(&vp, d)),
                "lon pan {d} is still covered"
            );
        }
    }

    /// Each axis's margin comes from that axis's own ranges. The bands here differ by
    /// 60% (10° of latitude against 16° of longitude), so a pan sized to sit just
    /// inside the latitude band lands *outside* it if the longitude band is
    /// substituted — a cross-axis mix-up no square fixture can see.
    #[test]
    fn each_axis_is_judged_against_its_own_band() {
        let vp = viewport();
        let (band_lat, band_lon) = viewport_ranges();
        assert!(
            band_lon > band_lat * 1.5,
            "fixture must be decisively non-square"
        );

        let tex = freshly_rendered(&vp, OVERDRAW_FRACTION);
        // Headroom is 30% of the band: 3° of latitude, 4.8° of longitude. A 6.5°
        // southward pan leaves 3.5° of the latitude band — still covered — but would
        // read as only 3.5° against a 4.8° longitude margin, and rebuild.
        let pan = panned_lat(&vp, -6.5);
        let headroom = 1.0 - PAN_REBUILD_THRESHOLD as f64;
        assert!(
            band_lat * headroom < 3.5 && 3.5 < band_lon * headroom,
            "fixture must straddle the two margins: {} < 3.5 < {}",
            band_lat * headroom,
            band_lon * headroom
        );
        assert!(
            !pan_exceeds_coverage(&tex, &pan),
            "the latitude band still covers this pan"
        );
    }

    /// The invariant clamping would otherwise break. A texture whose overdraw was cut
    /// to 0.2 tolerates far less pan than one with the full 1.0 — and the check has to
    /// notice, or the cache holds a stale image over ground it never rasterised.
    #[test]
    fn a_clamped_texture_runs_out_of_coverage_sooner_than_a_full_one() {
        let vp = viewport();
        let clamped = freshly_rendered(&vp, 0.2);
        let full = freshly_rendered(&vp, OVERDRAW_FRACTION);

        // 0.2 of a 10 degree viewport height is a 2 degree band; 0.7 of that is 1.4.
        let pan = panned_lat(&vp, -1.6);
        assert!(
            pan_exceeds_coverage(&clamped, &pan),
            "a 2-degree band cannot absorb a 1.6-degree pan"
        );
        assert!(
            !pan_exceeds_coverage(&full, &pan),
            "precondition: the same pan is comfortably inside a full-overdraw texture, \
             so only the coverage measurement distinguishes these"
        );
    }

    /// A texture with no overdraw at all — what a pane wider than the adapter's limit
    /// gets — must rebuild on any pan whatsoever, and *not* before.
    ///
    /// The unpanned case is the one that matters: with a zero band every comparison
    /// sits exactly on its boundary, so a `<` relaxed to `<=` reports "panned off"
    /// for a viewport that has not moved at all. That re-rasterises every frame on
    /// precisely the wide-pane wasm configuration this whole change exists for, and
    /// no non-degenerate fixture can see it.
    #[test]
    fn a_zero_overdraw_texture_rebuilds_on_the_slightest_pan() {
        let vp = viewport();
        let tex = freshly_rendered(&vp, 0.0);
        assert!(
            !pan_exceeds_coverage(&tex, &vp),
            "a zero-overdraw texture still covers the viewport it was rendered for"
        );
        assert!(pan_exceeds_coverage(&tex, &panned_lat(&vp, -0.001)));
        assert!(pan_exceeds_coverage(&tex, &panned_lat(&vp, 0.001)));
        assert!(pan_exceeds_coverage(&tex, &panned_lon(&vp, -0.001)));
        assert!(pan_exceeds_coverage(&tex, &panned_lon(&vp, 0.001)));
    }

    /// A pane that grew since its texture was rasterised is no longer covered, even
    /// without panning. The measured band goes negative and trips the comparison.
    #[test]
    fn a_pane_that_outgrew_its_texture_rebuilds() {
        let vp = viewport();
        let tex = freshly_rendered(&vp, 0.1);
        let grown = GeoBounds {
            min_lat: 20.0,
            max_lat: 50.0,
            min_lon: -110.0,
            max_lon: -80.0,
        };
        assert!(
            grown.max_lat - grown.min_lat > tex.max_lat - tex.min_lat,
            "fixture must actually outgrow the texture"
        );
        assert!(pan_exceeds_coverage(&tex, &grown));
    }

    // ── the two together ─────────────────────────────────────────────────

    /// End to end: plan a texture against a small limit, take the coverage the plan
    /// itself reports (exactly as `spawn_overlay_render` does), and the coverage
    /// check agrees it is fresh. Expanding by `OVERDRAW_FRACTION` instead — the bug
    /// clamping would introduce — claims ground the pixels never covered.
    #[test]
    fn the_plans_overdraw_is_what_the_coverage_check_reads_back() {
        let vp = viewport();
        let (band_lat_at_full, _) = viewport_ranges();
        let plan = plan_overlay_texture(pane(1440.0, 900.0), WEBGL2_LIMIT);
        assert!(
            plan.overdraw < OVERDRAW_FRACTION,
            "fixture must be a clamped one"
        );

        // The production path: the plan is asked for its own coverage.
        let honest = plan.coverage(&vp);
        assert!(!pan_exceeds_coverage(&honest, &vp));

        // The band the honest texture really has, and a pan that overruns it.
        let overrun = panned_lat(&vp, -(band_lat_at_full * plan.overdraw as f64 * 0.95));
        assert!(pan_exceeds_coverage(&honest, &overrun));

        // Had the bounds been expanded by the unclamped constant, the same pan would
        // have looked comfortably covered — the stale-overlay failure mode.
        let overclaimed = plan_with_overdraw(OVERDRAW_FRACTION).coverage(&vp);
        assert!(!pan_exceeds_coverage(&overclaimed, &overrun));
    }

    // ── OverlayTexturePlan::coverage ─────────────────────────────────────

    /// The plan's own fraction sizes the bounds, so pixels and coverage describe the
    /// same rectangle. A clamped plan must produce visibly tighter bounds than the
    /// unclamped constant would.
    #[test]
    fn the_bounds_grow_by_the_plans_overdraw_not_the_constant() {
        // A 1440pt-wide pane against WebGL2's floor: the plan has to give overdraw up.
        let plan = plan_overlay_texture(pane(1440.0, 900.0), WEBGL2_LIMIT);
        assert!(
            plan.overdraw < OVERDRAW_FRACTION,
            "fixture must be a clamped plan, else this test cannot tell the two apart"
        );

        let vp = viewport();
        let (lat_range, lon_range) = viewport_ranges();
        let honest = plan.coverage(&vp);
        let overclaimed = plan_with_overdraw(OVERDRAW_FRACTION).coverage(&vp);

        assert!((honest.min_lat - (vp.min_lat - lat_range * plan.overdraw as f64)).abs() < 1e-9);
        assert!((honest.max_lon - (vp.max_lon + lon_range * plan.overdraw as f64)).abs() < 1e-9);
        assert!(
            honest.min_lat > overclaimed.min_lat,
            "the clamped plan must claim strictly less ground than the constant would"
        );
        assert!(honest.max_lat < overclaimed.max_lat);
        assert!(honest.min_lon > overclaimed.min_lon);
        assert!(honest.max_lon < overclaimed.max_lon);
    }

    /// Zero overdraw — a pane wider than the adapter's whole texture limit — must
    /// leave the viewport exactly as it is rather than defaulting to a margin.
    #[test]
    fn zero_overdraw_leaves_the_viewport_untouched() {
        let vp = viewport();
        let bounds = plan_with_overdraw(0.0).coverage(&vp);
        assert_eq!(
            (
                bounds.min_lat,
                bounds.max_lat,
                bounds.min_lon,
                bounds.max_lon
            ),
            (vp.min_lat, vp.max_lat, vp.min_lon, vp.max_lon)
        );
    }

    /// Latitude is clamped to the Mercator-valid range; longitude is not, because
    /// the map wraps.
    #[test]
    fn latitude_is_clamped_to_the_mercator_range() {
        let polar = GeoBounds {
            min_lat: -80.0,
            max_lat: 80.0,
            min_lon: -10.0,
            max_lon: 10.0,
        };
        assert!(
            80.0 + 160.0 * OVERDRAW_FRACTION as f64 > MERCATOR_LAT_LIMIT,
            "fixture must actually overrun the clamp"
        );
        let bounds = plan_with_overdraw(OVERDRAW_FRACTION).coverage(&polar);
        assert_eq!(bounds.max_lat, MERCATOR_LAT_LIMIT);
        assert_eq!(bounds.min_lat, -MERCATOR_LAT_LIMIT);
        assert_eq!(bounds.min_lon, -10.0 - 20.0 * OVERDRAW_FRACTION as f64);
    }
}
