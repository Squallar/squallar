//! Texture-based overlay rendering cache.

use std::sync::Arc;

use squallar_geo::{GeoBounds, PlacedRaster};
use squallar_overlays::render::geo as overlay_geo;
use squallar_overlays::render::rasterize::HitMap;
use squallar_overlays::types::{OverlayFeature, ScreenPoint};
use squallar_source::product::FieldId;

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

/// How long input must be still before an *interaction* counts as over.
///
/// **This is no longer the overlay settle.** Until 2026-08-31 the zoom settle
/// was this duration on a wall clock, and the value was picked to sit above a
/// deliberate wheel gesture's notch gap (~0.35–0.4 s) and below the scripted
/// quiet floor (`gesture_player::QUIET_MIN_SECONDS`, 1.5 s) — a window chosen
/// so a scripted equality gate could hold from either side. What it cost the
/// person zooming was half a second of stretched overlay after their last
/// notch, every time. The settle now reads the gesture's own state instead;
/// see [`ZoomDrive`] and [`SETTLE_QUIET_FRAMES`].
///
/// What still uses it is the work that genuinely wants "the interaction is
/// over, resume the background spend": the tile pump's quiet latch
/// (`crate::ui::map::gesture_quiet`) and the loop refill's throttle
/// (`squallar_app::loop_refill`). Both are budget decisions about a shared
/// thread rather than a question about what is on the glass, and neither is
/// in the path between a person stopping and the picture being right.
pub const SETTLE_REPAINT_DELAY: std::time::Duration = std::time::Duration::from_millis(500);

/// Frames of rest a zoom must show before the settle re-raster is asked for.
///
/// **Frames, not milliseconds, and small.** The wall clock this replaced was
/// 30 frames at 60 Hz and 125 at 250 Hz — the same rule costing a person on a
/// faster display four times more frames of soft overlay. A count is the same
/// rule everywhere.
///
/// The number is not the settle's latency: [`ZoomDrive`] already answers "is
/// the zoom still being driven", so by the time this counts down the gesture
/// is over by egui's own reckoning. This is hysteresis on that answer, and two
/// frames is what one dropped or coalesced input frame costs — a trackpad that
/// reports no movement on a single frame while the fingers are still on it, a
/// digitizer that under-samples. One frame would let that misfire; more than
/// two buys nothing, because a gesture that is really over stays over.
///
/// The perceptual budget it has to fit inside is ~100 ms — the point where a
/// response stops reading as instant. Two frames is 33 ms at 60 Hz and 8 ms at
/// 250 Hz, and the drive signal ahead of it is bounded by egui's own
/// end-of-scroll (150 ms for a discrete wheel, immediate for a trackpad that
/// reports its phases). So the whole path fits, on both.
pub const SETTLE_QUIET_FRAMES: u8 = 2;

/// **How long the cache token must stand still before it stops counting as
/// swept.**
///
/// Frames and not a duration, for the reason [`SETTLE_QUIET_FRAMES`] is: the
/// quantity being judged is *pipeline depth*, and the pipeline is counted in
/// frames. The question this window answers is "will the token move again
/// before a raster asked for now can land", and the deepest pipeline the tree
/// models is a 2-frame raster into a 5-frame upload — 7 frames. Eight is the
/// first value above it, so a token that moves twice inside one round trip is
/// a sweep and one that does not is a nudge.
///
/// **Frame-rate independent in the sense that matters.** At 60 Hz a 10 fps
/// loop crosses an as-of bucket every 6 frames and reads as a sweep; at 250 Hz
/// the same loop crosses one every 25 frames and reads as a nudge — and it is
/// one, because there the raster lands three times over before the next bucket
/// is owed and no upload is ever discarded. The classification and the
/// pathology move together.
const SWEEP_QUIET_FRAMES: u8 = 8;

/// Whether the map's zoom is being driven this frame.
///
/// **The state the settle used to infer from a clock.** The question the settle
/// arm really asks is "is the person still zooming", and every input that could
/// answer it is on the frame's own [`egui::InputState`] — a scroll action egui
/// has not yet called finished, a pinch, fingers on the glass, a drag. Elapsed
/// time was a proxy for all of them, and a proxy that had to be wider than the
/// widest gesture it was standing in for.
///
/// **Whole-window, not per-pane, and deliberately.** egui's input is the
/// window's; a pane cannot be told apart from its neighbour in it. The same
/// choice `crate::ui::map::gesture_quiet` makes, for the same reason.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ZoomDrive(bool);

impl ZoomDrive {
    /// Nothing is moving the zoom this frame.
    pub const AT_REST: Self = Self(false);

    /// A live gesture is moving the zoom this frame.
    pub const LIVE: Self = Self(true);

    /// Read the frame's drive off egui's own input state.
    ///
    /// **`is_scrolling` leads because it is the only term that knows a wheel
    /// gesture is unfinished while nothing is arriving.** egui holds a scroll
    /// action open from the first notch until either the platform's own end
    /// phase (a trackpad, exactly) or 150 ms past the last wheel event (a
    /// discrete wheel, which reports no phases). Between the notch's smoothing
    /// draining and that verdict the deltas below are all zero and the gesture
    /// is still running; without this term the settle would fire inside it.
    ///
    /// The rest are the gestures egui does not route through the wheel: a
    /// pinch (`zoom_delta`), fingers down at all (`any_touches` —
    /// `multi_touch` is `None` for a single finger, and a one-finger drag-zoom
    /// is a gesture this app has), and a drag.
    pub fn of(input: &egui::InputState) -> Self {
        Self(
            input.is_scrolling()
                || input.smooth_scroll_delta != egui::Vec2::ZERO
                || input.zoom_delta() != 1.0
                || input.any_touches()
                || input.pointer.is_decidedly_dragging(),
        )
    }

    /// Whether a gesture is moving the zoom this frame.
    pub fn is_live(self) -> bool {
        self.0
    }
}

/// Overdraw the renderer *asks* for, as a fraction of the viewport dimension,
/// on each side of the viewport.
pub const OVERDRAW_FRACTION: f32 = 0.25;

/// **The least ground an overlay picture covers, as a multiple of the
/// viewport** — and the reason [`plan_overlay_texture`] does not derive the
/// ground and the texel count from the same number.
///
/// Until 2026-09-05 it did. One `scale` set both, so the ladder's overlay
/// oversampling rung bought its bytes back by shrinking the *ground* as well
/// as the resolution, and its bottom rung — 100 percent, `overdraw == 0.0`
/// exactly — left the picture covering precisely the viewport it was
/// rasterised for. [`pan_exceeds_coverage_beyond`] then reduces to four strict
/// inequalities against that viewport offset by [`COVERAGE_DEADBAND_TEXELS`],
/// so **the whole pan margin was one texel** and any drag a person could see
/// re-rasterised every enabled texture layer at full size, for as long as the
/// finger moved.
///
/// Counted on this module's own `gesture_dispatch_tests` rig — 6 layers, 150
/// frames, a pan of **0.05 of a viewport**, one raster frame into a
/// three-frame upload:
///
/// | margin per side | dispatched | superseded |
/// |-----------------|-----------:|-----------:|
/// | 0.25 (rung 150) |      **0** |          0 |
/// | 0.0  (rung 100) |    **288** |    **144** |
///
/// 288 dispatches is 32% of the 900 layer-frames in that run, and it is the
/// same figure the 2-viewport pan produces (294): **the count does not move
/// with how far the pan went**, because the trigger is not distance, it is
/// "the viewport is not exactly where the picture was rasterised for".
///
/// **What this constant costs is nothing.** The ground and the resolution are
/// two numbers now, and only their product is bytes: a picture covering 1.25
/// viewports at 0.8 texels per device pixel has exactly the texel count of one
/// covering 1.0 viewports at 1.0, so `pane_px` and both sides are bit-identical
/// at every rung and `squallar_device_profile::fit::picture_bytes` prices this
/// plan exactly as it priced the old one. At the two upper rungs (150 and 125)
/// the asked scale already exceeds this floor, the resolution is exactly 1.0
/// and nothing whatever changes; only the bottom rung moves.
const MIN_COVERAGE_SCALE: f32 = 1.25;

/// When the accumulated pan exceeds this fraction of the overdraw margin,
/// a fresh render is triggered so the texture stays ahead of the viewport.
///
/// The rest of the band — `1 - PAN_REBUILD_THRESHOLD` of it — is the cover: the
/// ground the pane still has to draw on while the replacement rasterises and
/// uploads. Both directions cost something. Above a half the cover is too
/// short: each texture is dispatched late and the viewport outruns it. Below a
/// half the trigger stops being the binding constraint — dispatch is gated by
/// the previous raster's arrival ([`RendersInFlight`] admits one raster per
/// destination, and a whole-picture layer has one destination), and once the
/// trigger is standing at every promotion
/// the next dispatch supersedes a hold and the brake at the end of
/// [`OverlayTextureCache::needs_rerender_with_policy`] suppresses the one after
/// it — so the extra rebuilds buy no cover and eventually cost some.
///
/// Swept 2026-08-22 against this module's own [`pan_exceeds_coverage_visibly`]
/// and [`OverlayTexturePlan::coverage`], on a 60 Hz loop reproducing the
/// dispatch path (one in-flight raster per pane and layer, `held` consulted by
/// the trigger at raster arrival, `current` replaced only once every upload
/// band has landed), desktop 1920×1080 at full overdraw, raster one frame.
/// **The quantity is counted, not timed, and pan speed is continuous** — the
/// fraction of frames on which nothing the pane holds covers the viewport,
/// averaged over 56 speeds from 0.25 to 3.0 viewports/second:
///
/// | threshold                 | 0.7  | 0.6  | 0.55 |**0.5** | 0.45 | 0.4  | 0.3  |
/// |---------------------------|------|------|------|--------|------|------|------|
/// | dry %, upload 1 frame     |  0.7 |  0   |  0   | **0**  |  0   |  0   |  0   |
/// | dry %, 2 frames (ring)    |  9.9 |  5.5 |  2.2 | **0**  |  0   |  0   |  0   |
/// | dry %, 3 frames (no ring) | 21.2 | 16.0 | 12.4 |**9.5** |  7.3 |  6.0 | 16.4 |
/// | dry %, 4 frames           | 33.9 | 28.4 | 27.3 |**26.4**| 27.2 | 29.9 | 36.8 |
/// | rebuilds/viewport, 3 frames| 5.29| 6.04 | 6.65 |**7.26**| 7.95 | 8.63 |10.44 |
///
/// **The optimum is interior, and it moves with the pipeline depth.** A half is
/// the cheapest threshold that is dry-free at one and two upload frames, and it
/// is the exact minimum at four; only at three is something else better, where
/// 0.40–0.425 costs 6.0% of frames against this value's 9.5%. **No threshold in
/// 0.25–0.70 is at least as good as a half at every depth**, so the constant
/// is not chosen against one of them. It costs no memory — the band is
/// [`OVERDRAW_FRACTION`], which this does not touch.
///
/// **Do not re-derive this from a "maximum sustainable pan" on a ladder of
/// integer frames-per-viewport.** Because the trigger can only fire on a frame
/// and the pipeline is a whole number of frames, `dry == 0` is a *sawtooth* in
/// pan speed, not a step: at 0.45 and a two-frame upload the pane fails from
/// 3.00 viewports/second and is sustainable again from 3.38 to 3.74. Walking
/// such a ladder downwards and taking the first sustainable rung reads the top
/// of a tooth, and the tooth moves with the threshold. The figure that means
/// something is the *first* failure — the speed below which every speed is
/// sustainable — which at a two-frame upload is 3.00 for both 0.45 and 0.5,
/// identical to four decimals.
///
/// **Two things this constant cannot reach.** Delivery, not raster, is most of
/// the latency it is dividing the band against: at a half and a three-frame
/// upload, removing delivery entirely takes that first-failure speed from 1.88
/// to 3.75 viewports/second, where no threshold in the sweep moves it past
/// 2.50. And where the adapter
/// has clamped the overdraw away the band is what shrinks, not this:
/// `pan_exceeds_coverage` measures the band off the texture's real bounds, so a
/// WebGL2 pane at the 2048 floor divides 0.033 of a viewport here and one at 2×
/// device pixels divides zero. **Measured, no threshold recovers that case and
/// none comes close**: the 2048-clamped 1920×1080 pane is dry on 71.4% of
/// frames at 0.3 and 80.6% at 0.7, and on every value between, against 0% at
/// full overdraw — a 9-point spread across the whole sweep and dry-free
/// nowhere in it. See [`COVERAGE_DEADBAND_TEXELS`], which is what protects that
/// case from spending rasters it cannot use, and which changes no cell of the
/// table above.
///
/// **Refused 2026-08-22: keying this on the upload depth** (proposed as a
/// function of the uploader's bands-per-frame). Re-swept on the real gate at a
/// real plan — the 1920×1080 pane at full overdraw, 2880×1620 texels, the same
/// 56 speeds, 33600 counted frames per depth — the dry minimum moves
/// *non-monotonically* with depth: every threshold from 0.3 to 0.6 is dry-free
/// at a one-frame upload, 0.3 to 0.5 at two, 0.40–0.425 is best at three (5.95%
/// against this value's 9.52%), and this value is the exact minimum again at
/// four (26.43%, where 0.40 costs 29.91%). A rule that has to fall and then
/// rise with its own argument is a four-entry table fitted to one rig.
///
/// Depth is also not the ring. It is `ceil(bytes / UPLOAD_BAND_BYTES)` over
/// bands-per-frame, so it moves with the plan as much as with the staging ring
/// — 18.66 MB a picture is two frames on the same ring that gives one frame at
/// 9.44 MB — so keying on the ring alone keys on half the argument. And the
/// ring's depth lives in `squallar-gpu`, which this geometry predicate has no
/// reach into and must not grow one for at most 3.6 points of dry frames on one
/// depth of four, bought at 16.6% more uploaded bytes per viewport panned.
const PAN_REBUILD_THRESHOLD: f32 = 0.5;

/// How far past the coverage trigger the viewport has to be before the picture
/// on screen counts as having run out of margin, in texels of that picture.
///
/// One texel is the smallest pan that can change the replacement: below it every
/// feature falls in the texel it already occupies, so the raster dispatched for
/// the new viewport is the raster already on screen, paid for again at full size.
///
/// **Texels rather than a fraction of the band, because the band can be zero.**
/// Where the adapter has clamped the overdraw away — a pane at the WebGL2 2048
/// floor at 2x device pixels — [`pan_exceeds_coverage`] reduces to four *strict*
/// inequalities against the exact viewport the texture was rasterised for, and
/// any nonzero motion trips them: a viewport wobbling by 1e-9 of a degree
/// dispatched full-size rasters continuously. A deadband priced in band is zero
/// exactly there, which is the case with nothing else protecting it. A texel is
/// ground per texture pixel, and exists whatever the band is.
///
/// **The other deadband in this workspace** is `CENTER_SLACK_POINTS` in
/// `vendor/walkers/src/viewport.rs`, which stops the map's centre clamp
/// re-firing on its own projection round trip and repainting forever. Same
/// reason, different shape, and the difference is worth knowing before
/// reaching for one as a model for the other: that one is already in the unit
/// its comparison is made in — projected points — and is a fixed constant, so
/// it needs no equivalent of [`COVERAGE_DEADBAND_VIEWPORT_CEILING`]. This one
/// is in texels of a picture whose resolution against its own ground varies,
/// which is exactly what that ceiling exists to bound.
const COVERAGE_DEADBAND_TEXELS: f64 = 1.0;

/// Ceiling on [`COVERAGE_DEADBAND_TEXELS`] once it is converted to ground, as a
/// fraction of the viewport span on that axis.
///
/// Nothing else bounds how coarse a texel is, and a deadband is ground a rebuild
/// is withheld over. This is that bound: never more than a thousandth of a
/// viewport of pan. It binds on no texture the app plans — the WebGL2 floor is
/// 2048 texels across a viewport at zero overdraw, so one texel there is half
/// this, and every wider texture is finer still — only on a picture whose
/// resolution against its own ground is degenerate.
///
/// The workspace's other deadband, `CENTER_SLACK_POINTS` in
/// `vendor/walkers/src/viewport.rs`, has no counterpart to this constant and
/// needs none: it is a fixed slack already expressed in the projected points
/// its comparison is made in, so there is no per-picture conversion for a
/// viewport to bound. See [`COVERAGE_DEADBAND_TEXELS`].
const COVERAGE_DEADBAND_VIEWPORT_CEILING: f64 = 1.0 / 1024.0;

/// Latitude beyond which Web Mercator stops being finite. Bounds are clamped to it
/// rather than allowed to run to the pole.
const MERCATOR_LAT_LIMIT: f64 = squallar_geo::MERCATOR_LAT_LIMIT_DEG;

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
    /// The glass this picture covers before the margin, in physical pixels —
    /// the pane rect at the density above, truncated. What the budget
    /// system's host-picture term scales by the oversampling in force
    /// (`squallar_device_profile::fit::picture_bytes`), carried on the plan
    /// so the scene is described from the size the planner was handed and
    /// not re-derived from a rect laid out on some other frame.
    pub pane_px: [u32; 2],
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

/// The overdraw fraction per side the budget's oversampling asks for —
/// `Budgets::overlay_oversample_percent` (150, 125, 100 per side) as the
/// fraction [`plan_overlay_texture`] takes: `(percent - 100) / 200`, so 0.25,
/// 0.125 and 0.0. Exact in `f32` for every entry of the table, each a dyadic
/// rational, which is what keeps the planned side equal to the budget
/// system's integer side to the pixel. A percent under 100 asks for nothing.
pub fn overdraw_for_oversample(percent: u16) -> f32 {
    (f32::from(percent.max(100)) - 100.0) / 200.0
}

/// Size the overlay texture for `screen_rect`, asking for `overdraw` on each
/// side — never more than [`OVERDRAW_FRACTION`] — and giving up overdraw
/// rather than exceeding `max_texture_side`.
pub fn plan_overlay_texture(
    screen_rect: egui::Rect,
    max_texture_side: u32,
    pixels_per_point: f32,
    overdraw: f32,
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
    // The margin asked for: the ladder's, held to the renderer's own ceiling,
    // and nothing for a fraction that is not a number.
    let asked = if overdraw.is_finite() {
        overdraw.clamp(0.0, OVERDRAW_FRACTION)
    } else {
        0.0
    };

    // Largest overdraw this axis can afford: `side * (1 + 2f) == max_side`.
    // Negative when the viewport alone overflows the limit, hence the `max(0.0)`.
    // A zero side divides to `inf`, which `min` discards — no special case needed.
    let affordable = |side: f32| (max_side as f32 / side - 1.0) / 2.0;
    let overdraw = asked
        .min(affordable(screen_w))
        .min(affordable(screen_h))
        .max(0.0);

    let scale = 1.0 + 2.0 * overdraw;

    // ── Ground and resolution, which are two numbers ────────────────────────
    //
    // `scale` above is how much bigger than the viewport this picture is
    // *allowed to be*, and until 2026-09-05 it was spent entirely on ground:
    // the texture covered `scale` viewports at one texel per device pixel. It
    // does not have to be. What costs bytes is the product, so the same texel
    // count buys a picture covering [`MIN_COVERAGE_SCALE`] viewports at
    // proportionally fewer texels per device pixel — and the ground is what
    // the pan trigger is judged against. See [`MIN_COVERAGE_SCALE`] for the
    // counted figures and for why the bytes are unchanged.
    //
    // The delivered scale, not the asked one, because the ceiling below can cut
    // it: `affordable` has already held `overdraw` to what `max_side` allows,
    // but once that floored at zero the arithmetic stopped targeting `max_side`
    // at all and only the `min` below is holding the line. Computed off the
    // untruncated sides so a picture that is under the ceiling — every arm this
    // tree has measured, where `max_texture_dimension_2d` is 8192 or more —
    // divides `scale` by itself exactly and keeps `pixels_per_point` bit-equal
    // to the density it was handed.
    let ceiling = |side: f32| {
        if side > 0.0 {
            max_side as f32 / side
        } else {
            f32::INFINITY
        }
    };
    let delivered = scale.min(ceiling(screen_w)).min(ceiling(screen_h));
    let coverage_scale = delivered.max(MIN_COVERAGE_SCALE);
    let resolution = delivered / coverage_scale;

    // `min(max_side)` is load-bearing, not defensive. It is the *only* thing keeping
    // the primary WebGL2 case legal: once `max(0.0)` has floored the overdraw at
    // zero — a pane at least as wide as the whole limit — `scale` is 1.0 and the
    // arithmetic above no longer targets `max_side` at all. A 3000-point pane
    // against a 2048 limit computes 3000 here and is cut to 2048 by this call.
    OverlayTexturePlan {
        width: ((screen_w * scale) as u32).min(max_side),
        height: ((screen_h * scale) as u32).min(max_side),
        // The ground, which is no longer the same question as the size above.
        overdraw: (coverage_scale - 1.0) / 2.0,
        // The density the *texture* is at, which is what the rasterizer must
        // draw its strokes and glyphs at — `app_fetch`'s `device_scale`. Below
        // the display's own density exactly when the ground was widened past
        // what the byte budget pays for at full resolution.
        pixels_per_point: density * resolution,
        pane_px: [screen_w as u32, screen_h as u32],
    }
}

/// What the whole-picture overlay pipeline has actually spent: rasters asked
/// for, pictures uploaded and bytes with them, and the ones thrown away.
pub mod ledger;

// ── Why a raster was asked for ───────────────────────────────────────────

/// Why one whole-picture overlay raster was dispatched.
///
/// **The margin's justification is a rate, and until this existed nothing
/// could measure it.** Overlay pictures are rasterized oversized — 2.25x the
/// pane's area at [`OVERLAY_OVERSAMPLE_PERCENTS[0]`][percents] — so that a pan
/// can move across the picture's expanded ground without asking for a new one.
/// What that margin buys is exactly the [`Self::PanCoverage`] dispatches it
/// prevents, and the only honest way to price it is to know what share of
/// rasters that arm is responsible for. [`OverlayTextureCache::needs_rerender`]
/// collapsed nine branches into a bare `bool`, so the share was not merely
/// unmeasured — it was unknowable.
///
/// [percents]: squallar_device_profile::constants::OVERLAY_OVERSAMPLE_PERCENTS
///
/// # The denominator
///
/// One variant is counted per [`RendersInFlight::record`], which is the same
/// mark [`ledger::Totals::dispatched`] counts and the same one
/// [`RendersInFlight::admits`] bounds. So the breakdown's denominator **is**
/// `dispatched`, exactly, and [`ledger::Totals::reasons_balance`] is that
/// identity rather than a hope. An ask that `admits` refused is in neither
/// figure: it spent no raster.
///
/// # What separates the two content arms
///
/// [`Self::ContentOneShot`] and [`Self::ContentSweeping`] are the same
/// *branch* — the cache token no longer matches the picture's — split on
/// `OverlayTextureCache::token_sweeping`, which this module already computed
/// for the dispatch brake. A token that moved once and then held still is a
/// one-shot; a token that moves again within [`SWEEP_QUIET_FRAMES`] frames is
/// a clock sweeping its window. That is the loop-versus-data distinction, and
/// it is a **frame-rate-relative** one rather than a semantic one: the first
/// tick of a playing loop is charged to `ContentOneShot`, because at that
/// instant nothing has yet distinguished it from a data arrival, and every
/// tick after it is charged to `ContentSweeping`. A loop playing faster than
/// one token move per `SWEEP_QUIET_FRAMES` frames reads as sweeping for as
/// long as it plays.
///
/// **`ContentOneShot` is not "data arrived".** The cache token mixes the data
/// generation with the theme, the pane's layer settings and the as-of stamp
/// (`squallar_egui::overlay_cache_token`), so a theme flip, a filter change
/// and a units change all land here too. The one variant that *is* an
/// unambiguous data arrival is [`Self::ArrivalDoor`], which is only ever armed
/// by the `SourceEvent::Data` drain.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum RerenderReason {
    /// The pane is drawing nothing for this layer yet — no texture, no hold
    /// and no blank. The first picture after a `clear`, a pane opening, or a
    /// layer being switched on.
    FirstPicture,
    /// The cache token moved once and then held still: data arrived, the
    /// theme flipped, a filter or unit changed, or an as-of stepped a single
    /// bucket. See the type note — this is not exclusively data.
    ContentOneShot,
    /// The cache token is moving on consecutive frames — a playing loop, a
    /// scrub, or a live as-of clock sweeping its window.
    ContentSweeping,
    /// The picture is no longer the size this pane would ask for: a display
    /// density change, a window moved to another monitor, a browser zoom, or
    /// the pane itself being resized. **Not a view movement** — the zoom and
    /// the ground are untouched and only the texel count moved.
    PlanResized,
    /// The zoom drifted [`ZOOM_REBUILD_BAND`] from the picture's own while a
    /// gesture was still running. Off in every shipped build — see
    /// [`MID_GESTURE_REBUILDS`] — and counted so that the policy's cost is
    /// visible if it is ever turned back on.
    ZoomBand,
    /// A gesture settled at a different quantized zoom. The picture the
    /// gesture was stretching is replaced by one rasterized at the zoom the
    /// map came to rest at.
    ZoomSettled,
    /// **The viewport ran off the edge of the picture's expanded ground.**
    /// This is the one arm the oversampling margin exists to prevent, and its
    /// share is what prices the margin.
    PanCoverage,
    /// Dispatched by the arrival drain rather than by a frame — `SourceEvent::
    /// Data` found a pane whose recorded token is stale. Unambiguously a data
    /// arrival: this door does not consult [`OverlayTextureCache::
    /// needs_rerender`] at all.
    ArrivalDoor,
    /// A raster was marked with nothing armed. **Zero in a healthy tree**, and
    /// a nonzero reading is a hole in this wiring rather than a category of
    /// rebuild: every production `record` is reached from one of the two
    /// doors, and both arm. Counted rather than folded into a neighbour so
    /// that the hole is visible instead of silently inflating whichever
    /// variant it was merged with.
    Unattributed,
}

impl RerenderReason {
    /// Every variant, in the order [`Self::index`] assigns.
    pub const ALL: [Self; Self::COUNT] = [
        Self::FirstPicture,
        Self::ContentOneShot,
        Self::ContentSweeping,
        Self::PlanResized,
        Self::ZoomBand,
        Self::ZoomSettled,
        Self::PanCoverage,
        Self::ArrivalDoor,
        Self::Unattributed,
    ];

    /// How many variants there are — the width of the ledger's counter array.
    pub const COUNT: usize = 9;

    /// This variant's slot in [`Self::ALL`] and in the ledger's array.
    pub const fn index(self) -> usize {
        match self {
            Self::FirstPicture => 0,
            Self::ContentOneShot => 1,
            Self::ContentSweeping => 2,
            Self::PlanResized => 3,
            Self::ZoomBand => 4,
            Self::ZoomSettled => 5,
            Self::PanCoverage => 6,
            Self::ArrivalDoor => 7,
            Self::Unattributed => 8,
        }
    }

    /// A short name for a log line. Stable — the Tier-2 rig reads these.
    pub const fn name(self) -> &'static str {
        match self {
            Self::FirstPicture => "first",
            Self::ContentOneShot => "content",
            Self::ContentSweeping => "sweep",
            Self::PlanResized => "resize",
            Self::ZoomBand => "zoom-band",
            Self::ZoomSettled => "zoom-settled",
            Self::PanCoverage => "pan",
            Self::ArrivalDoor => "arrival",
            Self::Unattributed => "unattributed",
        }
    }

    /// Whether the **view moving** is what asked for this raster, as against
    /// content changing under a view that did not move.
    ///
    /// [`Self::PlanResized`] is deliberately **not** here: a density or pane
    /// resize changes how many texels a point is worth and leaves the zoom and
    /// the ground exactly where they were. Folding it in would inflate the
    /// figure this lane exists to measure.
    pub const fn is_view_driven(self) -> bool {
        matches!(self, Self::ZoomBand | Self::ZoomSettled | Self::PanCoverage)
    }

    /// Whether a **larger oversampling margin could have prevented** this
    /// raster.
    ///
    /// Only [`Self::PanCoverage`] can be: it is the arm that fires when the
    /// viewport leaves the picture's expanded ground, and expanding that
    /// ground is what the margin does. A settled zoom asks for a picture at a
    /// different scale, which no amount of margin at the old scale answers.
    pub const fn is_margin_avoidable(self) -> bool {
        matches!(self, Self::PanCoverage)
    }
}

// ── How many rasters may be crossing at once ─────────────────────────────

/// Which of a layer's pictures a raster is on its way to, as an index the
/// layer assigns to its own destinations.
///
/// **Concurrency is over destinations, and never depth within one.** A
/// destination is a [`held`](OverlayTextureCache::hold) slot, and
/// [`OverlayTextureCache::hold`] *replaces* rather than queues: two rasters in
/// flight for the same destination cannot both reach the screen, because the
/// second's arrival throws away the first's upload before its last band lands.
/// Past roughly 2.3x the sustainable pan that closes into a loop with no exit —
/// the freeze
/// `coverage_dispatch_tests::a_fling_the_pipeline_cannot_follow_still_puts_pictures_on_screen`
/// pins, where the unbraked rule spent 300 full-size rasters, threw away all
/// 300 mid-upload and promoted none. Admission therefore allows **one raster
/// per destination**, and the device budget bounds how many *destinations* may
/// be outstanding at once.
///
/// Today a texture layer draws one picture and so has exactly one destination.
/// That is not a limitation this type imposes, and it is why raising the budget
/// changes nothing about what this crate dispatches: a one-destination cache
/// admits one raster at every limit. A tile grid has one destination per tile,
/// and that is what the budget is here to bound.
///
/// **An index and not an enum**, because the destinations a layer has are the
/// layer's own vocabulary — a grid's are its cells — and because a type that
/// can only name one destination makes the bound below untestable: nothing
/// could ever be refused by it, which is a check that cannot fail.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct RenderSlot(u32);

impl RenderSlot {
    /// The layer's whole picture — the only destination a texture layer names
    /// today, and the identity a one-picture layer keeps forever.
    pub const WHOLE: Self = Self(0);

    /// The `n`th of a layer's destinations. `nth(0)` is [`Self::WHOLE`].
    pub const fn nth(n: u32) -> Self {
        Self(n)
    }

    /// The index the layer assigned.
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// The identity of one dispatched raster: where it is going, and what it was
/// asked for.
///
/// **Every term is already on the wire.** `generation` is the render request's
/// `data_generation` and `bounds` is the expanded ground it was asked to cover;
/// the response echoes both, so an arrival can name the dispatch it answers
/// without a second channel — and a grouped dispatch, which is one request sent
/// to several panes, gives every one of those panes the same ticket by
/// construction.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct RenderTicket {
    /// Which of this layer's pictures the raster is for.
    pub slot: RenderSlot,
    /// The cache token the raster was asked at.
    pub generation: u64,
    /// The ground the raster was asked to cover — the *expanded* bounds the
    /// response carries back, not the viewport they were expanded from.
    pub bounds: GeoBounds,
}

impl RenderTicket {
    /// The ticket for a dispatch of the layer's whole picture — the only
    /// destination a texture layer has today. See [`RenderSlot`].
    pub fn whole(generation: u64, bounds: GeoBounds) -> Self {
        Self::for_slot(RenderSlot::WHOLE, generation, bounds)
    }

    /// The ticket for a dispatch of one named destination.
    pub fn for_slot(slot: RenderSlot, generation: u64, bounds: GeoBounds) -> Self {
        Self {
            slot,
            generation,
            bounds,
        }
    }
}

/// The rasters one [`OverlayTextureCache`] has outstanding.
///
/// This is the bounded form of what used to be a single `bool`. The `bool`
/// admitted exactly one dispatch per pane and layer whatever the device could
/// afford; this admits one per destination, up to the device's
/// `Budgets::concurrent_renders` — the same axis every other background render
/// on this device is already spent against.
///
/// # What it costs in memory
///
/// A raster in flight is a raster's bytes in flight, so the transient cost of
/// one cache is `outstanding x plan bytes`, and [`Self::len`] is the counted
/// quantity that bounds it. Nothing *here* measures bytes, so the product below
/// is arithmetic over a quantity tests check rather than a measured figure —
/// but the bytes themselves are now counted, at the two places they are really
/// spent: [`ledger::Totals::picture_bytes`] is what this pipeline hands to
/// egui, and `squallar_gpu::egui_renderer::texture_upload::UploadTotals` is what
/// the device is then made to move. Neither is this product.
///
/// **For every layer the tree draws today the bound is one raster, at every
/// budget**, because a layer that draws one picture has one destination and
/// [`Self::admits`] refuses a second for it: `concurrency_tests::
/// one_destination_admits_one_raster_at_every_budget` is that property. So
/// raising `concurrent_renders` cannot raise what any shipped layer holds. At
/// the 1920x1080 desktop pane's 2880x1620 plan that is 18.66 MB per pane and
/// layer with a raster out, exactly as before this type existed.
///
/// **A layer with more than one destination is bounded by the budget**, and the
/// aggregate is `panes x texture layers x budget x plan bytes` — which the
/// budget alone does not bound, and which a grid must price against its own
/// cell rather than against a whole picture: a 512-pixel RGBA cell is 1.05 MB,
/// an eighteenth of the whole-picture plan above, so six of them outstanding
/// cost a third of one picture.
///
/// The loop pool is a separate account and this does not touch it:
/// [`crate::pane::LoopFrameImage::Overlay`] holds a whole
/// [`OverlayTextureData`] per frame and is bounded by the pool's byte share,
/// not by anything here.
#[derive(Clone, Debug, Default)]
pub struct RendersInFlight {
    /// One entry per destination with a raster out — never two for the same
    /// [`RenderSlot`]. A `Vec` and not a map because the bound is
    /// `MAX_CONCURRENT_RENDERS`, which is 1, 3 or 6: scanning six tickets is
    /// cheaper than hashing one.
    out: Vec<RenderTicket>,
    /// Why the next [`Self::record`] is about to spend a raster, put here by
    /// whichever door decided it. See [`Self::arm`].
    armed: Option<RerenderReason>,
}

impl RendersInFlight {
    /// Whether this cache is waiting for anything at all.
    pub fn is_empty(&self) -> bool {
        self.out.is_empty()
    }

    /// How many destinations have a raster out.
    pub fn len(&self) -> usize {
        self.out.len()
    }

    /// Whether `slot` already has a raster on its way.
    pub fn holds(&self, slot: RenderSlot) -> bool {
        self.out.iter().any(|t| t.slot == slot)
    }

    /// Whether a raster may be dispatched for `slot` with at most `limit`
    /// destinations outstanding.
    ///
    /// Both conjuncts matter and they refuse different things. The first is the
    /// livelock guard — see [`RenderSlot`] — and holds at every limit. The
    /// second is the device budget.
    pub fn admits(&self, slot: RenderSlot, limit: usize) -> bool {
        !self.holds(slot) && self.out.len() < limit
    }

    /// Say why the raster this cache is about to dispatch is being spent.
    ///
    /// **Set as late as the decision allows, and consumed by the very next
    /// [`Self::record`]**, which is what makes the attribution exact rather
    /// than a guess about ordering. Both doors are same-frame ordered against
    /// this latch and neither can read the other's arm:
    ///
    /// 1. `Ingest` — `App::arrived_overlay_asks` arms [`RerenderReason::
    ///    ArrivalDoor`] on each cache it decided for, then
    ///    `App::dispatch_overlay_renders` records them.
    /// 2. The draw pass — `OverlayTextureCache::needs_rerender` arms the arm
    ///    it fired on, or clears the latch where it answered `false`.
    /// 3. `App::process_gui_actions`, on the actions *that same pass* emitted
    ///    (`App::setup_egui_frame` returns them and the drain is the next
    ///    statement), records them.
    ///
    /// So a latch can only survive a frame where the draw pass wanted a raster
    /// and [`Self::admits`] refused it — and on the next frame the draw pass
    /// re-arms from a freshly recomputed decision before anything records, or
    /// the arrival door overwrites it with its own. What can never happen is a
    /// `record` reading a reason from a *different* cache: the latch is per
    /// destination set, which is per pane and layer.
    pub fn arm(&mut self, reason: RerenderReason) {
        self.armed = Some(reason);
    }

    /// Drop a latch nothing is going to spend — the frame decided this cache
    /// is up to date after all.
    pub fn disarm(&mut self) {
        self.armed = None;
    }

    /// What is armed, if anything. For tests; production consumes it at
    /// [`Self::record`].
    pub fn armed(&self) -> Option<RerenderReason> {
        self.armed
    }

    /// Mark a raster as dispatched for `ticket.slot`.
    ///
    /// **Insert-or-replace, not push.** The dispatch paths mark
    /// unconditionally, because an unmarked dispatch is dispatched again on the
    /// next frame; a caller that marks for a slot already carrying one
    /// therefore ends up with the *newer* ticket recorded, which is what makes
    /// the older raster's arrival read as stale at [`Self::retire`] and be
    /// discarded rather than held. So this can never grow the outstanding set
    /// past one entry per destination, however it is called.
    pub fn record(&mut self, ticket: RenderTicket) {
        // Counted here and not at the dispatch that calls it, because this is
        // the mark itself: a dispatch that returned before marking has not
        // asked for a raster, and one that marked twice for a slot really did
        // spend two. Two relaxed `fetch_add`s; see [`ledger`].
        //
        // The reason is **taken**, not read: one arm pays for one raster, and
        // a second `record` with nothing re-armed is [`RerenderReason::
        // Unattributed`] rather than a repeat of the first one's cause. That
        // is the reading that makes a hole in the wiring visible.
        ledger::note_dispatched(self.armed.take().unwrap_or(RerenderReason::Unattributed));
        match self.out.iter_mut().find(|t| t.slot == ticket.slot) {
            Some(slot) => *slot = ticket,
            None => self.out.push(ticket),
        }
    }

    /// Retire the dispatch `ticket` names, and say whether this cache was still
    /// waiting for **that** raster.
    ///
    /// `false` means the result is stale and the caller must discard it rather
    /// than hold it: either the mark was abandoned while the raster flew — the
    /// pane closed, the layer was switched off, the renderer was rebuilt — or
    /// this destination has since been dispatched for a viewport the raster
    /// does not answer. A stale arrival leaves the newer ticket alone: it is a
    /// live dispatch, and retiring it here would let the destination be
    /// dispatched for twice over.
    ///
    /// This is the whole stale-result policy: **the cache accepts a raster only
    /// while it is still the one the cache asked for.**
    pub fn retire(&mut self, ticket: &RenderTicket) -> bool {
        let Some(at) = self.out.iter().position(|t| t.slot == ticket.slot) else {
            return false;
        };
        if self.out[at] != *ticket {
            return false;
        }
        self.out.remove(at);
        true
    }

    /// Forget every outstanding dispatch, whatever it was asked for.
    ///
    /// For the moments where the answer cannot arrive at all: the pane moved to
    /// another index, or the egui context that would hold the pixels is gone.
    /// Whatever is still flying reads as stale at [`Self::retire`].
    pub fn abandon_all(&mut self) {
        self.out.clear();
        // The pane moved or the context died, so whatever a frame decided
        // about this cache is about to be recomputed against a different one.
        // Leaving the latch would charge the next raster to a scene that is
        // gone.
        self.armed = None;
    }
}

// ── Texture cache ────────────────────────────────────────────────────────

/// Radar-specific metadata stored alongside the overlay texture.
#[derive(Clone)]
pub struct RadarTextureMeta {
    /// The gates behind these pixels, for the hover readout — see
    /// [`squallar_radar::hover::HoverSource`]. It replaced a `side²` `f32` grid
    /// of the same numbers resampled up to the raster's resolution.
    pub hover: Arc<squallar_radar::hover::HoverSource>,
    pub lat: f64,
    pub lon: f64,
    /// The half-width this texture was projected at, km — the renderer's own
    /// answer, which is the sweep's own reach, capped only by
    /// [`squallar_radar::types::MAX_EXTENT_KM`] and replaced by
    /// `FALLBACK_EXTENT_KM` when the scan states no reach at all.
    pub max_range_km: f64,
    /// Where the cut behind these pixels declared its velocity folds, m/s, or
    /// `None` for a raster no single cut is behind — every Level III product,
    /// every volume product, and any volume that declared nothing.
    pub nyquist_ms: Option<f64>,
    /// Where the melting layer these pixels were classified against came from,
    /// or `None` for a raster that classified nothing.
    pub melting_layer_source: Option<squallar_radar::hca::MeltingLayerSource>,
    /// Where the storm motion vector these pixels were shifted by came from,
    /// or `None` for a raster that shifted nothing.
    pub storm_motion: Option<squallar_radar::srv::SrvMotion>,
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
/// is a whole placed raster, and cloning it is a refcount on the texture handle,
/// a refcount on the hit map, and the small placement record beside them.
///
/// **The hit map is behind an `Arc` for exactly that sentence.** It used to be
/// an owned [`HitMap`], so every clone of this struct — and every one of the
/// destination panes a single arriving raster names — deep-copied an
/// `FxHashMap<u32, Vec<u32>>` of one entry per touched quarter-cell, on the
/// frame thread.
///
/// Measured on scene E2 (KTLX, every layer, a playing 1 h loop, one pane,
/// RTX 3090 / Vulkan on Xvfb), two legs an arm, counterbalanced ABBA: **215
/// and 205 deep copies a leg, moving 8.4 M and 16.0 M ids**, against none at
/// all once the handle is shared. The denominator is arrivals that CARRY a hit
/// map — the four vector layers, about 17% of the 1224 and 1187 rasters that
/// arrived — and one pane is the FLOOR of the saving, because the copy was per
/// destination pane. An earlier reading of this same cut put it at ~1233 a
/// leg; 1233 is the DISPATCHED raster count, and that reading had conflated
/// arrivals with copies.
///
/// What made it worth finding was a share rather than a count: a campaign
/// instrument timing the frame pump charged this copy **49% of the whole
/// `Apply` walk**, with a worst single copy of 3489 us against a p99
/// interact-frame service bar of 4 ms. That instrument never landed, so the
/// share is not reproducible from this tree; the counts above are.
///
/// `squallar_source::hit` had already made this argument for the *items* half
/// of a hit map and left the cells half owned; this is the same fix, one field
/// over.
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
    pub hit_map: Option<Arc<HitMap>>,
}

impl OverlayTextureData {
    /// What this picture was rendered for, without the pixels — the form the
    /// rebuild gate judges every answer in, picture-backed or blank.
    pub fn shape(&self) -> PictureShape {
        PictureShape {
            placed: self.placed,
            data_generation: self.data_generation,
            render_zoom: self.render_zoom,
            width: self.width,
            height: self.height,
        }
    }
}

/// **What a picture was rendered *for*, with no pixels under it** — every
/// field [`OverlayTextureCache::needs_rerender_with_policy`] judges a picture
/// by, and nothing else.
///
/// It exists because one of the two answers a rasterizer can give has no
/// pixels to judge: a raster that painted nothing is a *clear*, and the pane
/// that took it is as up to date as one holding a picture. Without a shape to
/// remember, the gate's `else { return true }` would re-ask for that same
/// empty picture on the very next frame and every frame after — a dispatch
/// storm on exactly the layers that cost the least to draw.
///
/// Read [`OverlayTextureData::shape`] for the picture-backed half. The two are
/// the same five fields, which is why the gate takes this and never the
/// texture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PictureShape {
    pub placed: PlacedRaster,
    /// The cache token these pixels — or this absence of them — answer.
    pub data_generation: u64,
    pub render_zoom: i32,
    pub width: u32,
    pub height: u32,
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
    /// **The picture the pane keeps drawing while the next one crosses to the
    /// GPU** — the whole of "a pane keeps its picture until the next one is
    /// whole".
    ///
    /// What each of `show`, `show_blank`, `clear` and `release_hold` does to a
    /// hold is the difference between a pane that flickers, one that goes
    /// stale and one that goes empty. [`Self::hold`] starts one,
    /// [`Self::take_held_if_delivered`] ends one the ordinary way, and
    /// `hold_tests` covers the four ways it can end.
    ///
    /// **A live gesture does not release a hold; it stands down the dispatch
    /// that would replace it** — `rerender_reason_with_policy` answers `None`
    /// while this is `Some`. `release_hold`'s only caller is a renderer
    /// rebuild. So a picture that vanishes mid-gesture was replaced by an
    /// arriving blank; it was not a hold let go of underneath it.
    held: Option<HeldOverlayTexture>,
    /// **The answer that painted nothing**, and the reason it is kept rather
    /// than discarded.
    ///
    /// A raster with no ink in it is not a failure and not an absence of an
    /// answer: it is the statement that this layer draws nothing here, and it
    /// has to *replace* whatever the pane was drawing or stale ink stays on
    /// the glass. [`Self::show_blank`] is that replacement, and it costs no
    /// buffer, no texture and no upload — the picture is never built.
    ///
    /// What is left over is the bookkeeping the picture used to carry: the
    /// token, the zoom, the plan size and the ground it covers, which is
    /// exactly [`PictureShape`]. Kept here so
    /// [`Self::needs_rerender_with_policy`] can tell "this pane is up to date
    /// and the answer was empty" from "this pane has never been answered",
    /// which are the same reading against [`Self::current`] alone and would
    /// put every blank layer into a re-dispatch loop.
    ///
    /// Mutually exclusive with [`Self::current`] by construction: every
    /// setter of one clears the other.
    blank: Option<PictureShape>,
    /// Whether a picture that was still crossing to the GPU has already been
    /// thrown away since the last one reached the screen. Cleared the moment
    /// [`Self::held`] empties by any route; read by
    /// [`Self::needs_rerender_with_policy`], which brakes **two** arms on it —
    /// content and coverage. The content arm joined it on measurement: 91.5%
    /// of a playing loop's superseded uploads were attributable to the as-of
    /// half of the token, and that arm's own note carries the figures.
    ///
    /// **Not the same quantity as [`ledger::Totals::superseded`], and the
    /// difference runs the opposite way to the obvious guess.** They rise on
    /// the same condition at the same instant — `squallar-app`'s overlay
    /// arrival asks `is_holding()` and calls
    /// [`ledger::note_superseded`] immediately before the [`Self::hold`] whose
    /// `|=` sets this flag — so neither is a subset of the other *by
    /// condition*. They differ by **coverage**: radar's own arrival holds
    /// through `PaneState::place_radar_raster` without touching the ledger, so
    /// this flag can be set on a pane the counter never counted. A leg reading
    /// a large `superseded` beside a fixture reading none is therefore not two
    /// denominators over one population; it is almost always a **synchronous
    /// fixture**, whose `deliver_held_rasters` empties [`Self::held`] before
    /// the next arrival and so can never reproduce the condition at all.
    hold_superseded: bool,
    /// The rasters this pane has asked for and not yet been answered, bounded
    /// by the device's `Budgets::concurrent_renders`. Replaced a `bool` that
    /// admitted exactly one dispatch per pane and layer whatever the device
    /// could afford; see [`RendersInFlight`] and [`RenderSlot`].
    pub renders: RendersInFlight,
    /// The zoom [`Self::needs_rerender`] was asked about last time, which is
    /// how it notices the zoom moving and re-arms [`Self::settle_owed_frames`].
    last_seen_zoom: Option<f64>,
    /// The cache token [`Self::needs_rerender`] was asked about last time —
    /// the zoom field's twin on the content axis, and how this cache notices
    /// that the token is *moving* rather than that it has moved.
    last_seen_token: Option<u64>,
    /// Frames the token has stood still for, saturating at
    /// [`SWEEP_QUIET_FRAMES`]. Reset by every move.
    token_still_frames: u8,
    /// **Whether the token is being swept rather than nudged**: it moved, and
    /// the move before it was inside [`SWEEP_QUIET_FRAMES`].
    ///
    /// A playing loop sweeps it — every
    /// [`TimeAxis::EventLifetime`](squallar_source::time::TimeAxis::EventLifetime)
    /// layer re-tokenizes per as-of bucket and the pane clock crosses buckets
    /// continuously — and so does a dragged scrubber. Data arriving, a theme
    /// flip and a filter change do not.
    token_sweeping: bool,
    /// Whether an upload has been thrown away *during the current sweep*.
    ///
    /// **[`Self::hold_superseded`] cannot answer this and that is the whole
    /// reason this field exists.** That flag is cleared by every delivery, so
    /// a pane under a sweeping clock re-learns the same lesson once per
    /// promotion for ever: dispatch, discard, brake, land, unbrake, dispatch,
    /// discard. Measured on the rig in `clock_sweep_tests`, that is **one
    /// discarded upload per picture promoted** — 150 discards against 150
    /// promotions over 600 frames at one bucket per frame, a 3-frame upload
    /// and a 1-frame raster. This one is not cleared by delivery; it is
    /// cleared when the sweep ends.
    sweep_discarded: bool,
    /// Frames of rest still owed before the zoom counts as settled. Re-armed
    /// to [`SETTLE_QUIET_FRAMES`] by any live [`ZoomDrive`] or any zoom
    /// movement, decremented by one per frame the pane asks, and the settle
    /// fires at zero. See [`Self::settle_is_counting_down`].
    settle_owed_frames: u8,
    /// **Whether the picture in [`Self::current`] is still crossing to the
    /// GPU.** [`Self::held`]'s twin for the one case a hold cannot describe.
    ///
    /// A hold exists so a *replacement* does not appear half-uploaded over
    /// the picture already on the glass. When there is no picture on the
    /// glass there is nothing to protect, so
    /// [`crate::pane::PaneState::place_radar_raster`] shows the arriving
    /// raster at once and it fills top-down as its bands land — which is the
    /// behaviour, not a defect.
    ///
    /// What that costs is an accounting hole: the picture is whole in
    /// `squallar_gpu`'s `TextureUploads::pending`, holding all of itself on
    /// the host, and [`Self::is_holding`] reads false for it. Every pane's
    /// FIRST radar picture takes that arm, so on a resume — the batch the
    /// plan-view door exists for — the door's occupancy term was
    /// structurally zero for the whole burst. This flag is the missing half.
    ///
    /// Set only by [`Self::show_arriving`], cleared by every other route into
    /// [`Self::current`] and by [`Self::settle_arrival`] on the frame the
    /// renderer says every band has landed.
    showing_arriving: bool,
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
            blank: None,
            hold_superseded: false,
            renders: RendersInFlight::default(),
            last_seen_zoom: None,
            last_seen_token: None,
            // Full, so the first token this cache is ever asked about reads as
            // a move that follows nothing — a fresh cache is not mid-sweep.
            token_still_frames: SWEEP_QUIET_FRAMES,
            token_sweeping: false,
            sweep_discarded: false,
            settle_owed_frames: 0,
            showing_arriving: false,
        }
    }

    pub fn current(&self) -> Option<&OverlayTextureData> {
        self.current.as_ref()
    }

    /// Put `data` on screen now, and let go of anything being held.
    ///
    /// **The picture is whole.** Every caller of this is a promotion or a
    /// picture the renderer took across on its own frame's queue, so nothing
    /// of it is still in the band queue; [`Self::show_arriving`] is the same
    /// swap for a picture that is.
    pub fn show(&mut self, data: OverlayTextureData) {
        self.held = None;
        self.hold_superseded = false;
        self.blank = None;
        self.showing_arriving = false;
        self.current = Some(data);
    }

    /// Put `data` on screen now **while its bands are still crossing**, and
    /// remember that they are.
    ///
    /// The pane paints it as it fills, top-down, which is what a first paint
    /// is; see [`Self::showing_arriving`] for why the remembering is not
    /// optional.
    pub fn show_arriving(&mut self, data: OverlayTextureData) {
        self.show(data);
        self.showing_arriving = true;
    }

    /// Whether the picture on the glass is still crossing to the GPU.
    pub fn is_showing_arriving(&self) -> bool {
        self.showing_arriving
    }

    /// The id of the picture on the glass while it is still crossing.
    pub fn showing_arriving_id(&self) -> Option<egui::TextureId> {
        self.showing_arriving
            .then(|| self.current.as_ref().map(|data| data.texture.id()))
            .flatten()
    }

    /// Stop charging the picture on the glass once `delivered` says every one
    /// of its bands has landed.
    ///
    /// The falling half of [`Self::show_arriving`], and it has to exist for
    /// [`Self::hold`]'s reason: a level that only ever rises latches, and a
    /// latched occupancy shuts the plan-view door for the life of the session.
    pub fn settle_arrival(&mut self, delivered: impl Fn(egui::TextureId) -> bool) {
        if let Some(id) = self.showing_arriving_id()
            && delivered(id)
        {
            self.showing_arriving = false;
        }
    }

    /// **Take an answer that painted nothing**: the pane stops drawing this
    /// layer, now, and `shape` is what the rebuild gate judges it by until the
    /// next answer.
    ///
    /// This is [`Self::show`] for the picture that was never built. It clears
    /// rather than draws, which is the whole of the behaviour a blank picture
    /// used to buy at the price of a full-size transparent RGBA buffer, a
    /// texture and an upload of it.
    ///
    /// **It clears a hold too**, and immediately rather than on the frame the
    /// last band lands: a hold exists so the picture on screen is not replaced
    /// by a half-uploaded one, and there is no upload here to be half of.
    /// The caller counts that discarded hold — see `ledger::note_superseded`.
    pub fn show_blank(&mut self, shape: PictureShape) {
        self.held = None;
        self.hold_superseded = false;
        self.current = None;
        self.showing_arriving = false;
        self.blank = Some(shape);
    }

    /// Whether the last answer this cache took painted nothing.
    pub fn is_blank(&self) -> bool {
        self.blank.is_some()
    }

    /// Hold `data` until its pixels have all reached the GPU.
    pub fn hold(&mut self, data: OverlayTextureData, data_time: Option<chrono::NaiveDateTime>) {
        // Replacing a hold discards an upload that had already started, and the
        // coverage arm is not allowed to do that twice running.
        let discarding = self.held.is_some();
        self.hold_superseded |= discarding;
        // Recorded here rather than read off `hold_superseded` at the gate,
        // because that flag is cleared by a delivery that may fall between two
        // asks and the discard would then never be seen at all. See
        // [`Self::sweep_discarded`].
        self.sweep_discarded |= discarding;
        self.blank = None;
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
        self.hold_superseded = false;
        self.held.take()
    }

    /// Forget the picture on screen and anything being held for it.
    pub fn clear(&mut self) {
        self.current = None;
        self.held = None;
        self.blank = None;
        self.hold_superseded = false;
        self.sweep_discarded = false;
        self.showing_arriving = false;
    }

    /// Let go of a held picture without showing it.
    pub fn release_hold(&mut self) {
        self.held = None;
        self.hold_superseded = false;
    }

    pub fn zoom_is_stale(&self, zoom: f64) -> bool {
        self.current
            .as_ref()
            .is_some_and(|tex| tex.render_zoom != quantize_zoom(zoom))
    }

    /// Whether this cache is still running the settle countdown down.
    ///
    /// **The pane must ask for another frame while this is true.** The
    /// countdown is counted in frames, and egui stops asking for frames of its
    /// own accord as soon as a wheel notch's smoothing has drained — which is
    /// before it calls the scroll action finished. Without a frame the
    /// countdown does not run, and the picture stays soft for as long as
    /// nothing else happens to wake the app.
    pub fn settle_is_counting_down(&self) -> bool {
        self.settle_owed_frames > 0
    }

    /// Whether a raster is owed for this frame's viewport, zoom and content.
    ///
    /// **One call is one frame.** The settle countdown moves here, so asking
    /// twice for the same frame spends two frames of it and a frame the pane
    /// skipped is a frame the countdown never sees.
    pub fn needs_rerender(
        &mut self,
        token: u64,
        zoom: f64,
        drive: ZoomDrive,
        viewport_bounds: &GeoBounds,
        plan: &OverlayTexturePlan,
    ) -> bool {
        self.needs_rerender_with_policy(
            token,
            zoom,
            drive,
            viewport_bounds,
            plan,
            MID_GESTURE_REBUILDS,
        )
    }

    /// [`Self::needs_rerender`] with the platform policy as a parameter.
    ///
    /// **Arming is done here rather than at either caller**, so that every
    /// path which can answer `true` charges the raster it is about to cause,
    /// and a `false` clears a latch an earlier frame left behind. See
    /// [`RendersInFlight::arm`] for the ordering that makes the latch exact.
    fn needs_rerender_with_policy(
        &mut self,
        token: u64,
        zoom: f64,
        drive: ZoomDrive,
        viewport_bounds: &GeoBounds,
        plan: &OverlayTexturePlan,
        mid_gesture_band: bool,
    ) -> bool {
        match self.rerender_reason_with_policy(
            token,
            zoom,
            drive,
            viewport_bounds,
            plan,
            mid_gesture_band,
        ) {
            Some(reason) => {
                self.renders.arm(reason);
                true
            }
            None => {
                self.renders.disarm();
                false
            }
        }
    }

    /// **Why** a raster is owed for this frame's viewport, zoom and content,
    /// or `None` where none is.
    ///
    /// This is the body [`Self::needs_rerender_with_policy`] wraps, and it
    /// carries every one of that gate's rules unchanged — what it adds is that
    /// each `true` now names the arm it came from. See [`RerenderReason`] for
    /// why the naming is the point.
    ///
    /// **One call is one frame**, exactly as before: the settle countdown and
    /// the sweep clock both advance here, so this must be called once per
    /// frame per cache and never speculatively.
    fn rerender_reason_with_policy(
        &mut self,
        token: u64,
        zoom: f64,
        drive: ZoomDrive,
        viewport_bounds: &GeoBounds,
        plan: &OverlayTexturePlan,
        mid_gesture_band: bool,
    ) -> Option<RerenderReason> {
        // Two things re-arm the countdown and they refuse different frames.
        // The drive is the gesture still running — a wheel action egui has not
        // called finished, fingers on the glass — and it covers the frames
        // where the zoom is bit-identical because the input was coalesced or
        // the smoothing has drained. The zoom moving covers everything that
        // moves the zoom without a gesture at all: a keyboard step, a restored
        // viewport, a pane following another.
        if drive.is_live() || self.last_seen_zoom != Some(zoom) {
            self.last_seen_zoom = Some(zoom);
            self.settle_owed_frames = SETTLE_QUIET_FRAMES;
        } else {
            self.settle_owed_frames = self.settle_owed_frames.saturating_sub(1);
        }
        let settled = self.settle_owed_frames == 0;

        // The same shape one axis over: is the *token* moving, and not merely
        // has it moved. Unconditional and above every early return below, for
        // the reason the settle countdown is — one call is one frame, and a
        // frame this cache skipped is a frame the sweep never sees.
        if self.last_seen_token != Some(token) {
            self.token_sweeping = self.token_still_frames < SWEEP_QUIET_FRAMES;
            self.last_seen_token = Some(token);
            self.token_still_frames = 0;
        } else {
            self.token_still_frames = self.token_still_frames.saturating_add(1);
            if self.token_still_frames >= SWEEP_QUIET_FRAMES {
                // The sweep is over, and with it the lesson it taught: the
                // next one is judged on its own pipeline, not on this one's.
                self.token_sweeping = false;
                self.sweep_discarded = false;
            }
        }

        // The newest picture this cache has — see the doc note. A held picture
        // outranks the one on screen for every question below about *what the
        // next picture should be*: it is what a dispatch would supersede. The
        // coverage arm at the end asks a different question and takes a
        // different picture; see there.
        //
        // **And the blank answer counts as a picture here**, which is the
        // whole reason [`Self::blank`] is kept: it is the newest answer this
        // cache has, it was rendered for a token, a zoom and a viewport like
        // any other, and a pane that has one is exactly as up to date as a
        // pane holding pixels. Falling through to the `return true` below
        // would re-ask for it every frame for ever.
        let Some(tex) = self
            .held
            .as_ref()
            .map(|held| held.data.shape())
            .or_else(|| self.current.as_ref().map(OverlayTextureData::shape))
            .or(self.blank)
        else {
            return Some(RerenderReason::FirstPicture);
        };
        // ── Content ─────────────────────────────────────────────────────────
        //
        // The pixels are for another token, so a new picture is owed —
        // **unless this pane has already thrown one away and is still waiting
        // on its replacement.** That last clause is the brake, and it is the
        // same one the coverage arm carries at the end of this function; what
        // follows is why the content arm now carries it too, because this
        // module used to say in as many words that it did not.
        //
        // **The measurement that reversed it** (native scene E2, 1 pane KTLX
        // at 10 fps with the full stack — six `TimeAxis::EventLifetime`
        // texture layers — 100 s legs, two runs per arm, interleaved against
        // a busy box so drift is common-mode). One binary, run against itself
        // with the as-of half of the token frozen:
        //
        // | figure          | as-of live | as-of frozen | attributable |
        // |-----------------|-----------:|-------------:|-------------:|
        // | dispatched      |       3089 |          811 |        73.7% |
        // | picture bytes   |   55.58 GB |     14.58 GB |        73.8% |
        // | **superseded**  |       1419 |          120 |    **91.5%** |
        // | frames presented|       6064 |         6809 |      +12.3%¹ |
        //
        // ¹ frames the app presented in the same 100 s; the count and not a
        // percentile, because `frame segments`' `prepare` p99 sits on the
        // instrument's top bin (38,055 us in three legs of four) and cannot
        // resolve the arms at all.
        //
        // So **44% of the pictures this pane rasterized under a playing loop
        // were handed to a cache already holding one** — uploaded, then
        // discarded before a single band of them was drawn. That is the
        // pathology [`RenderSlot`]'s note describes for the coverage arm and
        // the fling fixture pins: spend a raster, throw away the upload,
        // promote nothing. It was measured here on the *content* arm, which
        // is why the exemption this module used to grant it is gone.
        //
        // **What keeps it from dropping content the pipeline could deliver.**
        // The brake is a statement about delivery and never about rate: it
        // needs a hold that was already superseded AND one still in flight.
        // Both clear the moment [`Self::take_held_if_delivered`] sees a
        // picture land, so a pipeline that keeps up is never braked once —
        // the 1 fps end of the Speed slider, a synchronous test pipeline, and
        // every live pane that re-tokenizes once rather than continuously all
        // dispatch exactly as they did before. What it removes is the second
        // and later dispatch into a pane that has *demonstrated* it cannot
        // land the first.
        if tex.data_generation != token {
            // **A sweeping token is not the same event as a token that moved
            // once**, and the difference is what a dispatch can possibly
            // achieve. A one-shot move — data arrived, the theme flipped, a
            // filter changed — is finished: the raster asked for now is the
            // last one this stimulus owes, and pipelining it into a pane that
            // is still landing a picture gets it on the glass a cycle sooner.
            // A sweep owes another raster on the next frame and the frame
            // after; a dispatch into a pending hold can only *replace* that
            // hold when it comes back, and its own replacement is owed before
            // it lands. That is the fling with no exit, run by the clock
            // instead of by a finger.
            //
            // The refusal is narrower than "never overlap a hold", because
            // overlapping is only wrong where it really does throw an upload
            // away. A pipeline whose raster lands exactly as the hold
            // delivers never discards anything, and it keeps every dispatch
            // it had: measured on `clock_sweep_tests`' rig, a 2-frame raster
            // into a 2-frame upload at one bucket per frame spends 300
            // rasters, discards 0 and promotes 300, and it does so under this
            // rule too. What is refused is the pane that has *demonstrated* a
            // discard during this sweep — see [`Self::sweep_discarded`].
            // The two content exits keep their conditions exactly; what is
            // added is the name. `token_sweeping` is the same flag the brake
            // above is written on, so the loop-versus-one-shot split costs
            // nothing to compute and cannot disagree with the brake it sits
            // beside. The first exit is reachable only while sweeping, so it
            // has one name.
            if self.token_sweeping && self.sweep_discarded {
                return self
                    .held
                    .is_none()
                    .then_some(RerenderReason::ContentSweeping);
            }
            return (!(self.hold_superseded && self.held.is_some())).then_some(
                if self.token_sweeping {
                    RerenderReason::ContentSweeping
                } else {
                    RerenderReason::ContentOneShot
                },
            );
        }
        // The texture is no longer the size this pane would ask for. Nothing
        // else here can notice that: a display-density change — a window moved
        // to a second monitor, an OS scale setting, a browser zoom — leaves the
        // zoom and the geographic bounds exactly as they were and changes only
        // how many texels a point is worth. Without this the pane would keep a
        // half-density texture for as long as it stayed put.
        if tex.width != plan.width || tex.height != plan.height {
            return Some(RerenderReason::PlanResized);
        }
        let render_zoom = tex.render_zoom as f64 / ZOOM_QUANTIZATION_FACTOR;
        if mid_gesture_band && (zoom - render_zoom).abs() >= ZOOM_REBUILD_BAND {
            return Some(RerenderReason::ZoomBand);
        }
        if settled && tex.render_zoom != quantize_zoom(zoom) {
            return Some(RerenderReason::ZoomSettled);
        }

        // ── Coverage ────────────────────────────────────────────────────────
        //
        // Everything above asks what the *next* picture should be, and takes
        // the held one because that is what a dispatch would supersede.
        // Coverage is not that question. It asks whether the pane is about to
        // run off the edge of what it is **drawing**, and what it is drawing is
        // `current` until [`Self::take_held_if_delivered`] has seen every band
        // land — 2 frames on a device with a staging ring and 3 without.
        //
        // Three things have to hold before a raster is worth asking for. All
        // three were swept together on the 60 Hz dispatch loop this module's
        // `PAN_REBUILD_THRESHOLD` note describes, extended to supersede a hold
        // the way [`Self::hold`] really does — five pipeline shapes (raster 0-2
        // frames x upload 2-5) against nine thresholds — and the three of them
        // together change no sustainable pan speed anywhere on that grid, in
        // either direction, at any threshold at or above 0.3. What the tree
        // itself still checks is in `coverage_dispatch_tests`.
        // `blank` beside `current` for the reason it is beside them above: the
        // pane really is drawing that answer, and it is the answer's own
        // ground the margin is judged against.
        let displayed = self
            .current
            .as_ref()
            .map(OverlayTextureData::shape)
            .or(self.blank)
            .unwrap_or(tex);

        // **While the view is still moving, the margin is not the trigger.**
        //
        // Every other arm of this gate is gated on the settle or on a policy
        // that stands the gesture down — the zoom band arm by
        // [`MID_GESTURE_REBUILDS`], the zoom arm by `settled` itself. This arm
        // never was, and that asymmetry is the defect: with mid-gesture
        // rebuilds off, the zoom axis went quiet during a gesture while the pan
        // axis stayed wide open and fired on every frame it could get a
        // dispatch admitted. Counted on `gesture_dispatch_tests`, a pan of 0.05
        // of a viewport with the zoom bit-identical throughout — so no other arm
        // can be what answered — spent **294 rasters across 6 layers and 150
        // frames and threw away 144 of them**. A pan of 2 whole viewports on the
        // same rig spent 294, and a ten-notch zoom-out spent 294. **The trigger
        // was not distance; it was motion**, and three gestures crossing 0.05,
        // 2.0 and 1023x of ground costing the identical figure is what says so.
        //
        // So mid-gesture the question becomes the one the viewer can actually
        // answer: **is the pane about to draw nothing**, rather than is it
        // short of the margin it keeps for later. The margin exists to have a
        // replacement ready before the old picture runs out, and mid-gesture
        // there is no "before": a raster asked for now answers a viewport the
        // gesture has already left. The moment the gesture ends the full margin
        // rule below applies again unchanged, and that is what keeps a settled
        // pane's texture ahead of a sustained pan.
        //
        // **The drive and not the settle countdown**, which are different
        // questions and this is the one worth asking. The countdown is
        // hysteresis on top of the drive, and it is re-armed by any zoom this
        // cache has not seen before — including the very first zoom a fresh
        // cache is asked about, which is a cold start and not a gesture. What
        // this arm is about is a person's hand still being on the map, and
        // [`ZoomDrive`] is exactly that. The two frames they differ by fall on
        // the settled side, where the full rule below applies and can spend at
        // most one raster per layer, at the position the gesture ended.
        if drive.is_live() {
            // **And nothing goes out while a picture is already crossing to
            // this destination.** A dispatch made while the view is still
            // moving supersedes that hold by construction — the replacement is
            // owed before the first one can land — which is the freeze
            // [`RenderSlot`] describes, run by a finger instead of by a fling.
            // The in-flight half of the same rule is already refused one layer
            // up: [`RendersInFlight::admits`] declines a second raster for a
            // destination that has one out, so `held` is the half that was
            // still getting through.
            if self.held.is_some() {
                return None;
            }
            // Deadbanded, for the reason the margin arm below is: a viewport
            // wobbling by less than one of the picture's own texels rasterises
            // the picture it already has, and at a coverage floor that is still
            // the whole of the margin on the axis a clamp ate.
            return coverage_is_exhausted_visibly(&displayed, viewport_bounds)
                .then_some(RerenderReason::PanCoverage);
        }

        // **What is on screen has run out of margin**, by at least a texel of
        // itself. While it still has some, nothing is dispatched, whatever a
        // picture the viewer cannot see yet may have run out of. The two
        // disagree wherever a pan reverses: the hold was rasterised for a
        // viewport the map has already left, so it can be the one short of
        // margin while the picture being drawn still has room, and dispatching
        // there throws away an upload the viewer had no need of.
        //
        // The deadband is on this arm and not the next because it is a statement
        // about what the *viewer* could see change — see
        // [`COVERAGE_DEADBAND_TEXELS`] — and this is the arm reading the picture
        // the viewer is looking at. It governs the whole rule from here: the
        // three arms are ANDed, and a deadbanded `true` implies the undeadbanded
        // one, so nothing below can re-admit what this withheld.
        if !pan_exceeds_coverage_visibly(&displayed, viewport_bounds) {
            return None;
        }

        // **And so has the newest picture**, or the hold already answers this:
        // it was rasterised for a later viewport than the one on screen, and it
        // is what will *be* on screen once its last band lands. When nothing is
        // held these two are the same texture and this is one test asked twice.
        if !pan_exceeds_coverage(&tex.placed.geo, viewport_bounds) {
            return None;
        }

        // **And this pane has not already thrown one away.** [`Self::hold`]
        // replaces rather than queues, so a coverage dispatch made against a
        // pending hold discards a whole upload — and its replacement is
        // discarded in turn by the next one. Past roughly 2.3x the sustainable
        // pan that closes into a loop with no exit, and the loop is the fling:
        // over the 600 counted frames of
        // `coverage_dispatch_tests::a_fling_the_pipeline_cannot_follow_still_puts_pictures_on_screen`,
        // the unbraked rule spent 300 full-size rasters, threw away all 300
        // mid-upload, promoted **none**, and left the pane drawing the picture
        // it had when the fling started.
        //
        // The content arm carries this same brake, on the same two clauses,
        // for the same reason — see its note and the E2 figures there. The
        // arms between (plan size, zoom band, settle) stay unbraked: each
        // fires once per settled change rather than continuously, so none of
        // them can close the dispatch-and-discard loop this guards.
        // **The one arm the oversampling margin exists to prevent.** Every
        // other reason above is a picture that a wider margin would have been
        // asked for anyway; this is the one it buys off. See
        // [`RerenderReason::is_margin_avoidable`].
        (!(self.hold_superseded && self.held.is_some())).then_some(RerenderReason::PanCoverage)
    }
}

/// Whether a zoom that has drifted [`ZOOM_REBUILD_BAND`] from the texture's own
/// may be re-rasterized while the gesture is still moving.
///
/// **One policy value, every target — the quiet the wasm build always kept.**
/// Until WO-8 this was `!cfg!(target_arch = "wasm32")`: native re-rastered on
/// every band crossing mid-gesture, and that arm was most of the measured
/// re-raster storm — native scene A fed 14.5–15.6 whole-picture dispatches/s
/// (~11 GB of pictures per 41 s window) into a 19–32 ms interact frame
/// (WO-6 scoreboard, commit 222b666f). The hold is policy, not physics: the
/// raster itself runs on the worker on every target. What the band arm bought
/// was sharpness *during* the gesture, and what it cost was the frame the
/// gesture is judged by — so the gesture now draws the picture it has,
/// stretched, and the settle arm is what ends the softness. How soon it ends
/// is [`ZoomDrive`]'s answer, not a duration's: the picture is right again
/// [`SETTLE_QUIET_FRAMES`] frames after the gesture stops, wherever that
/// falls on the clock.
///
/// The machinery stays parameterized
/// ([`OverlayTextureCache::needs_rerender_with_policy`]) so the band arm's own
/// behaviour remains pinned; this value is the one production selection of it.
const MID_GESTURE_REBUILDS: bool = false;

/// [`pan_exceeds_coverage`] asked of the picture on screen, deadbanded by
/// [`COVERAGE_DEADBAND_TEXELS`] of that picture's own texels.
fn pan_exceeds_coverage_visibly(texture: &PictureShape, viewport_bounds: &GeoBounds) -> bool {
    let (deadband_lat, deadband_lon) = coverage_deadband(texture, viewport_bounds);
    pan_exceeds_coverage_beyond(
        &texture.placed.geo,
        viewport_bounds,
        deadband_lat,
        deadband_lon,
    )
}

/// The deadband in degrees, per axis, for `texture` judged against `viewport`.
/// See [`COVERAGE_DEADBAND_TEXELS`] and [`COVERAGE_DEADBAND_VIEWPORT_CEILING`].
fn coverage_deadband(texture: &PictureShape, viewport: &GeoBounds) -> (f64, f64) {
    // A picture with no pixels on an axis has no texel to be smaller than, and
    // is not one to withhold a rebuild for.
    let deadband = |ground: f64, texels: u32, viewport_span: f64| {
        if texels == 0 {
            return 0.0;
        }
        (ground.abs() * COVERAGE_DEADBAND_TEXELS / texels as f64)
            .min(viewport_span.abs() * COVERAGE_DEADBAND_VIEWPORT_CEILING)
    };
    let geo = &texture.placed.geo;
    (
        deadband(
            geo.max_lat - geo.min_lat,
            texture.height,
            viewport.max_lat - viewport.min_lat,
        ),
        deadband(
            geo.max_lon - geo.min_lon,
            texture.width,
            viewport.max_lon - viewport.min_lon,
        ),
    )
}

/// Returns `true` if the viewport has panned far enough outside the texture's
/// geo bounds that a re-render is warranted (PAN_REBUILD_THRESHOLD of margin).
fn pan_exceeds_coverage(texture_bounds: &GeoBounds, viewport_bounds: &GeoBounds) -> bool {
    pan_exceeds_coverage_beyond(texture_bounds, viewport_bounds, 0.0, 0.0)
}

/// Whether the viewport has ground in it this picture does not cover — **the
/// pane is drawing nothing there**, which is the only thing about coverage a
/// person can see while a gesture is running.
///
/// [`pan_exceeds_coverage`] with the whole band consumed rather than
/// [`PAN_REBUILD_THRESHOLD`] of it, so it shares that function's handling of an
/// edge already at the projection's limit — the case that spun for ever — and
/// cannot drift from it. It is strictly weaker: anything this answers `true`
/// for, `pan_exceeds_coverage` answers `true` for as well.
fn coverage_is_exhausted_visibly(texture: &PictureShape, viewport_bounds: &GeoBounds) -> bool {
    let (deadband_lat, deadband_lon) = coverage_deadband(texture, viewport_bounds);
    pan_exceeds_coverage_at(
        &texture.placed.geo,
        viewport_bounds,
        1.0,
        deadband_lat,
        deadband_lon,
    )
}

/// [`pan_exceeds_coverage`] with the trigger moved `deadband_lat` / `deadband_lon`
/// degrees later on every edge. A deadband wider than the margin puts the trigger
/// *outside* the texture's own bounds, which is what makes it mean anything at
/// zero overdraw, where the margin is zero and the bounds are the viewport.
fn pan_exceeds_coverage_beyond(
    texture_bounds: &GeoBounds,
    viewport_bounds: &GeoBounds,
    deadband_lat: f64,
    deadband_lon: f64,
) -> bool {
    pan_exceeds_coverage_at(
        texture_bounds,
        viewport_bounds,
        PAN_REBUILD_THRESHOLD,
        deadband_lat,
        deadband_lon,
    )
}

/// [`pan_exceeds_coverage_beyond`] with the fraction of the band a pan must
/// consume as a parameter, so [`coverage_is_exhausted_visibly`] can ask for the whole
/// of it through the same projection-limit handling rather than a second
/// spelling of these four inequalities.
fn pan_exceeds_coverage_at(
    texture_bounds: &GeoBounds,
    viewport_bounds: &GeoBounds,
    threshold: f32,
    deadband_lat: f64,
    deadband_lon: f64,
) -> bool {
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

    // Headroom left when the pan has consumed `threshold` of the band. Crossing
    // into it is what triggers the rebuild, leaving the rest of the band to
    // cover the viewport while the new texture rasterises. At
    // `PAN_REBUILD_THRESHOLD` that is half the band; at 1.0 there is no headroom
    // left at all and the question becomes whether the picture covers the
    // viewport — see [`coverage_is_exhausted_visibly`].
    let headroom = 1.0 - threshold as f64;
    let margin_lat = band_lat * headroom - deadband_lat;
    let margin_lon = band_lon * headroom - deadband_lon;

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

/// The turn this pane is looking at, as a longitude.
///
/// The map's centre lands at the projector's own rect centre by construction,
/// and walkers deliberately leaves that centre **unfolded** — pan a whole turn
/// east and it reads 190, not −170 — so this is the frame every geographic
/// datum has to be carried into before it is projected. See
/// [`squallar_geo::fold_lon_near`].
pub fn pane_turn_lon(projector: &walkers::Projector) -> f64 {
    projector
        .unproject(projector.clip_rect().center().to_vec2())
        .x()
}

/// The screen rect a north-west / south-east geographic corner pair covers.
///
/// **The pair is folded as one, never corner by corner.** `Projector::project`
/// is linear in longitude and folds nothing, so a footprint written in the
/// ±180 frame — a radar image's, a saved download box's — lands a whole world
/// off the glass once the map is panned past the antimeridian. Carrying it into
/// the pane's turn ([`pane_turn_lon`]) is what puts it back. Carrying each
/// corner *separately* would be a different and worse thing: a rect wider than
/// half a turn — an overlay picture at the zoom floor is up to 1.42 of one,
/// viewport plus overdraw — has corners that fold opposite ways, and the rect
/// comes back inside out and at 40 % of its size. One shift, taken from the
/// rect's own middle, cannot do that: it is a translation, so the width it was
/// handed is the width it returns, whatever that width is.
pub fn geo_corner_rect(
    projector: &walkers::Projector,
    nw: (f64, f64),
    se: (f64, f64),
) -> egui::Rect {
    let mid = (nw.1 + se.1) / 2.0;
    let shift = squallar_geo::fold_lon_near(mid, pane_turn_lon(projector)) - mid;
    let project = |(lat, lon): (f64, f64)| {
        projector
            .project(walkers::lat_lon(lat, lon + shift))
            .to_pos2()
    };
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
    squallar_geo::lat_rad_to_mercator_y(lat_rad)
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

/// Whose picture the coverage question is asked of, and what a coverage
/// dispatch may throw away.
#[cfg(test)]
mod coverage_dispatch_tests;

/// When the content arm may refuse: a pane that has thrown away an upload and
/// is still waiting on its replacement, and nothing else.
#[cfg(test)]
mod content_brake_tests;

/// What a sweeping pane clock costs the whole-picture dispatch: how many
/// rasters it spends, how many of those uploads it throws away, and how far
/// behind the clock the picture on the glass runs.
#[cfg(test)]
mod clock_sweep_tests;

/// The deadband on the coverage trigger: a pan too small to move a texel must
/// not re-rasterise the overlay, and must not be able to stall one either.
/// How many rasters may be crossing at once: what the bound admits, what it
/// refuses, and what a result that arrives for a dispatch the cache has moved
/// past is worth.
#[cfg(test)]
mod concurrency_tests;

#[cfg(test)]
mod deadband_tests;

#[cfg(test)]
mod geo_click_tests;

#[cfg(test)]
mod texture_budget_tests;

/// What a reading of the raster ledger licenses: the non-vacuity floor, the
/// arrival balance, and the two routes to the screen.
#[cfg(test)]
mod ledger_tests;

/// What one gesture is allowed to cost: the rasters a pan and a zoom-out spend
/// across a whole gesture, counted rather than timed, and the margin the
/// planner keeps at every rung of the ladder.
#[cfg(test)]
mod gesture_dispatch_tests;

/// The resize arm: what a picture of the wrong size owes, and why that arm
/// needs no brake of its own while a raster is already in flight.
#[cfg(test)]
mod resize_arm_tests;
