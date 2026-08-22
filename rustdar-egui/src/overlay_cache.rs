//! Texture-based overlay rendering cache.

use std::sync::Arc;

use rustdar_geo::{GeoBounds, PlacedRaster};
use rustdar_overlays::render::geo as overlay_geo;
use rustdar_overlays::render::rasterize::HitMap;
use rustdar_overlays::types::{OverlayFeature, ScreenPoint};
use rustdar_source::product::FieldId;

// ── Viewport state (reused for render-trigger detection) ─────────────────

/// Fixed-point scale for carrying a zoom level across the render channel.
pub const ZOOM_QUANTIZATION_FACTOR: f64 = 32.0;

/// Quantised zoom, for the render channel. See [`ZOOM_QUANTIZATION_FACTOR`].
fn quantize_zoom(zoom: f64) -> i32 {
    (zoom * ZOOM_QUANTIZATION_FACTOR).round() as i32
}

/// How far the map may zoom away from a texture's own zoom, in zoom units,
/// before the texture is re-rasterized mid-gesture.
pub const ZOOM_REBUILD_BAND: f64 = 1.0;

/// How long a zoom must be still before the gesture counts as settled — and so
/// also how long after it stops before the settle render is asked for.
pub const SETTLE_REPAINT_DELAY: std::time::Duration = std::time::Duration::from_millis(100);

/// Overdraw the renderer *asks* for, as a fraction of the viewport dimension,
/// on each side of the viewport.
pub const OVERDRAW_FRACTION: f32 = 0.25;

/// When the accumulated pan exceeds this fraction of the overdraw margin,
/// a fresh render is triggered so the texture stays ahead of the viewport.
///
/// The rest of the band — `1 - PAN_REBUILD_THRESHOLD` of it — is the cover: the
/// ground the pane still has to draw on while the replacement rasterises and
/// uploads. Splitting the band in half is what makes that cover longest **in
/// the only sense that matters, the pan speed the pane can sustain without
/// running off its own texture.**
///
/// Both directions cost something, which is why the optimum is interior. Above
/// a half, the cover is simply too short: each texture is dispatched late and
/// the viewport outruns it. Below a half, the trigger stops being the binding
/// constraint — dispatch is gated by the previous raster's arrival instead
/// ([`OverlayTextureCache::render_in_flight`] admits one at a time) — so the
/// extra rebuilds buy no speed, and because each fires earlier it is rasterised
/// for a viewport further behind and lands staler.
///
/// Swept 2026-08-22 against this module's own [`pan_exceeds_coverage`] and
/// [`OverlayTexturePlan::coverage`], on a 60 Hz loop reproducing the dispatch
/// path (one in-flight raster per pane and layer, `held` consulted by the
/// trigger at raster arrival, `current` replaced only once every upload band has
/// landed). Sustainable pan, viewports/second, desktop 1920×1080 at full
/// overdraw, raster 11.66 ms and a two-frame banded upload:
///
/// | threshold | 0.7 | 0.6 | 0.55 | **0.5** | 0.45 | 0.4 | 0.3 |
/// |-----------|-----|-----|------|---------|------|-----|-----|
/// | max vp/s  | 1.50| 1.67| 1.88 | **2.14**| 2.14 |2.00 |1.50 |
///
/// The peak is a plateau over 0.45–0.5 and 0.5 is its cheap end: same speed as
/// 0.45 for 11% fewer rebuilds per viewport panned. It costs no memory at all —
/// the band is [`OVERDRAW_FRACTION`], which this does not touch.
///
/// **Two things this constant cannot reach.** Delivery, not raster, is most of
/// the latency it is dividing the band against — 33.3 ms of banded upload
/// against 11.66 ms of raster, and with delivery removed entirely the same
/// sweep sustains 15 vp/s rather than 2.14. And where the adapter has clamped
/// the overdraw away the band is what shrinks, not this: `pan_exceeds_coverage`
/// measures the band off the texture's real bounds, so a WebGL2 pane at the
/// 2048 floor divides 0.033 of a viewport here and one at 2× device pixels
/// divides zero, where no threshold whatsoever buys cover.
const PAN_REBUILD_THRESHOLD: f32 = 0.5;

/// Latitude beyond which Web Mercator stops being finite. Bounds are clamped to it
/// rather than allowed to run to the pole.
const MERCATOR_LAT_LIMIT: f64 = rustdar_geo::MERCATOR_LAT_LIMIT_DEG;

/// The texture an overlay render should actually allocate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverlayTexturePlan {
    /// Texture width in pixels. Never exceeds the adapter's `max_texture_side`.
    pub width: u32,
    /// Texture height in pixels. Never exceeds the adapter's `max_texture_side`.
    pub height: u32,
    /// Overdraw actually afforded, per side, as a fraction of the viewport
    /// dimension. `<= OVERDRAW_FRACTION`, and `0.0` when the viewport alone
    /// already fills the adapter's limit.
    pub overdraw: f32,
    /// Physical pixels per logical point the texture was sized at — the
    /// display density [`plan_overlay_texture`] was handed, after the same
    /// clamping the pixel counts got.
    pub pixels_per_point: f32,
}

impl OverlayTexturePlan {
    /// The geographic ground a texture built to this plan covers, when it is
    /// rasterised for `viewport`.
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
pub fn plan_overlay_texture(
    screen_rect: egui::Rect,
    max_texture_side: u32,
    pixels_per_point: f32,
) -> OverlayTexturePlan {
    // A density that is not a positive number is not a description of a
    // display. egui never reports one, but this value reaches a texture
    // allocation and a `NaN` would arrive there as a zero-sized texture rather
    // than as an error anybody could read.
    let density = if pixels_per_point.is_finite() && pixels_per_point > 0.0 {
        pixels_per_point
    } else {
        1.0
    };
    let screen_w = screen_rect.width().max(0.0) * density;
    let screen_h = screen_rect.height().max(0.0) * density;
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
        pixels_per_point: density,
    }
}

// ── Texture cache ────────────────────────────────────────────────────────

/// Radar-specific metadata stored alongside the overlay texture.
#[derive(Clone)]
pub struct RadarTextureMeta {
    /// The gates behind these pixels, for the hover readout — see
    /// [`rustdar_radar::hover::HoverSource`]. It replaced a `side²` `f32` grid
    /// of the same numbers resampled up to the raster's resolution.
    pub hover: Arc<rustdar_radar::hover::HoverSource>,
    pub lat: f64,
    pub lon: f64,
    /// The half-width this texture was projected at, km — the renderer's own
    /// answer, which is the sweep's own reach, capped only by
    /// [`rustdar_radar::types::MAX_EXTENT_KM`] and replaced by
    /// `FALLBACK_EXTENT_KM` when the scan states no reach at all.
    pub max_range_km: f64,
    /// Where the cut behind these pixels declared its velocity folds, m/s, or
    /// `None` for a raster no single cut is behind — every Level III product,
    /// every volume product, and any volume that declared nothing.
    pub nyquist_ms: Option<f64>,
    /// Where the melting layer these pixels were classified against came from,
    /// or `None` for a raster that classified nothing.
    pub melting_layer_source: Option<rustdar_radar::hca::MeltingLayerSource>,
    /// Where the storm motion vector these pixels were shifted by came from,
    /// or `None` for a raster that shifted nothing.
    pub storm_motion: Option<rustdar_radar::srv::SrvMotion>,
    pub product: FieldId,
    /// The sweep angle these pixels depict — the *snapped* elevation the
    /// renderer was given, which is what
    /// [`crate::pane::PaneState::get_rendering_params`] resolves and what the
    /// selection is compared against.
    pub elevation: f32,
}

/// A rendered overlay texture and the geo bounds it covers.
///
/// `Clone` because a loop frame holds one: [`crate::pane::LoopFrameImage::Overlay`]
/// is a whole placed raster, and cloning it is a refcount on the texture handle
/// plus the small placement record beside it.
#[derive(Clone)]
pub struct OverlayTextureData {
    pub texture: egui::TextureHandle,
    pub placed: PlacedRaster,
    /// The cache token these pixels were rendered for (detects stale results).
    pub data_generation: u64,
    pub render_zoom: i32,
    pub width: u32,
    pub height: u32,
    pub radar_meta: Option<RadarTextureMeta>,
    pub hit_map: Option<HitMap>,
}

/// A picture that has been handed to the GPU and is not yet all there.
pub struct HeldOverlayTexture {
    pub data: OverlayTextureData,
    /// The pane's [`data_time`](crate::pane::PaneState::data_time) for these
    /// pixels, applied by [`crate::pane::PaneState::promote_held_raster`].
    pub data_time: Option<chrono::NaiveDateTime>,
}

/// Per-overlay-type texture cache for a single pane.
pub struct OverlayTextureCache {
    /// Currently displayed texture (if any) — **whole**, always.
    current: Option<OverlayTextureData>,
    held: Option<HeldOverlayTexture>,
    pub render_in_flight: bool,
    /// The zoom [`Self::needs_rerender`] was asked about last time, which is
    /// how it notices the zoom moving and re-stamps [`Self::zoom_still_since`].
    last_seen_zoom: Option<f64>,
    /// When [`Self::last_seen_zoom`] last changed, in the caller's clock
    /// (`egui::InputState::time`, seconds). The gesture has settled once `now`
    /// is [`SETTLE_REPAINT_DELAY`] past this.
    zoom_still_since: f64,
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
            held: None,
            render_in_flight: false,
            last_seen_zoom: None,
            zoom_still_since: f64::NEG_INFINITY,
        }
    }

    pub fn current(&self) -> Option<&OverlayTextureData> {
        self.current.as_ref()
    }

    /// Put `data` on screen now, and let go of anything being held.
    pub fn show(&mut self, data: OverlayTextureData) {
        self.held = None;
        self.current = Some(data);
    }

    /// Hold `data` until its pixels have all reached the GPU.
    pub fn hold(&mut self, data: OverlayTextureData, data_time: Option<chrono::NaiveDateTime>) {
        self.held = Some(HeldOverlayTexture { data, data_time });
    }

    pub fn held_texture(&self) -> Option<&egui::TextureHandle> {
        self.held.as_ref().map(|held| &held.data.texture)
    }

    pub fn is_holding(&self) -> bool {
        self.held.is_some()
    }

    /// Take the held picture if `delivered` says its pixels have all landed.
    pub fn take_held_if_delivered(
        &mut self,
        delivered: impl Fn(egui::TextureId) -> bool,
    ) -> Option<HeldOverlayTexture> {
        if !delivered(self.held.as_ref()?.data.texture.id()) {
            return None;
        }
        self.held.take()
    }

    /// Forget the picture on screen and anything being held for it.
    pub fn clear(&mut self) {
        self.current = None;
        self.held = None;
    }

    /// Let go of a held picture without showing it.
    pub fn release_hold(&mut self) {
        self.held = None;
    }

    pub fn zoom_is_stale(&self, zoom: f64) -> bool {
        self.current
            .as_ref()
            .is_some_and(|tex| tex.render_zoom != quantize_zoom(zoom))
    }

    pub fn needs_rerender(
        &mut self,
        token: u64,
        zoom: f64,
        now: f64,
        viewport_bounds: &GeoBounds,
        plan: &OverlayTexturePlan,
    ) -> bool {
        self.needs_rerender_with_policy(
            token,
            zoom,
            now,
            viewport_bounds,
            plan,
            mid_gesture_rerender_allowed(),
        )
    }

    /// [`Self::needs_rerender`] with the platform policy as a parameter.
    fn needs_rerender_with_policy(
        &mut self,
        token: u64,
        zoom: f64,
        now: f64,
        viewport_bounds: &GeoBounds,
        plan: &OverlayTexturePlan,
        mid_gesture_band: bool,
    ) -> bool {
        if self.last_seen_zoom != Some(zoom) {
            self.last_seen_zoom = Some(zoom);
            self.zoom_still_since = now;
        }
        // `now >= since + delay` rather than `now - since >= delay`: the two
        // differ only in the last ulp, but `(t + delay) - t` rounds below
        // `delay` for most `t`, so the subtraction form calls a frame that
        // arrives exactly one delay later — the frame the settle repaint
        // schedules — not yet settled and costs it another repaint cycle.
        let settled = now >= self.zoom_still_since + SETTLE_REPAINT_DELAY.as_secs_f64();

        // The newest picture this cache has — see the doc note. A held picture
        // outranks the one on screen: it is what a dispatch would supersede.
        let Some(tex) = self
            .held
            .as_ref()
            .map(|held| &held.data)
            .or(self.current.as_ref())
        else {
            return true;
        };
        if tex.data_generation != token {
            return true;
        }
        // The texture is no longer the size this pane would ask for. Nothing
        // else here can notice that: a display-density change — a window moved
        // to a second monitor, an OS scale setting, a browser zoom — leaves the
        // zoom and the geographic bounds exactly as they were and changes only
        // how many texels a point is worth. Without this the pane would keep a
        // half-density texture for as long as it stayed put.
        if tex.width != plan.width || tex.height != plan.height {
            return true;
        }
        let render_zoom = tex.render_zoom as f64 / ZOOM_QUANTIZATION_FACTOR;
        if mid_gesture_band && (zoom - render_zoom).abs() >= ZOOM_REBUILD_BAND {
            return true;
        }
        if settled && tex.render_zoom != quantize_zoom(zoom) {
            return true;
        }
        pan_exceeds_coverage(&tex.placed.geo, viewport_bounds)
    }
}

/// Whether a zoom that has drifted [`ZOOM_REBUILD_BAND`] from the texture's own
/// may be re-rasterized while the gesture is still moving.
///
/// **On wasm this is a policy hold, not a physical necessity.** The reason
/// originally written here — that the raster would run inline on the frame
/// thread — has been false since the overlay-worker slices landed 2026-08-14;
/// see `rustdar_worker::offload`'s own module doc, where the only inline
/// execution left on wasm is the fallback for a thread that has no sink.
/// A dispatched overlay raster goes to the worker.
///
/// **The re-enable is measured-affordable, and flipping it is not this
/// function's decision to record.** Measured 2026-08-18 on the A9 one-way
/// settle: worker raster 23.2 ms off-thread, delivery 5.3 ms between frames,
/// banding <= 5.2 ms/frame, cadence unbroken. But re-enabling mid-gesture
/// rebuilds is the A9 compositor verdict's own stated re-open trigger, so the
/// flip needs its own order and its own A9 re-run. Until one is done, the
/// `false` stands as a deliberate hold on gesture-time work, and the settle
/// arm is what bounds the resulting softness in time.
fn mid_gesture_rerender_allowed() -> bool {
    !cfg!(target_arch = "wasm32")
}

/// Returns `true` if the viewport has panned far enough outside the texture's
/// geo bounds that a re-render is warranted (PAN_REBUILD_THRESHOLD of margin).
fn pan_exceeds_coverage(texture_bounds: &GeoBounds, viewport_bounds: &GeoBounds) -> bool {
    // The part of the viewport that is on the map at all.
    let view_min_lat = viewport_bounds.min_lat.max(-MERCATOR_LAT_LIMIT);
    let view_max_lat = viewport_bounds.max_lat.min(MERCATOR_LAT_LIMIT);

    let tex_lat_range = texture_bounds.max_lat - texture_bounds.min_lat;
    let tex_lon_range = texture_bounds.max_lon - texture_bounds.min_lon;
    let view_lat_range = view_max_lat - view_min_lat;
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

    // An edge already at the projection's limit has no band to consume, and
    // asking it for one is what spun for ever.
    let south_is_complete = texture_bounds.min_lat <= -MERCATOR_LAT_LIMIT;
    let north_is_complete = texture_bounds.max_lat >= MERCATOR_LAT_LIMIT;

    (!south_is_complete && view_min_lat < texture_bounds.min_lat + margin_lat)
        || (!north_is_complete && view_max_lat > texture_bounds.max_lat - margin_lat)
        || viewport_bounds.min_lon < texture_bounds.min_lon + margin_lon
        || viewport_bounds.max_lon > texture_bounds.max_lon - margin_lon
}

// ── Drawing ──────────────────────────────────────────────────────────────

/// The screen rect a north-west / south-east geographic corner pair covers.
pub fn geo_corner_rect(
    projector: &walkers::Projector,
    nw: (f64, f64),
    se: (f64, f64),
) -> egui::Rect {
    let project = |(lat, lon): (f64, f64)| projector.project(walkers::lat_lon(lat, lon)).to_pos2();
    egui::Rect::from_two_pos(project(nw), project(se))
}

/// The screen rect a placed raster covers. See [`geo_corner_rect`].
pub fn placed_rect(projector: &walkers::Projector, placed: &PlacedRaster) -> egui::Rect {
    geo_corner_rect(
        projector,
        (placed.geo.max_lat, placed.geo.min_lon),
        (placed.geo.min_lat, placed.geo.max_lon),
    )
}

/// Draw an overlay texture as a geo-positioned image on the map.
pub fn draw_overlay_texture(
    painter: &egui::Painter,
    projector: &walkers::Projector,
    tex: &OverlayTextureData,
    screen_rect: egui::Rect,
) {
    let rect = placed_rect(projector, &tex.placed);

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
    rustdar_geo::lat_rad_to_mercator_y(lat_rad)
}

/// Test whether a geographic point (lat, lon) falls inside any polygon of an
/// overlay feature, using the even-odd rule on geo-coordinate rings.
pub fn geo_point_in_feature(lat: f64, lon: f64, feature: &OverlayFeature) -> bool {
    let merc_y = lat_rad_to_mercator_y(lat.to_radians());
    let ring_contains = |point: ScreenPoint, ring: &[(f64, f64)]| {
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
        // `lon` arrives from `Projector::unproject`, which is linear in pixel
        // x and folds nothing, so a click east of the dateline reads e.g. 185
        // while the ring it lands in is stored at -175. The point is moved
        // into the ring's frame rather than the ring into the point's: a point
        // has no shape to deform, and this has to reach the same verdict the
        // rasterizer reached when it shifted the polygon the other way, or a
        // Pacific zone draws where it cannot be clicked.
        let point = match overlay_geo::ring_lon_extent(exterior) {
            Some((rmin, rmax)) => ScreenPoint::new(
                (lon + overlay_geo::lon_shift(lon, lon, rmin, rmax)) as f32,
                merc_y as f32,
            ),
            None => ScreenPoint::new(lon as f32, merc_y as f32),
        };
        if !ring_contains(point, exterior) {
            continue;
        }
        // Inside the exterior — but inside any interior ring means this
        // polygon has a hole here, and the point is outside it. Another
        // polygon of the same feature may still contain the point.
        // The holes take the exterior's shifted point, not their own: a hole
        // shifted by a different turn than the ring it cuts is no longer in
        // that ring.
        if !polygon[1..].iter().any(|hole| ring_contains(point, hole)) {
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

/// A pane keeps its picture until the next one is whole: the four ways a hold
/// ends, and the one thing that must not happen while it lasts.
#[cfg(test)]
mod hold_tests;

/// When a re-render may be dispatched: the settle duration, the platform
/// policy on the mid-gesture band, and the hold as a dispatch already
/// answered.
#[cfg(test)]
mod settle_tests;

#[cfg(test)]
mod geo_click_tests;

#[cfg(test)]
mod texture_budget_tests;
