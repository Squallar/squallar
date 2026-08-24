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

/// What the whole-picture overlay pipeline has actually spent: rasters asked
/// for, pictures uploaded and bytes with them, and the ones thrown away.
pub mod ledger;

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
        // spend two. One relaxed `fetch_add`; see [`ledger`].
        ledger::note_dispatched();
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
    /// Whether a picture that was still crossing to the GPU has already been
    /// thrown away since the last one reached the screen. Cleared the moment
    /// [`Self::held`] empties by any route; see the coverage arm of
    /// [`Self::needs_rerender_with_policy`], which is the only thing that reads
    /// it and the only dispatch it brakes.
    hold_superseded: bool,
    /// The rasters this pane has asked for and not yet been answered, bounded
    /// by the device's `Budgets::concurrent_renders`. Replaced a `bool` that
    /// admitted exactly one dispatch per pane and layer whatever the device
    /// could afford; see [`RendersInFlight`] and [`RenderSlot`].
    pub renders: RendersInFlight,
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
            hold_superseded: false,
            renders: RendersInFlight::default(),
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
        self.hold_superseded = false;
        self.current = Some(data);
    }

    /// Hold `data` until its pixels have all reached the GPU.
    pub fn hold(&mut self, data: OverlayTextureData, data_time: Option<chrono::NaiveDateTime>) {
        // Replacing a hold discards an upload that had already started, and the
        // coverage arm is not allowed to do that twice running.
        self.hold_superseded |= self.held.is_some();
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
        self.hold_superseded = false;
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
        // outranks the one on screen for every question below about *what the
        // next picture should be*: it is what a dispatch would supersede. The
        // coverage arm at the end asks a different question and takes a
        // different picture; see there.
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
        let displayed = self.current.as_ref().unwrap_or(tex);

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
        if !pan_exceeds_coverage_visibly(displayed, viewport_bounds) {
            return false;
        }

        // **And so has the newest picture**, or the hold already answers this:
        // it was rasterised for a later viewport than the one on screen, and it
        // is what will *be* on screen once its last band lands. When nothing is
        // held these two are the same texture and this is one test asked twice.
        if !pan_exceeds_coverage(&tex.placed.geo, viewport_bounds) {
            return false;
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
        // This is the only arm braked. A hold that is stale in *content* is
        // still superseded, by the arms above, however many have been
        // superseded before it.
        !(self.hold_superseded && self.held.is_some())
    }
}

/// Whether a zoom that has drifted [`ZOOM_REBUILD_BAND`] from the texture's own
/// may be re-rasterized while the gesture is still moving.
///
/// **On wasm this is a policy hold, not a physical necessity.** The reason
/// originally written here — that the raster would run inline on the frame
/// thread — has been false since the overlay-worker slices landed 2026-08-14;
/// see `squallar_worker::offload`'s own module doc, where the only inline
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

/// [`pan_exceeds_coverage`] asked of the picture on screen, deadbanded by
/// [`COVERAGE_DEADBAND_TEXELS`] of that picture's own texels.
fn pan_exceeds_coverage_visibly(texture: &OverlayTextureData, viewport_bounds: &GeoBounds) -> bool {
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
fn coverage_deadband(texture: &OverlayTextureData, viewport: &GeoBounds) -> (f64, f64) {
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
