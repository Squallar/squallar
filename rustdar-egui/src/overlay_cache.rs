//! Texture-based overlay rendering cache.
//!
//! Overlay polygons (SPC outlooks, NWS alerts, mesoscale discussions) are
//! rasterized to RGBA textures on a background thread using tiny-skia, then
//! displayed as geo-positioned images on the map — the same approach used
//! for radar images.  This makes per-frame overlay rendering a single
//! `painter.image()` call per overlay type: truly near-zero cost.

use std::sync::Arc;

use rustdar_overlays::render::geo as overlay_geo;
use rustdar_overlays::render::rasterize::HitMap;
use rustdar_overlays::types::{GeoBounds, OverlayFeature, ScreenPoint};
use rustdar_radar::types::RadarProduct;

// ── Viewport state (reused for render-trigger detection) ─────────────────

/// Fixed-point scale for carrying a zoom level across the render channel.
///
/// A render request travels to a worker as [`crate::actions::GuiAction::
/// RenderOverlay`] and comes back as an `OverlayRenderResponse`, and the zoom
/// rides both legs as an `i32` at `zoom · 32` — encoded by [`quantize_zoom`],
/// decoded by `app_fetch::spawn_overlay_render` for the handlers that scale
/// their symbols by zoom. 32 (= 2⁵) is exact in binary, so the round trip loses
/// only the ~0.031 of a zoom unit it deliberately rounds away.
///
/// # It is not the re-render trigger
///
/// It was, and that is the whole reason the overlay stuttered on zoom.
/// [`OverlayTextureCache::needs_rerender`] compared this quantised value for
/// *equality*, so one step of it — 0.031 zoom units, about 4.7 px of finger
/// travel at the drag sensitivity — asked for a fresh full-size texture: 4.7 Mpx
/// at 1920×1080 and the current [`OVERDRAW_FRACTION`], and 18.7 Mpx before that
/// constant was cut to a quarter. A wheel or pinch zoom is continuous `f64`, so
/// during a gesture that missed on very nearly every frame.
///
/// The trigger is [`ZOOM_REBUILD_BAND`] now, and this constant is back to being
/// what its name says: a wire encoding. Making it *coarser* is no longer a way
/// to trade freshness for renders — it would only blur the zoom the handlers
/// draw at — and making it finer costs nothing.
pub const ZOOM_QUANTIZATION_FACTOR: f64 = 32.0;

/// Quantised zoom, for the render channel. See [`ZOOM_QUANTIZATION_FACTOR`].
fn quantize_zoom(zoom: f64) -> i32 {
    (zoom * ZOOM_QUANTIZATION_FACTOR).round() as i32
}

/// How far the map may zoom away from a texture's own zoom, in zoom units,
/// before the texture is re-rasterized mid-gesture.
///
/// One zoom unit is a factor of two in scale, so this is the whole budget: a
/// texture on screen during a gesture may be magnified up to 2× (soft) or
/// minified up to 2× (aliased) before a fresh one is asked for. That is a
/// deliberate trade and it is the reason the settle render below is not
/// optional — mid-gesture the overlay is *meant* to look approximate, and at
/// rest it is meant to be exact.
///
/// It is safe to relax this far because [`draw_overlay_texture`] projects the
/// texture's stored `geo_bounds` through the *current* projector, so a
/// stale-zoom texture is drawn over the right ground and only its resolution is
/// behind. Nor can it strand the viewport off the texture: see
/// [`pan_exceeds_coverage`], whose containment proof holds for any band and
/// which fires on its own as a zoom-out grows the viewport.
///
/// Two things do not rescale with it, both accepted and both corrected by the
/// settle render:
///
/// * **Strokes.** [`rustdar_overlays::render::rasterize`] thins an outline below
///   a 40 px feature, so a magnified texture shows the stroke it was drawn with
///   rather than the one this zoom would draw. It is a line width, and it lasts
///   as long as the gesture.
/// * **The radar-site plates.** tiny-skia cannot draw text, so the site raster
///   is the pill *behind* each label and egui draws the glyphs over it at a
///   fixed size every frame. Mid-gesture the pill therefore stretches up to 2×
///   under text that does not, which is the one artefact here that is not
///   simply "softer".
///
/// `RadarSites` is nevertheless **in** the band, deliberately. Excluding it is a
/// one-line change and it was considered: the plate is screen-space UI, and the
/// raster draws less than any other layer's. But what a gesture costs is not
/// path building — it is the buffer, and `app_fetch` says why: the site markers
/// cover the whole viewport, so the texture is exactly the size of every other
/// overlay's. Exempting it puts a full-size convert and a full-size
/// `Queue::write_texture` back on the frame thread on every frame of every zoom
/// for anyone with the layer on — 8.7 ms a render at 1920×1080 natively, and on
/// wasm the whole inline raster, measured at 224 ms against a p50 frame of
/// 289.5 ms. That is precisely the stutter this constant exists to remove, so
/// the plate stretches for a tenth of a second instead.
pub const ZOOM_REBUILD_BAND: f64 = 1.0;

/// How long after a zoom stops before the settle render is asked for.
///
/// egui is reactive: it draws a frame when something asks for one. A wheel or
/// pinch gesture's *last* frame is driven by the input event that ended it, and
/// [`OverlayTextureCache::needs_rerender`] decides the gesture has settled by
/// seeing the same zoom twice running — so without something asking for that
/// second frame, the overlay would sit at the gesture's last mid-flight
/// resolution indefinitely. `ui_map_pane` requests a repaint this far out for
/// as long as [`OverlayTextureCache::zoom_is_stale`] is true, which makes the
/// settle a consequence of the code rather than of another frame happening to
/// come along.
///
/// It doubles as the gesture's debounce. During a gesture the input frames
/// arrive well inside this window and supersede it, so the settle costs nothing
/// until the fingers stop; a tenth of a second after they do, one render lands.
pub const SETTLE_REPAINT_DELAY: std::time::Duration = std::time::Duration::from_millis(100);

/// Overdraw the renderer *asks* for, as a fraction of the viewport dimension,
/// on each side of the viewport.
///
/// This is a request, not a promise. A texture wide enough for `f` on both
/// sides is `1 + 2f` viewports across, and no adapter is obliged to allocate
/// one: WebGL2 only guarantees `max_texture_dimension_2d == 2048`, which a
/// viewport wider than 1365 points already blows past. [`plan_overlay_texture`]
/// cuts the fraction back to whatever the adapter can actually hold, and the
/// reduced value — never this constant — is what the rest of the pipeline works
/// from.
///
/// # Why a quarter and not one
///
/// It was `1.0`, which is *nine times the viewport's area*: a 1920×1080 pane
/// planned a 5760×3240 texture, 18.7 Mpx and 74.6 MB, of which — measured at
/// z=7 over a live 273-alert feed — 11% carried any alert at all. A quarter is
/// 2.25× the area, 2880×1620, 18.7 MB.
///
/// The cost that buys back is *texture area*, and it is paid on every single
/// render. **One arm, quoted once**, and the commit that moved this constant
/// quotes the same three pairs: a Ryzen 9 7950X / RTX 3090 (Vulkan) at z=7 over
/// the live 273-alert feed, release + LTO, best-of-7 with the two plans
/// interleaved in one process, and that whole probe interleaved against a
/// binary built at the previous commit.
///
/// | stage                    | 5760×3240 | 2880×1620 |
/// |--------------------------|-----------|-----------|
/// | raster (tiny-skia)       |   92.2 ms |   67.9 ms |
/// | convert to `ColorImage`  |   22.6 ms |    5.7 ms |
/// | `Queue::write_texture`   |   34.8 ms |    8.7 ms |
///
/// The last row is the one the user feels: `write_texture` copies the whole
/// buffer into a staging allocation **on the frame thread** before it returns,
/// so it is 34.8 ms of dropped frames per render at `1.0` and 8.7 at `0.25`.
/// The convert and the upload scale with area — 3.96× and 4.0× against the 4.0×
/// the pixel count demands. The raster does not — only 1.36× — and that is
/// worth knowing rather than glossing: `draw_feature` culls by geo-AABB, so a
/// smaller texture drops the features that fall outside it, but every feature
/// that survives is drawn at the *same* pixel scale in both plans, because the
/// ground shrank with the pixels. Only the fill and blend scale with area; path
/// building does not.
///
/// # What it costs
///
/// Pan re-renders. [`pan_exceeds_coverage`] fires once the viewport has eaten
/// `PAN_REBUILD_THRESHOLD` of the band, which is `0.7 · f` of a viewport: 0.7
/// viewports of travel at `1.0`, 0.175 at `0.25`. Four times as many pan
/// renders, each about four times cheaper on the frame thread — near enough a
/// wash, and the zoom case it buys is not close.
///
/// That wash is a **native** result, and it holds only because native puts just
/// the `write_texture` row on the frame thread — the one row that scales with
/// area exactly. On wasm `rustdar_frontend::offload` has no thread to give:
/// `overlay-render` runs *inline*, so raster, convert and upload are all on the
/// frame thread, and per unit of pan the trade is `4 · (67.9 + 5.7 + 8.7)`
/// against `1 · (92.2 + 22.6 + 34.8)` — about 2.2× worse, because the raster
/// row only fell 1.36×. It bites only where this constant binds rather than the
/// adapter's limit, i.e. a web pane no wider than 1365 points, where the
/// absolute textures are small. A browser measurement on real hardware puts the
/// inline overlay path at 224 ms per raster against a p50 frame duration of
/// 289.5 ms during a gesture, so the web win from *not* re-rasterising per
/// frame dwarfs this — but anything that multiplies web pan cost is worth
/// writing down rather than leaving to be rediscovered.
///
/// It also caps how far a zoom-*out* can coast on a stale texture, which is why
/// this constant and [`OverlayTextureCache::needs_rerender`]'s zoom band have
/// to be read together. Zooming out grows the viewport, so the pan check is
/// what stops it: with a centred viewport that check trips exactly when the
/// viewport's range reaches the texture's, i.e. after `log2(1 + 2f)` zoom units
/// — 1.58 at `1.0`, 0.58 at `0.25`. Zooming *in* shrinks the viewport and is
/// bounded by the zoom band alone.
pub const OVERDRAW_FRACTION: f32 = 0.25;

/// When the accumulated pan exceeds this fraction of the overdraw margin,
/// a fresh render is triggered so the texture stays ahead of the viewport.
///
/// In viewports of travel that is `PAN_REBUILD_THRESHOLD · OVERDRAW_FRACTION`,
/// so it moves with the overdraw and is 0.175 of a viewport at the current
/// pair. The remaining `0.3` of the band is what the old texture keeps covering
/// the viewport with while the new one rasterises.
const PAN_REBUILD_THRESHOLD: f32 = 0.7;

/// Latitude beyond which Web Mercator stops being finite. Bounds are clamped to it
/// rather than allowed to run to the pole.
///
/// [`crate::tiles::MERCATOR_LAT_LIMIT_DEG`], not a second copy of it: the tile
/// grid's edge and the overlay texture's edge are the same edge, and this file
/// read `85.05` while the grid under it ended 0.0011287798° further north —
/// **125.51 m** of meridian. Only ever a clamp bound, so what it cost was a
/// viewport between the two figures being treated as looking past the map when
/// it was still on it; but two numbers for one limit is how the next reader
/// picks the wrong one.
const MERCATOR_LAT_LIMIT: f64 = crate::tiles::MERCATOR_LAT_LIMIT_DEG;

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
    /// Physical pixels per logical point the texture was sized at — the
    /// display density [`plan_overlay_texture`] was handed, after the same
    /// clamping the pixel counts got.
    ///
    /// Load-bearing, not diagnostic, and for the same reason `overdraw` is:
    /// the rasterizer draws markers, label pills and strokes at sizes in
    /// *texels*, and a texture at two texels per point renders every one of
    /// them at half its intended size on screen unless it is told. It travels
    /// with the plan so that the density the pixels were counted at and the
    /// density the symbols are drawn at cannot be two different numbers.
    ///
    /// `1.0` on a display that is not scaled, which is every case before this
    /// field existed.
    pub pixels_per_point: f32,
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
/// # Points in, pixels out
///
/// `screen_rect` is in **logical points** and `max_texture_side` is in
/// **physical texels**, and until `pixels_per_point` was a parameter this
/// function compared the two directly and sized the texture in points. On an
/// unscaled display those are the same number and nothing was wrong; on a 2x
/// display it meant one texel per 2x2 physical pixels — the framebuffer was
/// HiDPI and the overlay drawn into it was not. Measured on a second device: a
/// 1376x755 CSS canvas with a 2752x1510 backing store, every overlay sized
/// 1376x755.
///
/// So the density enters here, once, and everything downstream is in texels.
/// It also makes the `affordable` comparison below honest, which it was not
/// before: it weighed a point count against a texel limit.
///
/// Overdraw is the free variable because the alternative — keeping the full
/// three-viewport coverage and shrinking the pixels — makes the overlay blurrier
/// the wider the window gets, which is exactly backwards. Cutting overdraw keeps
/// one texel per physical pixel and costs only re-render frequency.
///
/// **What that costs where the limit is tight.** A pane wide enough to spend
/// the whole limit on its viewport gets no overdraw and re-renders on every pan
/// step. At one texel per point a 1365-point pane reached that against a 2048
/// limit; at two it is a 683-point pane, which is most of a HiDPI phone
/// browser. That is the deliberate trade and it falls the right way — density
/// first, overdraw second — because a sharp overlay that re-renders more often
/// is recoverable where a permanently soft one is not. It only binds on a
/// device that really reports the WebGL2 floor; Firefox on the development box
/// reports 32768.
///
/// The returned `overdraw` is load-bearing, not diagnostic: it is what the geo
/// bounds get expanded by, so the texture's coverage and its pixel count describe
/// the same rectangle. Expanding by [`OVERDRAW_FRACTION`] after the pixels were
/// clamped would claim ground the texture does not cover, and
/// [`pan_exceeds_coverage`] would then hold off re-rendering over that gap.
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
///
/// Non-radar overlays set `radar_meta: None`. Radar overlays carry the gates
/// a hover reads, site coordinates, and the extent they were projected at,
/// which the per-frame range ring places itself from.
pub struct RadarTextureMeta {
    /// The gates behind these pixels, for the hover readout — see
    /// [`rustdar_radar::hover::HoverSource`]. It replaced a `side²` `f32` grid
    /// of the same numbers resampled up to the raster's resolution.
    pub hover: Arc<rustdar_radar::hover::HoverSource>,
    /// Radar site latitude.
    pub lat: f64,
    /// Radar site longitude.
    pub lon: f64,
    /// The half-width this texture was projected at, km — the renderer's own
    /// answer, which is the sweep's own reach, capped only by
    /// [`rustdar_radar::types::MAX_EXTENT_KM`] and replaced by
    /// `FALLBACK_EXTENT_KM` when the scan states no reach at all.
    ///
    /// It travels with the texture rather than beside it for the same reason
    /// `product` does: a pane-level copy could outlive the pixels it
    /// describes, and geography that outlives its picture places the next one
    /// wrong.
    pub max_range_km: f64,
    /// Where the cut behind these pixels declared its velocity folds, m/s, or
    /// `None` for a raster no single cut is behind — every Level III product,
    /// every volume product, and any volume that declared nothing.
    ///
    /// Travels with the texture for the reason `product` does, and it is the
    /// same failure: a pane-level copy could outlive the pixels it describes,
    /// and a fold limit that outlives its picture annotates the next one with
    /// the previous cut's PRF.
    pub nyquist_ms: Option<f64>,
    /// Where the melting layer these pixels were classified against came from,
    /// or `None` for a raster that classified nothing.
    ///
    /// Travels with the texture for the reason the two above it do, and it is
    /// the one where a pane-level copy would be worst: the layer is per-volume,
    /// so a field on the pane would outlive its object by exactly one volume
    /// and then describe a fresh classification with the previous volume's
    /// provenance — reporting a guess as measured, which is the failure this
    /// value exists to make visible.
    pub melting_layer_source: Option<rustdar_radar::hca::MeltingLayerSource>,
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
    /// The cache token these pixels were rendered for (detects stale results).
    ///
    /// Whatever `ui_map_pane::overlay_cache_token` answered at the time — the
    /// handler's content signature for a fetched overlay, the pane's own
    /// counter for the radar sites. Not the fetch counter: see that function
    /// for why the distinction is worth ~47 ms of frame thread per poll.
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
///
/// # There is no dispatch counter here, deliberately
///
/// There was one — `render_generation`, bumped by a `next_generation()` that
/// never had a caller, so it sat at `0` for the life of the process while
/// `poll_overlay_renders` compared arriving results against it with `<`. That
/// was inert while the value it was compared against was a monotonic counter
/// and `0` was the floor. It stopped being safe to leave lying about once the
/// arriving value became a **content hash** ([`OverlayTextureData::data_generation`]):
/// an ordering comparison over hashes is meaningless in both directions, so
/// wiring the counter up later would have begun discarding or accepting
/// results at random, and swapping the `<` for `!=` against a field nothing
/// writes would have discarded *every* result immediately.
///
/// Nothing is lost by its absence, because staleness here is level-triggered
/// rather than edge-triggered. A result carries the token, zoom and bounds it
/// was rendered for; it is stored with them, and the next frame asks
/// [`Self::needs_rerender`] whether those still describe what the pane wants.
/// A result that arrived late is therefore superseded on the very next frame by
/// the same test that would have asked for it in the first place — no sequence
/// number required.
pub struct OverlayTextureCache {
    /// Currently displayed texture (if any).
    pub current: Option<OverlayTextureData>,
    /// Whether a background render is in progress for this cache.
    pub render_in_flight: bool,
    /// The zoom [`Self::needs_rerender`] was asked about last time, which is
    /// how it knows on the next call whether the gesture has stopped.
    ///
    /// Private, and written by the query itself rather than by a separate
    /// `note_zoom` the caller has to remember, because forgetting to call that
    /// is precisely the permanent-blur failure this design has to make
    /// unreachable: a cache that never sees a second frame at the same zoom
    /// never settles, and the overlay stays soft until something else
    /// invalidates it. Asking the question *is* recording the answer.
    last_seen_zoom: Option<f64>,
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
            last_seen_zoom: None,
        }
    }

    /// Whether this cache's texture is at a zoom other than the map's.
    ///
    /// The one thing a caller needs in order to know a settle render is still
    /// owed, and therefore that another frame has to be asked for — see
    /// `ui_map_pane`. `false` for a cache with no texture: that asks for a
    /// render outright and needs no timer to do it.
    pub fn zoom_is_stale(&self, zoom: f64) -> bool {
        self.current
            .as_ref()
            .is_some_and(|tex| tex.render_zoom != quantize_zoom(zoom))
    }

    /// Check whether a re-render is needed for this overlay.
    ///
    /// Triggers on: cache token change, the zoom drifting a whole
    /// [`ZOOM_REBUILD_BAND`] from the texture's own, the zoom having *settled*
    /// anywhere other than the texture's own, or pan exceeding the overdraw
    /// margin. `token` is `ui_map_pane::overlay_cache_token`'s answer — see
    /// [`OverlayTextureData::data_generation`].
    ///
    /// # Why the zoom test is a band and a settle, and not an equality
    ///
    /// It was an equality on the quantised zoom, and that is what made zooming
    /// with an alert overlay on stutter: one quantisation step is 0.031 zoom
    /// units — about 4.7 px of finger travel — while a wheel or pinch gesture
    /// moves `zoom` continuously as an `f64`. So the test missed on very nearly
    /// every frame of a gesture, and each miss was a fresh full-size raster plus
    /// a `write_texture` on the frame thread.
    ///
    /// Measured over a 2-zoom-unit drag in 2 s at 1920×1080 over the live
    /// 273-alert feed, quoting **renders per second of gesture and frame-thread
    /// milliseconds per second of gesture** throughout, because a per-gesture
    /// count and a per-second time do not compare:
    ///
    /// | arm                                   | zoom in     | zoom out    |
    /// |---------------------------------------|-------------|-------------|
    /// | equality key, `OVERDRAW_FRACTION` 1.0 | —           | 8.0 / 301   |
    /// | equality key, quarter overdraw        | 15.5 / 204  | 19.0 / 248  |
    /// | this band + settle, quarter overdraw  | 1.5 / 20    | 3.0 / 39    |
    ///
    /// The middle row is the one this method changes; the top row is what the
    /// quarter-overdraw commit before it started from, and it is quoted because
    /// cheaper renders alone *raise* the count — capping it is this method's
    /// job, not that constant's.
    ///
    /// The bottom row is 2 renders + 1 settle and 5 + 1 over the 2 s drag.
    ///
    /// The two halves replace it with a claim about *when* the picture has to be
    /// right rather than *how often*:
    ///
    /// - **In motion**, drift up to a whole zoom unit is tolerated. The texture
    ///   is drawn through the current projector either way ([`draw_overlay_texture`]),
    ///   so it is in the geometrically correct place; only its resolution is
    ///   behind, by at most 2× in each direction.
    /// - **At rest**, `settled` — this cache saw the same `zoom` on the previous
    ///   frame — asks for the exact texture, once. That is what makes the
    ///   tolerance above temporary rather than permanent, and it is the half
    ///   whose failure mode is silent: a settle that never fires leaves the
    ///   overlay soft with nothing on screen to say so. It is level-triggered,
    ///   not edge-triggered — `settled` stays true for as long as the map is
    ///   still — so a frame lost to `render_in_flight`, or to a render landing
    ///   late, costs a frame of delay and not the settle itself.
    ///
    /// Relaxing the zoom key cannot strand the viewport off the texture: see
    /// [`pan_exceeds_coverage`], whose containment result holds for any zoom
    /// band at all.
    pub fn needs_rerender(
        &mut self,
        token: u64,
        zoom: f64,
        viewport_bounds: &GeoBounds,
        plan: &OverlayTexturePlan,
    ) -> bool {
        let settled = self.last_seen_zoom == Some(zoom);
        self.last_seen_zoom = Some(zoom);

        let Some(ref tex) = self.current else {
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
        //
        // These two fields were written and never read before this. They are
        // the plan's own output, so the comparison is against what this frame
        // would allocate rather than against any constant.
        if tex.width != plan.width || tex.height != plan.height {
            return true;
        }
        let render_zoom = tex.render_zoom as f64 / ZOOM_QUANTIZATION_FACTOR;
        if (zoom - render_zoom).abs() >= ZOOM_REBUILD_BAND {
            return true;
        }
        if settled && tex.render_zoom != quantize_zoom(zoom) {
            return true;
        }
        // Check if the viewport has panned outside the texture coverage
        pan_exceeds_coverage(&tex.geo_bounds, viewport_bounds)
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
/// # The two ranges are no longer at the same zoom, and it does not matter
///
/// This used to say they were: [`OverlayTextureCache::needs_rerender`] returned
/// early whenever the quantised zoom differed, so `viewport_bounds` was
/// guaranteed to span the same ground per pixel as it did at render time. That
/// sentence stopped being true the moment the zoom key became a band, and it is
/// rewritten rather than relied on.
///
/// Nothing here depended on it. This function does not look at pixels at all —
/// it compares two geographic rectangles, and the containment result below holds
/// whatever scale each was drawn at. What the change means in practice is that
/// `viewport_bounds` may now be *larger* than it was at render time, because the
/// map zoomed out inside the band. That is the case the old sentence dismissed
/// as a pane that had "grown", and the arithmetic handles it identically: a
/// viewport wider than the texture yields a negative band, hence a negative
/// margin, and trips the comparison at once — correct, because the texture no
/// longer covers the viewport.
///
/// So this is also what bounds a stale-zoom texture in the zoom-*out* direction,
/// and the two constants have to be read together: zooming out grows the
/// viewport until this fires, which for a centred viewport is exactly when the
/// viewport's range reaches the texture's — `log2(1 + 2·OVERDRAW_FRACTION)` zoom
/// units, 0.58 at a quarter. [`ZOOM_REBUILD_BAND`] alone bounds zooming *in*,
/// which shrinks the viewport and can never trip this.
///
/// That cap now rests on **longitude alone**, and the reason is the completeness
/// rule below: once a zoom-out has pushed both latitude edges onto the clamp,
/// neither of them can trip anything ever again, so latitude stops bounding the
/// zoom-out entirely. The figure is unaffected — longitude has no clamp and its
/// range scales with the zoom exactly as latitude's did, so the trip still lands
/// at 0.585 zoom units — but it is one axis holding it up rather than two, which
/// is worth knowing before anyone gives longitude a clamp of its own.
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
/// Note what that derivation does *not* assume: nothing about where
/// `view_range` came from, and in particular nothing about it matching the
/// texture's zoom. So this function cannot report "still covered" about a
/// texture that does not in fact contain the viewport — for any threshold
/// anyone picks, and for any zoom band. That is what makes both
/// `PAN_REBUILD_THRESHOLD` and [`ZOOM_REBUILD_BAND`] safe to tune: the
/// stale-overlay failure mode is unrepresentable rather than merely untested.
///
/// # The pole is not a pan, and this used to spin for ever there
///
/// The guarantee above is one-way, and the other way round is the one that
/// bites: nothing in it stops this from answering `true` about a texture that
/// has *just been rendered for this very viewport*. If it does, the pane asks
/// for a render, the render lands, the arriving result asks for a redraw, and
/// the next frame asks again — a closed loop at whatever frame rate the machine
/// can manage, with no input and no decay. Measured in Chromium on the wasm
/// build: ~12 rasters a second, two full-size fills and uploads per frame, held
/// for a three-minute probe.
///
/// Two clamps meeting is what produced it. [`OverlayTexturePlan::coverage`]
/// clamps the texture's latitude to [`MERCATOR_LAT_LIMIT`], because that is
/// where Web Mercator stops; walkers does *not* clamp the map to the world, so
/// once the pane is taller than the world — zoom out far enough, or pan to the
/// top of it — `viewport_geo_bounds` unprojects the pane's corners to latitudes
/// past the same limit. The viewport then names ground the projection cannot
/// draw, the texture is clamped to ground it can, and no render can ever close
/// the gap: `view_max_lat > tex_max_lat` for every texture there will ever be.
///
/// So two things are true of a latitude edge and are handled here rather than
/// left to arithmetic that cannot see them:
///
/// * A viewport beyond [`MERCATOR_LAT_LIMIT`] is looking at nothing. The part
///   past the limit is off the map, so it is clipped away before it is compared
///   — a texture is not stale for failing to cover empty space.
/// * A texture edge *at* the limit is complete. There is no more world to
///   pre-render into on that side, so no pan can consume its band and the
///   margin test does not apply to it.
///
/// Neither weakens the containment result. Clipping only shrinks the viewport,
/// and the skipped edge is skipped exactly when `tex_edge` is at the limit and
/// `view_edge` is clipped to it — so `view_max_lat <= tex_max_lat` holds on that
/// edge by construction rather than by comparison. Longitude needs neither:
/// `coverage` does not clamp it, because the map wraps.
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

    // If viewport extends beyond texture bounds minus the margin threshold, re-render
    (!south_is_complete && view_min_lat < texture_bounds.min_lat + margin_lat)
        || (!north_is_complete && view_max_lat > texture_bounds.max_lat - margin_lat)
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
///
/// [`crate::volume_view::mercator_y_of_lat`], which that module's doc names as
/// *the* spelling of this in this crate — "a second spelling of it there is
/// precisely the drift this seam exists to prevent". This was that second
/// spelling: byte-identical arithmetic, written out again forty lines from a
/// hit test that has to agree with what the renderer drew.
#[inline]
fn lat_rad_to_mercator_y(lat_rad: f64) -> f64 {
    crate::volume_view::mercator_y(lat_rad)
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
