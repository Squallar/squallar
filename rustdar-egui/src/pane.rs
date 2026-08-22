use crate::overlay_cache::OverlayTextureCache;
use chrono::NaiveDateTime;
use rustdar_device_profile::budget::MAX_PANES_DESKTOP;
use rustdar_radar::hover::HoverSource;
use rustdar_radar::types::{RenderView, ScanInfo};
use rustdar_source::handler::{PaneMut, PaneRef};
use rustdar_source::id::{LayerId, known};
use rustdar_source::product::FieldId;
use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;
use walkers::MapMemory;

use crate::field_facts::elevation_selects_picture;
use rustdar_radar::fields as radar_fields;
#[path = "pane_content.rs"]
mod content;

/// The pane's own layer list, split from the build's catalogue.
mod layer_stack;
pub use layer_stack::{LayerStack, RemovedLayer};

pub use content::{
    BASE_HALF_WIDTH_KM, CrossSectionPane, DEFAULT_VERTICAL_EXAGGERATION, MAX_EYE_DISTANCE,
    MAX_VERTICAL_EXAGGERATION, MIN_EYE_DISTANCE, MIN_VERTICAL_EXAGGERATION, MapPane, MapRender,
    OrbitCamera, OrbitDelta, PaneContent, PaneKind, SectionLine, SectionTarget, SectionUnavailable,
    VolumePane, VolumeRegion, VolumeStamp, VolumeTarget, VolumeViewMode, box_size_km,
    resolution_km,
};

const DEFAULT_PANE_ZOOM: f64 = 4.0;

/// **What a 3D pane resolved to ask for**: which of its layers, and which of
/// that layer's fields.
///
/// The layer id travels with the ask so the dispatcher on the far side of the
/// action channel can resolve the same handler this walk chose, without
/// re-running the walk against a pane that may have moved in between.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VolumeAsk {
    /// The layer that will build the grid.
    pub layer: LayerId,
    /// The field the grid is of — that layer's *current* field, as the layer
    /// itself reports it.
    pub field: FieldId,
}

/// What a pane says when nothing in its stack can build a volume **and there
/// is nothing to switch on either** — the build has no 3D source at all.
///
/// Distinct from [`crate::ui::VOLUME_EMPTY_STATE`], which is the pane saying
/// its own 3D machinery is missing (no volume pane state, no painter). This
/// one is about the *stack*.
pub const NO_VOLUME_LAYER: &str = "No layer in this pane can build a 3D volume.";

pub type PaneId = usize;

/// **Which link group a pane belongs to.** The identity the three per-pane
/// link booleans never had: they said *whether* a pane synced, never *with
/// whom*, so an app with six panes carried eighteen hidden bits and no way to
/// answer "what is linked to what".
///
/// A group is named by a letter wherever it is shown and stored as the index
/// behind that letter. There are never more groups than panes, so
/// [`MAX_PANES_DESKTOP`] bounds the alphabet at `A..=F`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GroupId(u8);

impl GroupId {
    /// The group every fresh pane starts in, and the one every pane of a
    /// config written before groups existed lands in — which is what makes
    /// the migration exact: one group holding everybody is precisely the
    /// model the three booleans described.
    pub const FIRST: Self = Self(0);

    /// Every group a layout can express, in letter order.
    pub fn all() -> impl Iterator<Item = Self> {
        (0..Self::COUNT).map(Self)
    }

    /// How many distinct groups exist — one per pane a layout can hold.
    pub const COUNT: u8 = MAX_PANES_DESKTOP as u8;

    /// The group at `index`, or `None` for an index no layout can reach.
    /// The read side of the wire format: a file naming a group this build
    /// has no letter for is a file from a build with more panes.
    pub fn from_index(index: u8) -> Option<Self> {
        (index < Self::COUNT).then_some(Self(index))
    }

    /// This group's index — what is persisted.
    pub fn index(self) -> u8 {
        self.0
    }

    /// The letter this group is named by, on the pill, on the border and in
    /// the sync section. One spelling, derived, so the three cannot drift.
    pub fn letter(self) -> char {
        char::from(b'A' + self.0)
    }

    /// **This group's accent, the same in both OS themes.** The pane border
    /// paints it, the sync section's group row tints its letter with it, and
    /// the two are the same function so they cannot disagree.
    ///
    /// Deliberately not read off `Visuals`: the accent sits on map tiles,
    /// whose brightness has nothing to do with the app theme, so a
    /// theme-derived colour would be picked against the wrong background half
    /// the time. It is drawn over its own dark backing instead — see
    /// `ui_map::draw_pane_border` — which is what makes one palette legible
    /// on both. The hues are spaced around the wheel and no two are a
    /// red/green pair.
    pub fn accent(self) -> egui::Color32 {
        const ACCENTS: [egui::Color32; GroupId::COUNT as usize] = [
            egui::Color32::from_rgb(56, 189, 248),  // A - sky
            egui::Color32::from_rgb(251, 146, 60),  // B - orange
            egui::Color32::from_rgb(163, 230, 53),  // C - lime
            egui::Color32::from_rgb(192, 132, 252), // D - violet
            egui::Color32::from_rgb(244, 114, 182), // E - pink
            egui::Color32::from_rgb(45, 212, 191),  // F - teal
        ];
        ACCENTS[self.0 as usize]
    }

    /// What reads against [`Self::accent`] — black or white, whichever the
    /// accent's own luminance calls for, so the letter on the border tab is
    /// legible without asking the theme.
    pub fn accent_ink(self) -> egui::Color32 {
        let c = self.accent();
        let luma =
            0.2126 * f32::from(c.r()) + 0.7152 * f32::from(c.g()) + 0.0722 * f32::from(c.b());
        if luma > 140.0 {
            egui::Color32::BLACK
        } else {
            egui::Color32::WHITE
        }
    }
}

#[derive(Clone)]
pub struct RadarImageData {
    pub texture: egui::TextureHandle,
    pub lat: f64,
    pub lon: f64,
    /// The half-width this frame was projected at, km — what the renderer
    /// handed back. The range ring is drawn on it.
    pub max_range_km: f64,
    pub placed: rustdar_geo::PlacedRaster,
    pub hover: Arc<HoverSource>,
    /// Where the cut this frame was drawn from declared its velocity folds,
    /// m/s, or `None` for a frame no single cut is behind.
    pub nyquist_ms: Option<f64>,
    /// Where the melting layer this frame was classified against came from, or
    /// `None` for a frame that classified nothing.
    pub melting_layer_source: Option<rustdar_radar::hca::MeltingLayerSource>,
    /// Where the storm motion vector this frame was shifted by came from, or
    /// `None` for a frame that shifted nothing.
    pub storm_motion: Option<rustdar_radar::srv::SrvMotion>,
}

/// Holds a rendered cross-section raster and the little that has to travel with
/// it for the pane to draw honest axes over it.
#[derive(Clone)]
pub struct SectionImageData {
    pub texture: egui::TextureHandle,
    pub axes: rustdar_radar::xsect::SectionAxes,
    pub tilt_elevations_deg: Vec<f64>,
    /// When each of those rungs was flown, milliseconds since the Unix epoch,
    /// in the same order — `CrossSection::tilt_collected_ms`.
    pub tilt_collected_ms: Vec<i64>,
    /// The fingerprint of the tilt ladder this frame was cut from, from
    /// [`rustdar_radar::sampler::ladder_fingerprint`].
    pub ladder: u64,
}

/// A finished loop frame's picture, in whichever shape its pane draws.
#[derive(Clone)]
pub enum LoopFrameImage {
    PlanView(RadarImageData),
    /// A `SECTION_WIDTH × SECTION_HEIGHT` vertical slice along the loop's line.
    Section(SectionImageData),
    /// A **resident voxel grid**, named rather than held.
    Volume(VolumeFrameGrid),
    /// **One non-radar layer's frame**: a placed raster, whole.
    ///
    /// It carries its own [`PlacedRaster`](rustdar_geo::PlacedRaster), so the
    /// painter puts it back on the ground under any pan or zoom without
    /// re-rasterizing — the same way a radar loop frame is placed and never
    /// rebuilt. A loop's frames are rasterized once and thereafter only
    /// re-placed; zooming past their resolution makes them soft, and that is
    /// the whole of it.
    Overlay(crate::overlay_cache::OverlayTextureData),
}

/// The resident grid one 3D loop frame marches, named by what built it.
#[derive(Clone, Debug, PartialEq)]
pub struct VolumeFrameGrid {
    /// The store id of the resident grid. Unique per build; the GPU side keeps
    /// exactly the ids the store is still holding.
    pub id: u64,
    pub target: VolumeTarget,
}

impl LoopFrameImage {
    /// Which render view produced this picture, so a consumer that holds one
    /// can be checked against the loop it is about to be placed in.
    ///
    /// **`None` for [`Self::Overlay`], and that is the point.** `RenderView` is
    /// radar's own vocabulary — which of radar's three pipelines cut these
    /// pixels — and an overlay frame was cut by none of them. Answering
    /// `PlanView` here would let a finished *radar* render satisfy the
    /// `self.view == RenderView::PlanView` gate on an overlay timeline and be
    /// filed into an overlay frame; pinned by
    /// `an_overlay_frame_is_not_a_radar_render_view`.
    pub fn view(&self) -> Option<RenderView> {
        match self {
            Self::PlanView(_) => Some(RenderView::PlanView),
            Self::Section(_) => Some(RenderView::CrossSection),
            Self::Volume(_) => Some(RenderView::Volume),
            Self::Overlay(_) => None,
        }
    }

    pub fn plan_view(&self) -> Option<&RadarImageData> {
        match self {
            Self::PlanView(image) => Some(image),
            Self::Section(_) | Self::Volume(_) | Self::Overlay(_) => None,
        }
    }

    pub fn section(&self) -> Option<&SectionImageData> {
        match self {
            Self::Section(image) => Some(image),
            Self::PlanView(_) | Self::Volume(_) | Self::Overlay(_) => None,
        }
    }

    pub fn volume(&self) -> Option<&VolumeFrameGrid> {
        match self {
            Self::Volume(grid) => Some(grid),
            Self::PlanView(_) | Self::Section(_) | Self::Overlay(_) => None,
        }
    }

    /// The placed raster one non-radar loop frame holds, or `None` for any of
    /// radar's three shapes.
    pub fn overlay(&self) -> Option<&crate::overlay_cache::OverlayTextureData> {
        match self {
            Self::Overlay(image) => Some(image),
            Self::PlanView(_) | Self::Section(_) | Self::Volume(_) => None,
        }
    }
}

/// A single rendered frame in a radar loop.
pub struct LoopFrame {
    pub timestamp: NaiveDateTime,
    pub image: Option<LoopFrameImage>,
    pub render_in_flight: bool,
    /// True once a render for this frame has been attempted and produced nothing
    /// (no matching sweep for the selected product/elevation, or the render itself
    /// failed). Terminal for this frame's current scan data.
    pub render_failed: bool,
}

/// Which entries of a listing of `total` scans a loop that may hold `held`
/// frames keeps, oldest-first — or `None` when the listing fits and every scan
/// becomes a frame.
pub fn listing_sample_indices(total: usize, held: usize) -> Option<Vec<usize>> {
    if total <= held {
        return None;
    }
    Some(
        (0..held)
            .map(|i| i * (total - 1) / (held - 1).max(1))
            .collect(),
    )
}

/// Tolerance for comparing two selected elevation angles. Shared with the render
/// dispatcher, which uses it when deciding whether two panes' selections are the
/// same and whether a queued render already covers a frame.
pub const ELEVATION_TOLERANCE: f32 = 0.01;

/// **One selected tilt as a render's identity carries it: tenths of a degree.**
///
/// The one spelling of the quantum. `rustdar-app`'s render-key builder calls
/// this rather than rounding again, so a picture's cache slot and the check
/// asking whether that picture is already in hand can never disagree about
/// which two angles are the same angle.
///
/// A bucket, not a tolerance: bucketing is transitive and a tolerance is not,
/// which is why identity uses this and the snapped-sweep *agreement* checks
/// still use [`ELEVATION_TOLERANCE`].
pub fn elevation_tenths(elevation: f32) -> i32 {
    (elevation * 10.0).round() as i32
}

/// Every input `render_radar_to_image` is given *except the scan itself*: the radar
/// site whose coordinates set the projection, and the product/elevation selection
/// that picks the sweep out of that scan.
#[derive(Clone, Debug)]
pub struct RenderTarget {
    /// NEXRAD site code supplying the projection geometry (e.g. "KTLX").
    pub site: String,
    pub product: FieldId,
    /// The pane's *selected* elevation, not the per-scan snapped sweep angle.
    pub elevation: f32,
}

impl RenderTarget {
    pub fn new(site: impl Into<String>, product: &FieldId, elevation: f32) -> Self {
        Self {
            site: site.into(),
            product: product.clone(),
            elevation,
        }
    }

    /// Whether this target names the same picture as `site`/`product`/`elevation`
    /// **when that picture is drawn as `view`**.
    ///
    /// Site and product are exact. The tilt is compared **only when it selects
    /// the picture** — `RenderView::elevation_selects_picture` is the single
    /// arbiter, the same question `retarget_renders_keyed` asks — and then by
    /// [`elevation_tenths`] bucket, which is the quantum the render's own
    /// identity is built on.
    pub fn matches_parts(
        &self,
        site: &str,
        product: &FieldId,
        elevation: f32,
        view: RenderView,
    ) -> bool {
        self.site == site
            && self.product == *product
            && (!elevation_selects_picture(view, product)
                || elevation_tenths(self.elevation) == elevation_tenths(elevation))
    }

    pub fn matches(&self, other: &RenderTarget, view: RenderView) -> bool {
        self.matches_parts(&other.site, &other.product, other.elevation, view)
    }
}

/// What a **section** loop's frames were cut for, beyond what
/// [`RenderTarget`] already says.
#[derive(Clone, Debug, PartialEq)]
pub struct SectionLoopKey {
    pub line: SectionLine,
    /// The storm motion vector the frames were derived with, as raw bits, and
    /// `None` for every product that does not read one.
    pub storm_motion: Option<(u32, u32)>,
    /// Which derived rung the frames fell to when there was no override and no
    /// RPG vector — the reader's `Settings > Storm motion` choice.
    pub srv_fallback: rustdar_radar::srv::SrvFallback,
}

impl SectionLoopKey {
    /// The key for `line` under the storm motion vector `motion` and derived
    /// rung `fallback`, in the same `(speed_kt, direction_from_deg)` form the
    /// extraction is handed.
    pub fn new(
        line: SectionLine,
        motion: Option<(f32, f32)>,
        fallback: rustdar_radar::srv::SrvFallback,
    ) -> Self {
        Self {
            line,
            storm_motion: motion.map(|(speed, direction)| (speed.to_bits(), direction.to_bits())),
            srv_fallback: fallback,
        }
    }
}

/// The 3D counterpart of [`SectionLoopKey`]: what a resident voxel grid depends
/// on that a [`RenderTarget`] cannot say.
#[derive(Clone, Debug, PartialEq)]
pub struct VolumeLoopKey {
    /// The ground every frame is resampled over, or `None` for the default box
    /// about the site.
    pub region: Option<VolumeRegion>,
    /// The storm motion vector the grids were derived with, as raw bits, and
    /// `None` for every product that does not read one.
    pub storm_motion: Option<(u32, u32)>,
    /// Which derived rung the grids fell to; both derived rungs leave
    /// `storm_motion` `None`, so nothing else in this key moves.
    pub srv_fallback: rustdar_radar::srv::SrvFallback,
}

impl VolumeLoopKey {
    /// The key for `region` under the storm motion vector `motion` and derived
    /// rung `fallback`, in the same `(speed_kt, direction_from_deg)` form the
    /// extraction is handed.
    pub fn new(
        region: Option<VolumeRegion>,
        motion: Option<(f32, f32)>,
        fallback: rustdar_radar::srv::SrvFallback,
    ) -> Self {
        Self {
            region,
            storm_motion: motion.map(|(speed, direction)| (speed.to_bits(), direction.to_bits())),
            srv_fallback: fallback,
        }
    }
}

/// The view-specific half of a loop's render key.
#[derive(Clone, Debug, PartialEq)]
pub enum LoopViewKey {
    Section(SectionLoopKey),
    Volume(VolumeLoopKey),
}

/// The two sweep angles a sibling broadcast has to reconcile.
#[derive(Clone, Copy, Debug)]
pub struct BroadcastSweep {
    pub rendered: f32,
    /// The sweep the receiving loop's *own* scan for this frame resolves the same
    /// selection to, or `None` if it has no scan for the frame yet (or that scan
    /// carries no sweep for the product). `None` refuses the image.
    pub own: Option<f32>,
}

impl BroadcastSweep {
    /// Whether the incoming image depicts the sweep this loop would have rendered.
    /// Compared within [`ELEVATION_TOLERANCE`], as every other angle comparison is.
    pub fn agrees(&self) -> bool {
        self.own
            .is_some_and(|own| (own - self.rendered).abs() <= ELEVATION_TOLERANCE)
    }
}

/// The state phases for a radar loop playback instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopPhase {
    Inactive,
    FetchingScanList,
    /// Scans listed and downloads/renders started, waiting to reach render budget.
    Rendering,
    /// Sufficient frames have rendered to allow playback, but playing is not started.
    Ready,
    Playing,
    Paused,
}

/// What a pane's animation is waiting on (WI-7) — the legible form of a blank
/// the loop machinery is about to fill. Both arms carry the quantity a reader
/// can hold, because that is all the notice may say: an animating layer with a
/// frame owed paints NOTHING for it (WI-6), and the caption is what makes that
/// nothing distinguishable from "there is no data here".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopLoading {
    /// A frame listing is in flight; how long it has been out
    /// ([`LayerTimeState::listing_wait`]).
    Listing { waited: std::time::Duration },
    /// The playhead's own frame has no picture yet: which frame (0-based) of
    /// how many held.
    Frame { index: usize, total: usize },
}

/// **One layer's own place on the timeline, in one pane.**
///
/// Everything a layer knows about the frames it can show and which of them it
/// is showing. It lives on the [`LayerSlot`], so two layers in one pane keep
/// two of these and a layer in two panes keeps one per pane — which is what
/// lets a pane animate radar and a model field at their own cadences.
///
/// **Generic on purpose.** No member here names a radar site or a coordinate.
/// The source-specific half of a layer's timeline — for radar, the geometry
/// its frames are projected from — rides in [`Self::anchor`] as a value only
/// the owning layer's own code names ([`rustdar_radar::loop_geometry::LoopGeometry`]),
/// so a second frame-series layer inherits a timeline rather than three
/// NEXRAD fields.
pub struct LayerTimeState {
    pub phase: LoopPhase,
    /// **Which frame this layer is showing, as an index into [`Self::frames`]
    /// — a cache, not a decision.** The decision is the pane's
    /// [`PaneTimePosture::mode`]: the frame shown is the latest whose stamp is
    /// at or before the instant the pane depicts. This field is written by
    /// [`Self::settle_playhead`] and by nothing else, so no caller can park
    /// the picture on a frame the pane's own clock does not name.
    ///
    /// **It is the nearest frame, not necessarily a qualifying one** — when the
    /// clock sits before everything this layer holds it reads 0, and
    /// [`Self::playhead_qualifies`] is what says so.
    current_frame: usize,
    /// **Whether [`Self::current_frame`] is the frame the pane's clock names,
    /// or merely the nearest one to it.** False when the clock sits before
    /// every frame this layer holds — routine on a pane whose layers have
    /// different spans: scrub to 06Z on a pane holding a day of satellite and
    /// 45 minutes of radar, and radar has nothing valid to show.
    ///
    /// **The two readings of the playhead part here, and they are different
    /// questions:**
    ///
    /// - *What is presented* — [`Self::playhead_stamp`] and the pane's picture
    ///   accessors, which go through [`Self::qualifying_frame`] and answer
    ///   `None` when this is false. `TimeAxis::FrameSeries` says nothing is
    ///   drawn when no frame qualifies, and a frame valid *after* the depicted
    ///   instant is a fabrication, not a fallback.
    /// - *What is kept textured* — [`Self::render_set_indices`] and the
    ///   eviction and readiness built on it, which stay centred on
    ///   [`Self::current_frame`] whatever this says. Frame 0 is the correct
    ///   centre when the clock precedes everything: those are the nearest
    ///   frames, and so the ones a scrub forward needs first.
    ///
    /// True on a fresh state so that a directly-parked index is an answer; the
    /// clock's own verdict replaces it at the next [`Self::settle_playhead`].
    playhead_qualifies: bool,
    pub frames: Vec<LoopFrame>,
    /// The window this layer's frames were listed for, in seconds — the extent
    /// the arrival path slides as newer frames land.
    pub span_secs: u64,
    /// **The exact window the in-flight listing was asked over**, recorded at
    /// dispatch and compared whole when a listing lands. [`Self::span_secs`]
    /// alone cannot tell two asks apart: a deep-scrub refill's window is by
    /// construction the same width as a live enable's, anchored somewhere
    /// else entirely, and matching on the width filed one pane's era into
    /// another pane's loop. Every producer echoes the range it was dispatched
    /// with verbatim, so the comparison is exact equality, not tolerance.
    ///
    /// Meaningful while [`Self::phase`] is [`LoopPhase::FetchingScanList`];
    /// after the listing is accepted it is only the record of the last ask,
    /// and the arrival path stops consulting it.
    pub asked_range: Option<(NaiveDateTime, NaiveDateTime)>,
    /// **What the source answered when asked which frames exist.** `None`
    /// until a listing has been accepted through the contract; the frames the
    /// pane actually holds are [`Self::frames`], and this is the answer they
    /// were chosen from. No producer writes it before WO-M12 — radar's
    /// listing still arrives on its own path.
    pub listing: Option<rustdar_source::time::FrameListing>,
    /// Whether the listing this loop was built from had to be **sampled** to fit
    /// the frame cap — `Some(true)` when scans were dropped, `Some(false)` when
    /// every scan in the window became a frame, `None` before a listing has been
    /// accepted at all.
    ///
    /// A **recorded decision, never a derivation**: the frames alone cannot say
    /// whether a wider gap is a dropped scan or a slower sweep.
    pub sampled: Option<bool>,
    /// The source's own frame cadence over this window, in seconds: the median
    /// gap between consecutive frames in the listing the loop was built from,
    /// measured **before** any sampling. `None` until a listing has been accepted.
    /// Measured cadences: TDWR 360 s (VCP 80 and 90), WSR-88D precip (VCP
    /// 212/215) 259 s, clear-air (VCP 35) 517 s. A median, not a mean: a site
    /// that changes VCP mid-window mixes two cadences.
    ///
    /// Recorded with [`Self::sampled`], and for the same reason.
    pub cadence_secs: Option<u32>,
    pub last_advance: Option<web_time::Instant>,
    /// When this loop entered [`LoopPhase::FetchingScanList`], or `None` for a
    /// loop that was never built ([`Self::new`]).
    pub listing_since: Option<web_time::Instant>,
    /// **The source-specific half of this layer's timeline**, opaque here and
    /// named by the layer that owns it. Radar puts a
    /// [`rustdar_radar::loop_geometry::LoopGeometry`] in it; read it back with
    /// [`Self::anchor_as`].
    pub anchor: Option<rustdar_source::handler::FetchPayload>,
    /// The [`RenderTarget`] every frame's render state was produced for, or `None`
    /// before the first dispatch. When the pane's selection moves both the
    /// texture and the `render_failed` flag are stale; see `retarget_renders`.
    pub rendered_for: Option<RenderTarget>,
    /// Which kind of picture this loop's frames are, fixed for the life of the
    /// state by the pane kind that started it.
    pub view: RenderView,
    /// The view-specific half of the render key, or `None` for a plan-view
    /// loop.
    pub view_key: Option<LoopViewKey>,
}

/// Config-file content addressed to a build that is not this one, carried
/// between load and save so the file survives a session under this build.
#[derive(Clone, Debug, Default)]
pub struct PaneConfigBaggage {
    /// `layer_slots` entries that are not slot objects at all, verbatim,
    /// re-appended after the slots on save.
    pub layer_slots: Vec<serde_json::Value>,
    /// The same, for `removed_layers`.
    pub removed_layers: Vec<serde_json::Value>,
    /// Whole pane-level fields this build does not know.
    pub fields: serde_json::Map<String, serde_json::Value>,
}

/// **How far one press of the navigation buttons moves the pane's clock.**
///
/// [`Self::OneFrame`] is not a duration: it means "to the next frame the
/// pane's time-primary layer actually has", which is a different distance at
/// every site and every VCP. It persists as the `0` the config file has
/// always written for it, so no reader of an older or newer file has to
/// change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeStep {
    Secs(i64),
    OneFrame,
}

impl TimeStep {
    /// The step as the config file spells it — `0` for [`Self::OneFrame`].
    pub fn as_secs(self) -> i64 {
        match self {
            Self::Secs(secs) => secs,
            Self::OneFrame => 0,
        }
    }

    /// The step a config file's number names.
    pub fn from_secs(secs: i64) -> Self {
        if secs == 0 {
            Self::OneFrame
        } else {
            Self::Secs(secs)
        }
    }
}

/// **The instant a pane's picture depicts.**
///
/// One clock per pane, and every layer on it is shown at the moment this
/// names — which is what lets a scrub move a radar loop and a warning
/// polygon together instead of each on its own playhead.
///
/// [`Live`](TimeMode::Live) is *the newest there is*, not a timestamp: a
/// live pane follows arrivals instead of parking on the instant they landed,
/// which is why it cannot be spelled as an `AsOf(now)` sampled once.
///
/// Not the same question as [`PaneState::viewing_live`], and the two are
/// deliberately separate — see that field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeMode {
    /// The newest thing each layer has.
    Live,
    /// A named instant, UTC. Each layer shows the latest frame at or before
    /// it; an [`rustdar_source::time::TimeAxis::EventLifetime`] layer shows
    /// what was valid then (WO-E7c).
    AsOf(NaiveDateTime),
}

impl TimeMode {
    /// The instant this names, or `None` while the pane is following live.
    pub fn as_of(self) -> Option<NaiveDateTime> {
        match self {
            Self::Live => None,
            Self::AsOf(t) => Some(t),
        }
    }

    pub fn is_live(self) -> bool {
        matches!(self, Self::Live)
    }
}

/// **The bucket an instant falls in at `quantum` resolution.**
///
/// What a scrubbed pane's texture cache keys an as-of-dependent layer on
/// instead of the raw instant: dragging the scrubber moves the clock every
/// frame, and keying on the instant itself would mint a texture per tick.
/// The quantum is the layer's own
/// [`SourceHandler::as_of_quantum`](rustdar_source::handler::SourceHandler::as_of_quantum)
/// — a minute for NWS lifetimes, a second for lightning's fade ramp.
///
/// A zero quantum would divide by zero and is floored at one second; the
/// contract has no zero, and a bucket is not the place to discover that.
pub fn as_of_bucket(instant: NaiveDateTime, quantum: std::time::Duration) -> i64 {
    let secs = (quantum.as_secs() as i64).max(1);
    instant.and_utc().timestamp().div_euclid(secs)
}

/// **One pane's posture on the timeline**: the instant it depicts, how wide a
/// window it is looking over, how fast it plays, and how far one step moves
/// it.
///
/// A pane fact, not a layer fact — the layers each keep their own
/// [`LayerTimeState`] and are shown at whatever moment this posture names.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaneTimePosture {
    /// **The instant this pane depicts** — the clock every layer's playhead is
    /// derived from, and the one thing a scrub, a step and a playback advance
    /// all write.
    pub mode: TimeMode,
    /// How far back the pane's timeline reaches, in seconds. The setting a
    /// new listing is asked for; the window a listing was actually built with
    /// is the layer's own [`LayerTimeState::span_secs`].
    pub span_secs: u64,
    /// Playback rate, in frames per second.
    pub speed_fps: f32,
    /// How far one navigation step moves the pane.
    pub step: TimeStep,
}

/// The posture a pane is born with: the same numbers the config file's own
/// defaults carry, so a pane that has never been configured and a pane loaded
/// from a default file are the same pane.
impl Default for PaneTimePosture {
    fn default() -> Self {
        Self {
            mode: TimeMode::Live,
            span_secs: 3600,
            speed_fps: 5.0,
            step: TimeStep::Secs(600),
        }
    }
}

/// **One layer, in one pane: its identity, whether it draws, and its saved
/// configuration — the three facts that used to live in three parallel
/// containers.**
///
/// A pane's slots are an ordered list and **the vector's order IS the draw
/// order**, bottom to top. That is the whole reason the three collections
/// merged: `draw_order`, `enabled_overlays` and `overlay_configs` were keyed
/// on the same [`LayerId`] and had to be kept in step by hand, and a
/// transform that dropped an id from one of them left the other two saying
/// something the user could not see.
pub struct LayerSlot {
    /// Which layer this slot is. Open — an id no handler in this build serves
    /// is a layer a newer build has, and it keeps its slot and its position.
    pub id: LayerId,
    /// Whether this pane draws the layer.
    pub enabled: bool,
    /// The layer's saved configuration, as JSON. `null` means "nothing saved"
    /// — a handler with a `null` slot keeps whatever state it already has,
    /// which is exactly what an absent map entry meant before.
    pub config: serde_json::Value,
    /// **The live per-pane state, for a handler that defined one.** Runtime
    /// only: [`Self::config`] is what persists, and this is derived from it
    /// by [`PaneState::hydrate_layer_states`] and written back to it by
    /// [`PaneState::adopt_handler_state`]. A slot that has never been
    /// hydrated — a fresh pane, a clone — carries `None` and is re-derived on
    /// the next hydrate rather than being copied.
    pub state: Option<rustdar_source::handler::FetchPayload>,
    /// **This layer's own place on the timeline, in this pane** — the frames
    /// it holds and which of them it is showing. Runtime only, like
    /// [`Self::state`]: nothing here persists, and a stack copied between
    /// panes does not bring another pane's frames with it.
    pub time: LayerTimeState,
}

/// **Cloning a slot does not clone its state.** The state is a `dyn Any` with
/// no `Clone` to call, and it does not need one: `config` is the same facts in
/// a form that copies, and the clone's `None` is re-derived from it the next
/// time the pane is hydrated. Layer-link sync is the caller this matters to —
/// it copies a whole stack between panes, and the copy comes up with the
/// source's saved configuration rather than a shared handle to its state.
impl Clone for LayerSlot {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            enabled: self.enabled,
            config: self.config.clone(),
            state: None,
            // For the same reason `state` is not cloned, and one more: a
            // timeline is *this pane's* position and *this pane's* textures,
            // so handing a copy to another pane would show it frames it never
            // asked for. `adopt_layers` keeps each destination pane's own.
            time: LayerTimeState::new(),
        }
    }
}

/// The state is a `dyn Any`: it can be reported present or absent and no more.
impl std::fmt::Debug for LayerSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LayerSlot")
            .field("id", &self.id)
            .field("enabled", &self.enabled)
            .field("config", &self.config)
            .field("state", &self.state.is_some())
            .field("frames", &self.time.frames.len())
            .finish()
    }
}

/// **Two slots are equal when they carry the same layer, flag and saved
/// configuration.** The runtime state is deliberately not compared: it is
/// derived from `config`, so comparing it would make a hydrated slot unequal
/// to the identical slot that has not been asked for yet. The timeline is
/// left out for the same reason and one more: it is a position, and two
/// slots for the same layer are the same slot whether or not one of them is
/// mid-playback.
impl PartialEq for LayerSlot {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.enabled == other.enabled && self.config == other.config
    }
}

impl LayerSlot {
    /// A slot for `id` with nothing configured.
    pub fn new(id: LayerId, enabled: bool) -> Self {
        Self {
            id,
            enabled,
            config: serde_json::Value::Null,
            state: None,
            time: LayerTimeState::new(),
        }
    }
}

/// **One pane's layer stack, in the shape [`PaneRef`] borrows from.**
///
/// The sibling-config table is built once here and shared by every
/// [`Self::layer`] call, so asking twelve handlers about one pane costs one
/// allocation rather than twelve.
pub struct PaneView<'a> {
    pane_idx: usize,
    slots: Vec<(&'a LayerId, &'a serde_json::Value)>,
    loading_site: Option<&'a str>,
    pane: &'a PaneState,
}

impl<'a> PaneView<'a> {
    /// What this pane looks like to `id`'s handler. A layer this pane has no
    /// slot for gets a `null` config and no state — the same answer an absent
    /// map entry has always given.
    pub fn layer(&'a self, id: &LayerId) -> PaneRef<'a> {
        let slot = self.pane.slot(id);
        PaneRef {
            pane_idx: self.pane_idx,
            config: slot.map_or(&serde_json::Value::Null, |slot| &slot.config),
            state: slot
                .and_then(|slot| slot.state.as_deref())
                .map(|s| s as &dyn Any),
            slots: &self.slots,
            loading_site: self.loading_site,
            // One pane's view carries no peers: a caller that has to weigh
            // the whole layer across panes builds a `PaneRef::across`.
            peers: &[],
        }
    }

    /// Which pane this is.
    pub fn pane_idx(&self) -> usize {
        self.pane_idx
    }
}

/// **The radar slot's two owners, merged.** `fresh` is the handler's half; the
/// pane's [`RADAR_SLOT_PANE_KEYS`] in `config` are left exactly as they were.
/// A plain overwrite would erase this pane's whole selection.
fn merge_radar_slot(config: &mut serde_json::Value, fresh: &serde_json::Value) {
    let mut merged = match config.take() {
        serde_json::Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    if let serde_json::Value::Object(fresh) = fresh {
        for (key, value) in fresh {
            if RADAR_SLOT_PANE_KEYS.contains(&key.as_str()) {
                continue;
            }
            merged.insert(key.clone(), value.clone());
        }
    }
    *config = serde_json::Value::Object(merged);
}

/// **The radar slot's config is the pane's, not a handler's.** It carries
/// this pane's own selection — site, product, elevation — and the live-chunk
/// switch that used to be one global, none of which any handler produces.
/// [`PaneState::adopt_handler_state`] holds these four back from the radar
/// handler for that reason, and
/// `the_radar_handler_and_the_pane_do_not_claim_the_same_slot_members` is
/// what says the two owners never collide.
pub const RADAR_SLOT_PANE_KEYS: [&str; 4] = ["site", "product", "elevation", "live_chunks"];

/// Per-pane state: each pane independently selects a radar product,
/// elevation, layer toggles, and maintains its own map viewport.
pub struct PaneState {
    /// This pane's radar site. **Private since WO-E6b**: it is persisted as
    /// a member of the radar slot's config, decoded on load and re-emitted on
    /// save, so every read and write goes through [`Self::site`] /
    /// [`Self::set_site`] and there is one place to change when WO-E9 moves
    /// the selection again.
    site: String,
    pub scan_info: Option<ScanInfo>,
    /// When the data behind this pane's current radar image was collected (UTC).
    pub data_time: Option<NaiveDateTime>,
    /// Private since WO-E6b — see [`Self::site`].
    selected_product: FieldId,
    /// Private since WO-E6b — see [`Self::site`].
    selected_elevation: f32,
    /// **Whether this pane's *selection* follows live data** — which sites
    /// keep a chunk feed, whether the archive auto-poll runs, and which way
    /// the Live button is painted.
    ///
    /// **Not** [`PaneTimePosture::mode`], and deliberately not folded into it:
    /// that is the instant the picture *depicts*, and a pane playing a loop
    /// depicts an older instant every frame while still following the live
    /// site. Folding the two would stop the chunk feed the moment a loop
    /// played, which is a behaviour change, not a simplification.
    pub viewing_live: bool,
    /// **Which group this pane's three links are scoped to** — `None` for a
    /// pane that belongs to no group and so syncs with nobody, whatever its
    /// flags say. Persisted; default [`GroupId::FIRST`], which is what makes
    /// a pre-group config load unchanged: one group holding every pane is the
    /// model the flags alone described.
    ///
    /// The flags are kept beside it rather than folded into it because a pure
    /// group model cannot say "in this group, but keeping my own clock" — the
    /// partial membership the border paints as a dashed accent.
    pub group: Option<GroupId>,
    /// Whether this pane follows shared time (plan §3.7). Persisted; default
    /// **true** — every pane before the field existed behaved as linked.
    /// Scoped to [`Self::group`]: on, but in no group, still syncs with
    /// nobody.
    pub time_link: bool,
    /// Whether this pane's viewport belongs to the linked group. Persisted;
    /// default **true**.
    pub viewport_link: bool,
    /// Whether this pane's layer state belongs to the linked group. Persisted;
    /// default **true**.
    pub layer_link: bool,
    pub hover_value: Option<String>,
    /// Hover tooltip text from overlay handlers (e.g. model data CIN value).
    pub overlay_hover_value: Option<String>,
    pub last_hover_pos: Option<egui::Pos2>,
    pub map_memory: MapMemory,
    /// Per-overlay-type texture caches (background-rendered), keyed by
    /// [`LayerId`]. Only texture overlay kinds (SPC, NWS, discussions) have
    /// cache entries; entries are created lazily.
    pub overlay_textures: HashMap<LayerId, OverlayTextureCache>,
    /// **This pane's curated layer stack: one [`LayerSlot`] per layer the pane
    /// draws, bottom to top.** The list's order is the draw order; each slot
    /// carries its own enabled flag and its own saved config. Replaces the
    /// three parallel containers `draw_order` / `enabled_overlays` /
    /// `overlay_configs` (WO-E6b).
    ///
    /// **A [`LayerStack`], not a `Vec<LayerSlot>`, and not the registry.** The
    /// registry is what this *build* can draw; this is what this *pane* draws,
    /// and the two are reconciled by [`LayerStack::admits`] rather than by the
    /// stack being re-derived from the catalogue. See that module's own note
    /// for why a complete projection was the wrong shape.
    pub layers: LayerStack,
    /// Config-file content addressed to a build that is not this one — see
    /// [`PaneConfigBaggage`]. Written by `load_ui_config`, read only by
    /// `ui_config_json`, and never acted on in between.
    pub config_baggage: PaneConfigBaggage,
    /// **Where this pane sits on the clock, and how it moves along it.** The
    /// pane's own posture, shared by every layer it draws — the layers keep
    /// their own [`LayerTimeState`] on their slots.
    pub time: PaneTimePosture,
    /// **Which layer this pane's loop transport addresses** — the timeline the
    /// ∞ toggle, the play/step/seek buttons and the scrubber all move.
    ///
    /// Deliberately a *stored* choice rather than a derived one, and
    /// deliberately separate from [`Self::clock_layer`]: the transport has to
    /// keep answering the same layer while that layer's loop is being torn
    /// down or built up, which is exactly when "the topmost animating layer"
    /// is `None` or is briefly somebody else. It is re-derived only when the
    /// pane's enabled set changes — see [`Self::refresh_transport`].
    ///
    /// Persisted. Default [`known::RADAR`], which is what makes every config
    /// written before the field existed load unchanged: radar was the only
    /// layer the transport could address.
    transport: LayerId,
    /// The answer [`Self::loop_state`] gives a pane that has no radar slot to
    /// keep one on: inactive, empty, and never written. A pane the `Gui` owns
    /// has been through `initialize_pane_enabled` and has the slot, so this is
    /// what a bare [`PaneState`] answers before it is seeded.
    orphan_time: LayerTimeState,
    /// Which site is currently being loaded for this pane (transient loading indicator).
    pub loading_site: Option<String>,
    /// Generation counter for RadarSites texture invalidation.
    /// Bumped when site, loading_site, or theme changes.
    pub radar_sites_render_gen: u64,
    pub content: PaneContent,
}

impl Default for LayerTimeState {
    fn default() -> Self {
        Self::new()
    }
}

impl LayerTimeState {
    /// A layer with no timeline: nothing listed, nothing held, inactive.
    pub fn new() -> Self {
        Self {
            phase: LoopPhase::Inactive,
            current_frame: 0,
            playhead_qualifies: true,
            frames: Vec::new(),
            span_secs: 0,
            asked_range: None,
            listing: None,
            sampled: None,
            cadence_secs: None,
            last_advance: None,
            listing_since: None,
            anchor: None,
            rendered_for: None,
            view: RenderView::PlanView,
            view_key: None,
        }
    }

    /// A layer that has just been asked for a listing covering `span_secs`,
    /// anchored at `anchor` — the source-specific half only that source names.
    pub fn begin(
        span_secs: u64,
        view: RenderView,
        anchor: rustdar_source::handler::FetchPayload,
    ) -> Self {
        Self {
            phase: LoopPhase::FetchingScanList,
            span_secs,
            // Where a loop first enters `FetchingScanList`, and so where the
            // clock on that phase starts. See [`Self::listing_since`]. The
            // shell re-enters the phase when a scrub lands outside the window
            // it listed (`App::refill_unserved_loop_windows`) and restamps
            // this there, for the same reason.
            listing_since: Some(web_time::Instant::now()),
            anchor: Some(anchor),
            view,
            ..Self::new()
        }
    }

    /// This layer's [`Self::anchor`] as `T`, or `None` when it has none (or it
    /// is not a `T`). The one door onto the source-specific half.
    pub fn anchor_as<T: 'static>(&self) -> Option<&T> {
        self.anchor.as_deref()?.downcast_ref::<T>()
    }

    pub fn is_active(&self) -> bool {
        !matches!(self.phase, LoopPhase::Inactive)
    }

    /// Which of [`Self::frames`] this layer is showing. Read-only: the value
    /// is a derivation of the pane's clock, and [`Self::settle_playhead`] is
    /// the one thing that writes it.
    pub fn current_frame(&self) -> usize {
        self.current_frame
    }

    /// **The derivation, and the only writer of the playhead.** The frame
    /// shown at [`TimeMode::AsOf`] `T` is the latest whose stamp is at or
    /// before `T`; under [`TimeMode::Live`] it is the newest frame there is.
    ///
    /// When no frame qualifies — the clock sits before the oldest frame this
    /// layer still holds, which both eviction and a pane of mixed spans
    /// produce — this records *that there is no answer*
    /// ([`Self::playhead_qualifies`]) and parks the index on the nearest
    /// frame. The layer then presents nothing, while the render set still has
    /// a centre to grow from.
    pub fn settle_playhead(&mut self, mode: TimeMode) {
        let qualifying = self.qualifying_frame_at(mode);
        self.playhead_qualifies = qualifying.is_some();
        // NOT a fallback for the presented frame — `playhead_qualifies` is
        // already false here, so nothing is drawn and no stamp is named. This
        // index only has to be somewhere sensible for texture residency to
        // walk outward from, and the nearest frame is exactly that.
        self.current_frame = qualifying.unwrap_or(0);
    }

    /// **The contract's own answer**, without writing it: the latest frame
    /// stamped at or before the depicted instant, `None` when none is. Under
    /// [`TimeMode::Live`] it is the newest frame there is, and `None` only for
    /// a layer holding no frames.
    ///
    /// This is `rustdar_source::time::TimeAxis::FrameSeries`'s rule verbatim,
    /// including *nothing is drawn when no frame qualifies*.
    pub fn qualifying_frame_at(&self, mode: TimeMode) -> Option<usize> {
        if self.frames.is_empty() {
            return None;
        }
        match mode {
            TimeMode::Live => Some(self.frames.len() - 1),
            TimeMode::AsOf(t) => self
                .frames
                .partition_point(|frame| frame.timestamp <= t)
                .checked_sub(1),
        }
    }

    /// **The render set's centre** — [`Self::qualifying_frame_at`] floored to
    /// the nearest frame held, `0` for a layer holding none.
    ///
    /// **Not what the layer presents.** When the clock sits before every frame
    /// this names frame 0 while the layer draws nothing; 0 is the right answer
    /// for *which textures to hold* and the wrong one for *what to show*, and
    /// the callers that want the latter ask [`Self::qualifying_frame`].
    pub fn frame_at(&self, mode: TimeMode) -> usize {
        self.qualifying_frame_at(mode).unwrap_or(0)
    }

    /// **The frame this layer presents**: [`Self::current_frame`] when the
    /// pane's clock actually names it, `None` when no frame qualifies. Every
    /// read that ends up on screen goes through here.
    pub fn qualifying_frame(&self) -> Option<usize> {
        self.playhead_qualifies.then_some(self.current_frame)
    }

    /// The stamp of the frame the playhead is on — `None` for a layer holding
    /// no frames, and `None` when the pane's clock precedes every frame it
    /// holds, because then there is no frame to name.
    pub fn playhead_stamp(&self) -> Option<NaiveDateTime> {
        self.frames
            .get(self.qualifying_frame()?)
            .map(|f| f.timestamp)
    }

    pub fn is_playing(&self) -> bool {
        matches!(self.phase, LoopPhase::Playing)
    }

    pub fn is_render_ready(&self) -> bool {
        matches!(
            self.phase,
            LoopPhase::Ready | LoopPhase::Playing | LoopPhase::Paused
        )
    }

    pub fn is_fetching(&self) -> bool {
        matches!(self.phase, LoopPhase::FetchingScanList)
    }

    /// How long this loop has been waiting for its scan listing, or `None` if
    /// it is not waiting for one.
    pub fn listing_wait(&self, now: web_time::Instant) -> Option<std::time::Duration> {
        if !self.is_fetching() {
            return None;
        }
        Some(
            self.listing_since
                .map_or(std::time::Duration::ZERO, |since| {
                    now.saturating_duration_since(since)
                }),
        )
    }

    pub fn has_playback_started(&self) -> bool {
        matches!(self.phase, LoopPhase::Playing | LoopPhase::Paused)
    }

    pub fn is_rendered_for(&self, target: &RenderTarget) -> bool {
        self.rendered_for
            .as_ref()
            .is_some_and(|t| t.matches(target, self.view))
    }

    /// The index of the frame a finished render for `timestamp`, produced for
    /// `target`, must be written to — or `None` if the result has to be dropped.
    pub fn frame_awaiting_render_result(
        &self,
        timestamp: NaiveDateTime,
        target: &RenderTarget,
    ) -> Option<usize> {
        if self.view != RenderView::PlanView || !self.is_active() || !self.is_rendered_for(target) {
            return None;
        }
        self.frames
            .iter()
            .position(|f| f.timestamp == timestamp && f.render_in_flight)
    }

    pub fn frame_awaiting_render_result_mut(
        &mut self,
        timestamp: NaiveDateTime,
        target: &RenderTarget,
    ) -> Option<&mut LoopFrame> {
        let idx = self.frame_awaiting_render_result(timestamp, target)?;
        Some(&mut self.frames[idx])
    }

    /// The index of the frame that should receive a texture finished by *another*
    /// pane for `timestamp`/`target`, or `None` if this loop cannot use it.
    pub fn frame_accepting_broadcast(
        &self,
        timestamp: NaiveDateTime,
        target: &RenderTarget,
        sweep: BroadcastSweep,
    ) -> Option<usize> {
        if self.view != RenderView::PlanView
            || !self.is_active()
            || !self.is_rendered_for(target)
            || !sweep.agrees()
        {
            return None;
        }
        self.frames
            .iter()
            .position(|f| f.timestamp == timestamp && f.image.is_none())
    }

    pub fn frame_accepting_broadcast_mut(
        &mut self,
        timestamp: NaiveDateTime,
        target: &RenderTarget,
        sweep: BroadcastSweep,
    ) -> Option<&mut LoopFrame> {
        let idx = self.frame_accepting_broadcast(timestamp, target, sweep)?;
        Some(&mut self.frames[idx])
    }

    /// The index of a frame this loop can hand to a pane keyed to `target`, letting
    /// that pane skip a render it would otherwise dispatch.
    pub fn frame_donatable_to(
        &self,
        timestamp: NaiveDateTime,
        target: &RenderTarget,
    ) -> Option<usize> {
        if self.view != RenderView::PlanView || !self.is_active() || !self.is_rendered_for(target) {
            return None;
        }
        self.frames.iter().position(|f| {
            f.timestamp == timestamp && f.image.as_ref().is_some_and(|i| i.plan_view().is_some())
        })
    }

    /// Whether this loop's frames are cut for `target` **and** `key` — the
    /// section counterpart of [`Self::is_rendered_for`].
    pub fn is_cut_for(&self, target: &RenderTarget, key: &SectionLoopKey) -> bool {
        self.is_rendered_for(target) && self.section_key() == Some(key)
    }

    /// The section half of this loop's key, or `None` — including for a loop
    /// whose key is a *volume* key.
    pub fn section_key(&self) -> Option<&SectionLoopKey> {
        match &self.view_key {
            Some(LoopViewKey::Section(key)) => Some(key),
            Some(LoopViewKey::Volume(_)) | None => None,
        }
    }

    pub fn volume_key(&self) -> Option<&VolumeLoopKey> {
        match &self.view_key {
            Some(LoopViewKey::Volume(key)) => Some(key),
            Some(LoopViewKey::Section(_)) | None => None,
        }
    }

    /// The index of the frame awaiting a **section** result for
    /// `timestamp`/`target`/`key`, or `None` if this loop is not owed one.
    pub fn frame_awaiting_section_result(
        &self,
        timestamp: NaiveDateTime,
        target: &RenderTarget,
        key: &SectionLoopKey,
    ) -> Option<usize> {
        if self.view != RenderView::CrossSection
            || !self.is_active()
            || !self.is_cut_for(target, key)
        {
            return None;
        }
        self.frames
            .iter()
            .position(|f| f.timestamp == timestamp && f.render_in_flight)
    }

    pub fn frame_awaiting_section_result_mut(
        &mut self,
        timestamp: NaiveDateTime,
        target: &RenderTarget,
        key: &SectionLoopKey,
    ) -> Option<&mut LoopFrame> {
        let idx = self.frame_awaiting_section_result(timestamp, target, key)?;
        Some(&mut self.frames[idx])
    }

    /// The index of the frame that should receive a **section** raster finished
    /// by another pane, or `None` if this loop cannot use it.
    pub fn frame_accepting_section_broadcast(
        &self,
        timestamp: NaiveDateTime,
        target: &RenderTarget,
        key: &SectionLoopKey,
        ladder: u64,
        own_ladder: Option<u64>,
    ) -> Option<usize> {
        if self.view != RenderView::CrossSection
            || !self.is_active()
            || !self.is_cut_for(target, key)
            || own_ladder != Some(ladder)
        {
            return None;
        }
        self.frames
            .iter()
            .position(|f| f.timestamp == timestamp && f.image.is_none())
    }

    pub fn frame_accepting_section_broadcast_mut(
        &mut self,
        timestamp: NaiveDateTime,
        target: &RenderTarget,
        key: &SectionLoopKey,
        ladder: u64,
        own_ladder: Option<u64>,
    ) -> Option<&mut LoopFrame> {
        let idx =
            self.frame_accepting_section_broadcast(timestamp, target, key, ladder, own_ladder)?;
        Some(&mut self.frames[idx])
    }

    /// The index of a **section** frame this loop can hand to a pane keyed to
    /// `target`/`key`, letting that pane skip a cut it would otherwise dispatch.
    pub fn section_frame_donatable_to(
        &self,
        timestamp: NaiveDateTime,
        target: &RenderTarget,
        key: &SectionLoopKey,
        wanted_ladder: u64,
    ) -> Option<usize> {
        if self.view != RenderView::CrossSection
            || !self.is_active()
            || !self.is_cut_for(target, key)
        {
            return None;
        }
        self.frames.iter().position(|f| {
            f.timestamp == timestamp
                && f.image
                    .as_ref()
                    .and_then(LoopFrameImage::section)
                    .is_some_and(|s| s.ladder == wanted_ladder)
        })
    }

    /// Point the loop's frame renders at `product`/`elevation`, discarding every
    /// frame's render state if that differs from what the frames were last rendered
    /// for. Returns `true` if frames were invalidated.
    pub fn retarget_renders(&mut self, product: &FieldId, elevation: f32) -> bool {
        self.retarget_renders_for(product, elevation, None)
    }

    /// [`Self::retarget_renders`] including the section half of the key.
    pub fn retarget_renders_for(
        &mut self,
        product: &FieldId,
        elevation: f32,
        section: Option<SectionLoopKey>,
    ) -> bool {
        self.retarget_renders_keyed(product, elevation, section.map(LoopViewKey::Section))
    }

    /// [`Self::retarget_renders`] including whichever view half this loop has.
    pub fn retarget_renders_keyed(
        &mut self,
        product: &FieldId,
        elevation: f32,
        key: Option<LoopViewKey>,
    ) -> bool {
        // Runs for every looping pane every frame, and almost always finds no change,
        // so ask before building a target rather than allocating one to throw away.
        // The site the frames are projected from, which this layer keeps in its
        // own anchor rather than in the generic timeline beside it.
        let site = crate::radar_layer::site(self);
        if self
            .rendered_for
            .as_ref()
            .is_some_and(|t| t.matches_parts(site, product, elevation, self.view))
            && self.view_key == key
        {
            return false;
        }

        // Nothing to discard before the first dispatch — frames start blank.
        let had_previous_target = self.rendered_for.is_some();
        let site = site.to_string();
        self.rendered_for = Some(RenderTarget::new(site, product, elevation));
        self.view_key = key;
        if !had_previous_target {
            return false;
        }

        for frame in &mut self.frames {
            frame.image = None;
            frame.render_in_flight = false;
            frame.render_failed = false;
        }
        true
    }

    /// Bring the frame list back inside `held`, by the same even sampling the
    /// listing was capped with. Returns whether anything was dropped.
    pub fn cap_frames(&mut self, held: usize) -> bool {
        let Some(indices) = listing_sample_indices(self.frames.len(), held) else {
            return false;
        };
        // A paused loop stays on the moment the user parked it: the pane's
        // clock does not move because the frame list was thinned, so the next
        // `settle_playhead` puts the playhead on whichever survivor that
        // moment now names. Nothing here touches the index — before WO-E7b
        // this hunt for "the survivor nearest the old index" was how the same
        // intention was spelled, and an index is the wrong thing to preserve
        // across a resample.
        let mut kept: Vec<Option<LoopFrame>> = self.frames.drain(..).map(Some).collect();
        self.frames = indices.into_iter().filter_map(|i| kept[i].take()).collect();
        self.sampled = Some(true);
        true
    }

    /// **The frame that depicts `valid`**, by stamp rather than by index.
    ///
    /// The one door a raster arriving from the wire files itself through: a
    /// render is dispatched for a *stamp* and comes back one or more frames
    /// later, by which time the index it was dispatched at may name another
    /// frame — the list is rebuilt whenever the loop is re-listed or capped,
    /// and `cap_frames` re-samples it outright. Two frames never share a
    /// `timestamp`, so the stamp is the identity that survives.
    pub fn frame_at_stamp_mut(&mut self, valid: NaiveDateTime) -> Option<&mut LoopFrame> {
        self.frames.iter_mut().find(|f| f.timestamp == valid)
    }

    /// Drop textures outside the intended render set once more than `budget` frames
    /// are textured, capping loop memory.
    pub fn evict_textures_outside_render_set(&mut self, budget: usize) {
        let textured = self.frames.iter().filter(|f| f.image.is_some()).count();
        if textured <= budget {
            return;
        }
        let keep = self.render_set_indices(budget);
        for (idx, frame) in self.frames.iter_mut().enumerate() {
            if !keep.contains(&idx) {
                frame.image = None;
            }
        }
    }

    /// Indices of the frames the renderer intends to have textured: up to `budget`
    /// frames, walking outward from the playhead (forward first, then backward).
    pub fn render_set_indices(&self, budget: usize) -> Vec<usize> {
        let num_frames = self.frames.len();
        let budget = num_frames.min(budget);
        let current = self.current_frame;

        let mut indices = Vec::with_capacity(budget);
        for offset in 0..budget {
            let fwd = (current + offset) % num_frames;
            if !indices.contains(&fwd) {
                indices.push(fwd);
            }
            if indices.len() >= budget {
                break;
            }
            let bwd = (current + num_frames - offset) % num_frames;
            if !indices.contains(&bwd) {
                indices.push(bwd);
            }
            if indices.len() >= budget {
                break;
            }
        }
        indices
    }

    /// True when no frame in the intended render set is still waiting on a texture.
    pub fn render_set_settled(
        &self,
        budget: usize,
        scan_available: impl Fn(&LoopFrame) -> bool,
    ) -> bool {
        self.render_set_indices(budget).into_iter().all(|idx| {
            let frame = &self.frames[idx];
            frame.image.is_some()
                || (!frame.render_in_flight && (frame.render_failed || !scan_available(frame)))
        })
    }
}

impl PaneState {
    /// The NEXRAD site this pane is viewing.
    ///
    /// # Why a method beside a public field
    ///
    /// This accessor and the five below it are plain delegation to the
    /// same-named fields and do **nothing else** — no invalidation, no
    /// generation bump, no derived state. They exist so that every production
    /// read and write of the three radar-selection fields already goes through
    /// a function on the day WO-E6b moves those fields behind a layer slot: at
    /// that point the field becomes private and only these six bodies change.
    /// A call site converted here is a call site that migration never touches.
    ///
    /// The fields stay `pub` deliberately — the test suites read them directly
    /// and are migrated in E6b's own named sub-task, and a field and a method
    /// of the same name are two different namespaces in Rust, so both spellings
    /// resolve.
    ///
    /// **The setters must never grow a side effect.** Load flows assign a
    /// pane's site part-way through applying a config, before the scan, the
    /// product list or the viewport have been restored; anything "helpful"
    /// added here (dropping the scan, bumping
    /// [`radar_sites_render_gen`](Self::radar_sites_render_gen), clearing the
    /// loop) would fire in the middle of that sequence and change load
    /// ordering. The invalidations that belong to a site change already live at
    /// the call sites that mean it.
    pub fn site(&self) -> &str {
        &self.site
    }

    /// Set the site this pane is viewing. Plain assignment — see [`Self::site`].
    pub fn set_site(&mut self, site: String) {
        self.site = site;
    }

    /// The radar field this pane has selected.
    ///
    /// **By value, and that costs nothing**: a pane's field is always the
    /// registry's own `&'static` spelling — the constructor starts there, and
    /// [`crate::ui_config::product_or_default`] resolves whatever is on disk
    /// back to it — so the `Cow` inside is always borrowed and the clone is a
    /// pointer copy. Returning it owned is what keeps the hundred-odd callers
    /// free of a borrow of the pane they are usually about to write to.
    pub fn selected_product(&self) -> FieldId {
        self.selected_product.clone()
    }

    /// Set the selected radar product. Plain assignment — see [`Self::site`].
    pub fn set_selected_product(&mut self, product: FieldId) {
        self.selected_product = product;
    }

    /// The elevation angle this pane has selected, in degrees.
    pub fn selected_elevation(&self) -> f32 {
        self.selected_elevation
    }

    /// Set the selected elevation angle. Plain assignment — see [`Self::site`].
    pub fn set_selected_elevation(&mut self, elevation: f32) {
        self.selected_elevation = elevation;
    }

    pub fn new() -> Self {
        Self::with_site("KTLX".to_string())
    }

    pub fn with_site(site: String) -> Self {
        let mut map_memory = MapMemory::default();
        let _ = map_memory.set_zoom(DEFAULT_PANE_ZOOM);
        Self {
            site,
            scan_info: None,
            data_time: None,
            selected_product: radar_fields::known::REFLECTIVITY,
            selected_elevation: 0.0,
            viewing_live: true,
            group: Some(GroupId::FIRST),
            time_link: true,
            viewport_link: true,
            layer_link: true,
            hover_value: None,
            overlay_hover_value: None,
            last_hover_pos: None,
            map_memory,
            // Lazily filled by `overlay_cache_mut`; an absent entry answers
            // every read exactly as a fresh empty cache did.
            overlay_textures: HashMap::new(),
            // **Empty on purpose**, and it is the pane's whole layer state
            // that starts empty rather than only its flags: a pane born here
            // has no saved opinion about any layer, and `Gui` seeds it from
            // the handlers through `initialize_pane_enabled` — which every
            // site that makes a pane calls, because the flags always needed
            // that seeding and now the order comes with them.
            layers: LayerStack::default(),
            config_baggage: PaneConfigBaggage::default(),
            time: PaneTimePosture::default(),
            transport: known::RADAR,
            orphan_time: LayerTimeState::new(),
            loading_site: None,
            radar_sites_render_gen: 0,
            content: PaneContent::Map(Box::default()),
        }
    }

    pub fn kind(&self) -> PaneKind {
        self.content.kind()
    }

    /// What a render dispatched for this pane produces.
    pub fn render_view(&self) -> rustdar_radar::types::RenderView {
        self.content.render_view()
    }

    /// Whether this pane is drawing the plan view every pane used to be.
    pub fn is_map(&self) -> bool {
        self.render_view() == rustdar_radar::types::RenderView::PlanView
    }

    /// Whether this pane paints map geography through a projector *somewhere*
    /// on screen this frame.
    pub fn draws_ground(&self) -> bool {
        match self.render_view() {
            rustdar_radar::types::RenderView::PlanView => true,
            // Its own map, drawn below the frame and copied onto the box's
            // bottom face — geography on a surface, which is all this asks.
            rustdar_radar::types::RenderView::Volume => {
                self.volume().is_some_and(|volume| !volume.hide_floor)
            }
            rustdar_radar::types::RenderView::CrossSection => false,
        }
    }

    /// Whether this pane takes part in the shared viewport — the group
    /// [`Gui::sync_viewports`](crate::Gui) moves together, at **both** ends.
    pub fn shares_viewport(&self) -> bool {
        self.is_map()
    }

    /// Whether these two panes are in the same group. Two panes with no group
    /// are **not** in the same group: `None` means "with nobody", not "with
    /// the other loners".
    pub fn in_group_with(&self, other: &Self) -> bool {
        matches!((self.group, other.group), (Some(a), Some(b)) if a == b)
    }

    /// Whether every link this pane can offer is on — the pill's one-bit
    /// reading, said once so the pill, the border and the section agree.
    /// A pane that shows no map is not asked about the viewport.
    pub fn fully_linked(&self) -> bool {
        (!self.shares_viewport() || self.viewport_link) && self.layer_link && self.time_link
    }

    /// **In a group, and opted out of at least one of its dimensions.** The
    /// state the border marks differently from a full member, because it is
    /// the one a group letter alone would misdescribe.
    pub fn partial_member(&self) -> bool {
        self.group.is_some() && !self.fully_linked()
    }

    /// Whether this pane draws the map layers at all — the Layers panel's gate,
    /// and so the question "would a row here toggle anything?".
    pub fn draws_map_layers(&self) -> bool {
        self.render_view() != rustdar_radar::types::RenderView::CrossSection
    }

    /// This pane's render mode, if it is a map pane at all.
    pub fn map_render(&self) -> Option<MapRender> {
        self.map().map(|map| map.render)
    }

    /// Whether this pane can animate a sequence of past volumes.
    pub fn can_loop(&self) -> bool {
        self.render_view().can_loop() && self.cross_section().is_none_or(|s| s.line.is_some())
    }

    /// This pane's cross-section state, or `None` if it is not a section pane.
    pub fn cross_section(&self) -> Option<&CrossSectionPane> {
        match &self.content {
            PaneContent::CrossSection(section) => Some(section),
            _ => None,
        }
    }

    pub fn cross_section_mut(&mut self) -> Option<&mut CrossSectionPane> {
        match &mut self.content {
            PaneContent::CrossSection(section) => Some(section),
            _ => None,
        }
    }

    /// This pane's map state, or `None` if it is a cross-section pane.
    pub fn map(&self) -> Option<&MapPane> {
        match &self.content {
            PaneContent::Map(map) => Some(map),
            PaneContent::CrossSection(_) => None,
        }
    }

    pub fn map_mut(&mut self) -> Option<&mut MapPane> {
        match &mut self.content {
            PaneContent::Map(map) => Some(map),
            PaneContent::CrossSection(_) => None,
        }
    }

    /// This pane's 3D volume state, or `None` unless it is *currently drawing*
    /// the volume.
    pub fn volume(&self) -> Option<&VolumePane> {
        self.map()
            .filter(|map| map.render == MapRender::Volume)
            .map(|map| &map.volume)
    }

    pub fn volume_mut(&mut self) -> Option<&mut VolumePane> {
        self.map_mut()
            .filter(|map| map.render == MapRender::Volume)
            .map(|map| &mut map.volume)
    }

    /// Draw this map pane's ground in `render` from now on, keeping everything
    /// about *what it is looking at* — and keeping the other mode's state, so
    /// that switching back returns the same picture.
    pub fn set_map_render(&mut self, render: MapRender) -> bool {
        let Some(map) = self.map_mut() else {
            return false;
        };
        if map.render == render {
            return true;
        }
        map.render = render;
        *self.loop_state_mut() = LayerTimeState::new();
        true
    }

    /// Convert this pane to `kind`, keeping everything about *what it is looking
    /// at*: its site, its scan, its product and elevation selection, its
    /// viewport and its layer toggles.
    pub fn set_kind(&mut self, kind: PaneKind) {
        if self.kind() == kind {
            return;
        }
        self.set_content(PaneContent::for_kind(kind));
    }

    /// Make this pane draw `view`, whichever combination of kind and render
    /// mode that takes.
    pub fn set_view(&mut self, view: rustdar_radar::types::RenderView) {
        use rustdar_radar::types::RenderView;
        match view {
            RenderView::PlanView | RenderView::Volume => {
                self.set_kind(PaneKind::Map);
                let render = if view == RenderView::Volume {
                    MapRender::Volume
                } else {
                    MapRender::Plan
                };
                self.set_map_render(render);
            }
            RenderView::CrossSection => self.set_kind(PaneKind::CrossSection),
        }
    }

    /// Replace this pane's per-kind content wholesale, as the config loader does
    /// when it has both the kind and the state in hand.
    pub fn set_content(&mut self, content: PaneContent) {
        let previous = self.render_view();
        self.content = content;
        if self.render_view() != previous || !self.render_view().can_loop() {
            *self.loop_state_mut() = LayerTimeState::new();
        }
    }

    pub fn active_image(&self) -> Option<&RadarImageData> {
        self.active_loop_image().and_then(LoopFrameImage::plan_view)
    }

    /// The playing frame's cross-section raster, or `None` when this pane is not
    /// animating a section.
    pub fn active_section_image(&self) -> Option<&SectionImageData> {
        self.active_loop_image().and_then(LoopFrameImage::section)
    }

    /// The playing frame's resident voxel grid, or `None` when this pane is not
    /// animating a 3D volume — including while its loop is still filling.
    pub fn active_volume_frame(&self) -> Option<&VolumeFrameGrid> {
        self.active_loop_image().and_then(LoopFrameImage::volume)
    }

    /// Whichever picture the loop's playhead is on, before it is narrowed to a
    /// kind. One lookup, so the two accessors above cannot walk to different
    /// frames.
    fn active_loop_image(&self) -> Option<&LoopFrameImage> {
        let ls = self.loop_state();
        ls.frames
            .get(ls.qualifying_frame()?)
            .and_then(|f| f.image.as_ref())
    }

    /// **The raster layer `id` puts on the map this frame** — the draw fork
    /// every non-radar textured layer is painted through (WI-6).
    ///
    /// Two sources, and the layer's own timeline picks:
    ///
    /// * **animating** — the frame under *this layer's* playhead, and nothing
    ///   else. A frame with no picture yet, and a clock sitting before every
    ///   frame the layer holds, both answer `None`: the pane draws nothing
    ///   rather than the last raster that happened to be lying around. That
    ///   fallback is the whole defect this fork exists to close — an hour-old
    ///   forecast left on the glass, unlabelled, reading as the answer.
    /// * **not animating** — the layer's live raster, exactly as before.
    ///
    /// **Addressed at `id`, not at the transport**, which mirrors radar's own
    /// arm ([`Self::active_image`] reads [`Self::loop_state`]). Every layer's
    /// playhead is settled from the pane clock by [`Self::settle_playheads`],
    /// so a second animating layer that is not the transport still has a frame
    /// to name — and gating on the transport would leave exactly that layer
    /// painting the stale live raster this fork removes.
    pub fn overlay_texture_on_screen(
        &self,
        id: &LayerId,
    ) -> Option<&crate::overlay_cache::OverlayTextureData> {
        let ls = self.time_state(id);
        if ls.is_active() {
            return ls
                .frames
                .get(ls.qualifying_frame()?)?
                .image
                .as_ref()
                .and_then(LoopFrameImage::overlay);
        }
        self.overlay_cache(id).and_then(|c| c.current())
    }

    /// When the data behind the image *currently on screen* was collected —
    /// or, for a forecast frame, when it is *valid*: the transport playhead's
    /// own stamp, which for a frame past the wall clock is the valid time.
    ///
    /// **Transport-addressed** (WI-9). It read `loop_state()` — radar's slot
    /// by definition — so a pane looping a forecast reported no time of its
    /// own and the chip fell through to another pane's clock.
    pub fn data_time_on_screen(&self) -> Option<NaiveDateTime> {
        if self.transport_state().is_active() {
            // `playhead_stamp`, not the index: a clock before every frame the
            // loop holds names no frame, and captioning the oldest one would
            // date the picture hours off what is actually on screen.
            return self.transport_state().playhead_stamp();
        }
        self.data_time
    }

    /// **Whether this pane depicts a forecast instant** — its playhead stamp
    /// sits after the wall clock (WI-10). The playhead is the pane's own
    /// clock when it is parked, else the stamp its timeline presents (a
    /// forecast transport under a live clock presents its newest frame).
    ///
    /// **Not the negation of [`Self::viewing_live`]**, which is about the
    /// *selection* following live data — a pane parked on a forecast frame
    /// still follows its live site (the chunk feed and auto-poll keep
    /// running). This is about the *instant depicted*, which is the question
    /// the scrubber's resting position and the forward-step enable actually
    /// ask.
    pub fn depicts_future(&self, now: NaiveDateTime) -> bool {
        let depicted = match self.time.mode {
            TimeMode::AsOf(t) => Some(t),
            TimeMode::Live => self.data_time_on_screen(),
        };
        depicted.is_some_and(|t| t > now)
    }

    /// What the radar image on screen depicts, **when that is not what this pane
    /// has selected** — the product and sweep the pixels really are, so a caller
    /// can say so.
    pub fn stale_image_on_screen(&self) -> Option<(FieldId, f32)> {
        if self.loop_state().is_active() {
            return None;
        }
        let meta = self
            .overlay_cache(&known::RADAR)?
            .current()?
            .radar_meta
            .as_ref()?;
        let matches_selection = match self.get_rendering_params() {
            Some((product, elevation)) => {
                meta.product == product && (meta.elevation - elevation).abs() <= ELEVATION_TOLERANCE
            }
            // No params means this pane's scan does not offer the selected
            // product at all, so no render will be dispatched and the old image
            // will stand indefinitely. There is no snapped angle to compare
            // against, so the product alone decides.
            None => meta.product == self.selected_product(),
        };
        (!matches_selection).then_some((meta.product.clone(), meta.elevation))
    }

    /// Where the picture **on the glass** folds, m/s — the Nyquist velocity
    /// the cut behind those pixels declared, or `None` when nothing on screen
    /// can carry that annotation. The ramp is fixed at ±36.01 m/s while a
    /// WSR-88D's Doppler cuts declare 23.84–62.94 m/s (ten volumes measured in
    /// `rustdar_radar::nyquist`), so past ±Vny the sign wraps. A TDWR always
    /// answers `None`: it declares `nyquist_velocity = 0` on every cut.
    pub fn displayed_nyquist_ms(&self) -> Option<f64> {
        if self.selected_product() != radar_fields::known::VELOCITY || !self.is_map() {
            return None;
        }
        if self.loop_state().is_active() {
            // No product gate here, and none is needed: `retarget_renders`
            // drops every frame texture the moment the selection moves, so a
            // looping pane cannot hold a frame depicting anything else.
            return self.active_image().and_then(|frame| frame.nyquist_ms);
        }
        if self.stale_image_on_screen().is_some() {
            return None;
        }
        self.overlay_cache(&known::RADAR)?
            .current()?
            .radar_meta
            .as_ref()?
            .nyquist_ms
    }

    /// Where the melting layer behind the classification **on screen** came
    /// from, or `None` when the picture is not a classification.
    pub fn displayed_melting_layer_source(&self) -> Option<rustdar_radar::hca::MeltingLayerSource> {
        if self.selected_product() != radar_fields::known::HYDROMETEOR_CLASSIFICATION
            || !self.is_map()
        {
            return None;
        }
        if self.loop_state().is_active() {
            return self
                .active_image()
                .and_then(|frame| frame.melting_layer_source);
        }
        if self.stale_image_on_screen().is_some() {
            return None;
        }
        self.overlay_cache(&known::RADAR)?
            .current()?
            .radar_meta
            .as_ref()?
            .melting_layer_source
    }

    /// The storm motion vector behind the storm-relative field **on screen**,
    /// or `None` when the picture is not storm-relative.
    pub fn displayed_storm_motion(&self) -> Option<rustdar_radar::srv::SrvMotion> {
        if self.selected_product() != radar_fields::known::STORM_RELATIVE_VELOCITY || !self.is_map()
        {
            return None;
        }
        if self.loop_state().is_active() {
            return self.active_image().and_then(|frame| frame.storm_motion);
        }
        if self.stale_image_on_screen().is_some() {
            return None;
        }
        self.overlay_cache(&known::RADAR)?
            .current()?
            .radar_meta
            .as_ref()?
            .storm_motion
    }

    /// **One layer's `PaneRef`, for a caller that asks about exactly one.**
    ///
    /// The sibling table is **empty**: materialising it needs a `Vec` that
    /// outlives the call, which is what [`Self::view`] exists for. A caller
    /// whose handler reads a sibling slot (the site picker reading the radar
    /// slot's `"site"`) must go through `view()`; every other caller wants
    /// this, because it composes into one expression and borrows nothing that
    /// has to be kept alive.
    pub fn layer_ref(&self, pane_idx: usize, id: &LayerId) -> PaneRef<'_> {
        let slot = self.slot(id);
        PaneRef {
            pane_idx,
            config: slot.map_or(&serde_json::Value::Null, |slot| &slot.config),
            state: slot
                .and_then(|slot| slot.state.as_deref())
                .map(|s| s as &dyn Any),
            slots: &[],
            loading_site: self.loading_site.as_deref(),
            peers: &[],
        }
    }

    /// **This pane, as handlers see it.** Build once per pane per frame and
    /// ask it for each layer in turn: the sibling table is materialised here
    /// rather than per layer, which is the whole reason the type exists.
    pub fn view(&self, pane_idx: usize) -> PaneView<'_> {
        PaneView {
            pane_idx,
            slots: self
                .layers
                .iter()
                .map(|slot| (&slot.id, &slot.config))
                .collect(),
            loading_site: self.loading_site.as_deref(),
            pane: self,
        }
    }

    /// This pane's slot for `id`, or `None` for a layer it has no slot for.
    pub fn slot(&self, id: &LayerId) -> Option<&LayerSlot> {
        self.layers.iter().find(|slot| slot.id == *id)
    }

    /// This pane's slot for `id`, mutably.
    /// **One layer's timeline in this pane.** A layer with no slot has no
    /// timeline either and answers with the empty one — inactive, no frames.
    pub fn time_state(&self, id: &LayerId) -> &LayerTimeState {
        self.slot(id).map_or(&self.orphan_time, |slot| &slot.time)
    }

    /// [`Self::time_state`], to write. A layer with no slot gains one, at the
    /// top of the stack — the same answer [`Self::set_overlay_enabled`] gives
    /// a layer it is asked about and this pane has never heard of.
    pub fn time_state_mut(&mut self, id: &LayerId) -> &mut LayerTimeState {
        if self.slot(id).is_none() {
            self.layers.push(LayerSlot::new(id.clone(), false));
        }
        &mut self
            .layers
            .iter_mut()
            .find(|slot| slot.id == *id)
            .expect("the slot was just inserted")
            .time
    }

    /// **The layer whose stamps this pane's clock walks** — its *time-primary*
    /// layer, the topmost animating one in the draw order (the slot list runs
    /// bottom to top, so the last match wins).
    ///
    /// Only a [`rustdar_source::time::TimeAxis::FrameSeries`] layer can hold
    /// frames, so this is that set narrowed to the ones actually running. The
    /// other half of the question — which layers *declare* `FrameSeries` on a
    /// pane that is animating nothing, which is what decides whether a
    /// one-frame step is offered at all — needs the registry and is asked of
    /// [`crate::Gui`].
    pub fn clock_layer(&self) -> Option<&LayerId> {
        self.layers
            .iter()
            .rev()
            .find(|slot| slot.time.is_active())
            .map(|slot| &slot.id)
    }

    /// Every layer this pane is animating, bottom to top — the set a frame
    /// budget divides across (WO-E7d).
    pub fn animating_layers(&self) -> impl Iterator<Item = &LayerSlot> {
        self.layers.iter().filter(|slot| slot.time.is_active())
    }

    /// [`Self::animating_layers`], to write — the set the readiness pass walks
    /// so that settling a loop is asked of every layer that is running one,
    /// not of the radar slot by name (WI-2).
    ///
    /// It yields only slots that already exist, which is the one behavioural
    /// difference from the radar-addressed walk it replaced:
    /// [`Self::time_state_mut`] *creates* a slot for a layer the pane has
    /// never heard of, so reading the radar timeline every frame materialised
    /// an empty radar slot on every pane. An absent slot is inactive and had
    /// nothing to settle either way.
    pub fn animating_layers_mut(&mut self) -> impl Iterator<Item = &mut LayerSlot> {
        self.layers.iter_mut().filter(|slot| slot.time.is_active())
    }

    /// **What this pane's animation is waiting on**, or `None` when nothing —
    /// the question the map's loading notice asks (WI-7).
    ///
    /// A listing in flight outranks a frame owed a picture: a refill after a
    /// deep scrub restamps [`LayerTimeState::listing_since`] while old frames
    /// may still be held, and "the listing is out" is the truer statement of
    /// the two.
    ///
    /// The frame arm claims *loading* only while something is actually coming:
    /// the frame's render is in flight, or the layer's batch has not settled
    /// ([`LayerTimeState::is_render_ready`] false, so renders are still being
    /// dispatched). A frame that failed, or sits empty in a settled batch
    /// because its scan holds no data, is WI-6's honest nothing — not loading.
    pub fn loop_loading(&self, now: web_time::Instant) -> Option<LoopLoading> {
        for slot in self.animating_layers() {
            if let Some(waited) = slot.time.listing_wait(now) {
                return Some(LoopLoading::Listing { waited });
            }
        }
        for slot in self.animating_layers() {
            let ls = &slot.time;
            let Some(index) = ls.qualifying_frame() else {
                continue;
            };
            let Some(frame) = ls.frames.get(index) else {
                continue;
            };
            let coming = frame.render_in_flight || !ls.is_render_ready();
            if frame.image.is_none() && !frame.render_failed && coming {
                return Some(LoopLoading::Frame {
                    index,
                    total: ls.frames.len(),
                });
            }
        }
        None
    }

    /// **Whether this pane's clock is running.** Asked of the pane, answered
    /// by the time-primary layer's [`LoopPhase`], which stays the one
    /// authority: a `playing` stored beside it would be a second truth to
    /// keep in step, and the phase is what the transport, the dispatcher and
    /// the wake term already read.
    pub fn playing(&self) -> bool {
        self.clock_layer()
            .is_some_and(|id| self.time_state(id).is_playing())
    }

    /// **Re-derive every layer's playhead from this pane's clock.** The one
    /// door: call it after the clock moves or a layer's frame list changes,
    /// and no other code writes a playhead.
    pub fn settle_playheads(&mut self) {
        let mode = self.time.mode;
        for slot in &mut self.layers {
            slot.time.settle_playhead(mode);
        }
    }

    /// Move this pane's clock, and settle every layer onto it.
    pub fn set_time_mode(&mut self, mode: TimeMode) {
        self.time.mode = mode;
        self.settle_playheads();
    }

    /// **Park this pane's clock on one layer's frame**, named by index — what
    /// the scrubber and the frame-step buttons do. The clock takes that
    /// frame's own stamp, so every other layer on the pane moves to the same
    /// instant rather than each keeping a private index. `false` for an index
    /// that names no frame, and then nothing moved.
    pub fn park_on_frame(&mut self, id: &LayerId, index: usize) -> bool {
        let Some(stamp) = self
            .time_state(id)
            .frames
            .get(index)
            .map(|frame| frame.timestamp)
        else {
            return false;
        };
        self.set_time_mode(TimeMode::AsOf(stamp));
        true
    }

    /// [`Self::park_on_frame`] on the radar layer — the loop the transport
    /// means by "this pane's loop".
    pub fn park_on_loop_frame(&mut self, index: usize) -> bool {
        self.park_on_frame(&known::RADAR, index)
    }

    /// **The radar layer's timeline in this pane** — what the loop transport,
    /// the dispatcher and the arrival path all mean by "this pane's loop".
    pub fn loop_state(&self) -> &LayerTimeState {
        self.time_state(&known::RADAR)
    }

    /// [`Self::loop_state`], to write.
    pub fn loop_state_mut(&mut self) -> &mut LayerTimeState {
        self.time_state_mut(&known::RADAR)
    }

    /// **The layer this pane's loop transport addresses** — see
    /// [`Self::transport`](#structfield.transport). Radar until something
    /// moves it, which is what makes every caller below identical to the
    /// radar-addressed [`Self::loop_state`] on a radar pane.
    pub fn transport_layer(&self) -> &LayerId {
        &self.transport
    }

    /// Address the transport at `id`. The config loader's door — restoring a
    /// saved choice is the one write that is not a re-derivation, because the
    /// file already holds the answer [`Self::refresh_transport`] would
    /// recompute.
    pub fn set_transport_layer(&mut self, id: LayerId) {
        self.transport = id;
    }

    /// **The transport layer's timeline in this pane** — what the ∞ toggle,
    /// the transport buttons and the scrubber all read.
    ///
    /// Not the same accessor as [`Self::loop_state`], and deliberately so:
    /// that one is radar's own timeline, which the arrival path, the render
    /// dispatcher and the scan cache go on addressing by name because their
    /// payloads are radar's.
    /// Every one of the three goes through [`Self::transport_layer`] rather
    /// than reading the field, so there is exactly one place that decides
    /// which layer this pane's transport is about.
    pub fn transport_state(&self) -> &LayerTimeState {
        self.time_state(self.transport_layer())
    }

    /// [`Self::transport_state`], to write.
    pub fn transport_state_mut(&mut self) -> &mut LayerTimeState {
        let id = self.transport_layer().clone();
        self.time_state_mut(&id)
    }

    /// [`Self::park_on_frame`] on the transport layer — what the scrubber and
    /// the frame-step buttons do.
    pub fn park_on_transport_frame(&mut self, index: usize) -> bool {
        let id = self.transport_layer().clone();
        self.park_on_frame(&id, index)
    }

    /// **The layer the transport *should* address**: this pane's topmost
    /// enabled layer that comes in stamped frames, or `None` when it has
    /// none.
    ///
    /// The slot list runs bottom to top, so the last match is the topmost —
    /// the same reading [`Self::clock_layer`] makes, narrowed by what the
    /// handlers *declare* rather than by what happens to be running. Needs
    /// the registry, so it is asked with one rather than stored.
    pub fn topmost_frame_series_layer<'a>(
        &'a self,
        overlays: &rustdar_overlays::render::overlay_state::OverlayRegistry,
    ) -> Option<&'a LayerId> {
        self.layers
            .iter()
            .rev()
            .filter(|slot| slot.enabled)
            .map(|slot| &slot.id)
            .find(|id| {
                overlays.handlers().any(|handler| {
                    handler.id() == **id
                        && matches!(
                            handler.time_axis(),
                            rustdar_source::time::TimeAxis::FrameSeries { .. }
                        )
                })
            })
    }

    /// **Re-derive which layer the transport addresses.** Called wherever the
    /// pane's enabled set moves, and by the ∞ toggle before it starts a loop.
    ///
    /// Two things it refuses to do, both for the same reason — the transport
    /// must never be left addressing a timeline nobody can drive:
    ///
    /// - A pane with no frame-series layer at all keeps the layer it had,
    ///   rather than losing its transport to an empty answer.
    /// - A **running** loop keeps its transport. Ticking some other layer in
    ///   the stack is not a request to abandon the loop already playing, and
    ///   handing the controls to an idle layer mid-loop would leave the ∞
    ///   button reading "off" over frames still arriving.
    pub fn refresh_transport(
        &mut self,
        overlays: &rustdar_overlays::render::overlay_state::OverlayRegistry,
    ) {
        if self.transport_state().is_active() {
            return;
        }
        if let Some(id) = self.topmost_frame_series_layer(overlays).cloned() {
            self.transport = id;
        }
    }

    pub fn slot_mut(&mut self, id: &LayerId) -> Option<&mut LayerSlot> {
        self.layers.iter_mut().find(|slot| slot.id == *id)
    }

    /// The draw order, bottom to top — the slot list's own order.
    pub fn draw_order(&self) -> impl DoubleEndedIterator<Item = &LayerId> + ExactSizeIterator + '_ {
        self.layers.iter().map(|slot| &slot.id)
    }

    /// The draw order as an owned list, for the callers that reorder it or
    /// hold it across a borrow of the pane.
    pub fn draw_order_vec(&self) -> Vec<LayerId> {
        self.draw_order().cloned().collect()
    }

    /// Reorder the slot list to `order`, carrying each slot's enabled flag and
    /// config with it. Ids in `order` this pane has no slot for join as fresh
    /// disabled slots; slots `order` omits keep their relative order at the
    /// end, so a partial list can never silently drop a layer.
    pub fn set_draw_order(&mut self, order: &[LayerId]) {
        let mut remaining = self.layers.take_slots();
        let mut reordered: Vec<LayerSlot> = Vec::with_capacity(remaining.len());
        for id in order {
            match remaining.iter().position(|slot| slot.id == *id) {
                Some(pos) => reordered.push(remaining.remove(pos)),
                None => reordered.push(LayerSlot::new(id.clone(), false)),
            }
        }
        reordered.append(&mut remaining);
        self.layers.set_slots(reordered);
    }

    /// **Offer this pane every registered layer, and let the stack decide
    /// which it takes.**
    ///
    /// `wanted` is `(id, draw_order_weight, default_enabled)` for every handler
    /// the registry serves; a layer joins only if [`LayerStack::admits`] says
    /// so, at the position `weight_of` puts it in among the slots that already
    /// have weights.
    ///
    /// **The name is now a promise this call does not keep, and that is the
    /// point.** It used to fill in every hole, which is what made a pane's
    /// stack a complete projection of the build's catalogue; a layer that ships
    /// off now waits in the catalogue, and one the user removed never comes
    /// back. The reconcile a saved order still gets — a layer this build serves
    /// that the file never named landing at its weight position rather than on
    /// top — is unchanged for the layers that do join.
    pub fn insert_missing_slots(
        &mut self,
        wanted: &[(LayerId, u32, bool)],
        weight_of: &dyn Fn(&LayerId) -> Option<u32>,
    ) {
        for (id, weight, default_on) in wanted {
            if !self.layers.admits(id, *default_on) {
                continue;
            }
            self.insert_slot_at_weight(LayerSlot::new(id.clone(), *default_on), *weight, weight_of);
        }
    }

    /// Put `slot` in at the position `weight` earns it: above every slot whose
    /// own weight is lower, below the first that is higher, and on top of the
    /// stack when nothing above it has a weight at all (a stack of ids from a
    /// newer build).
    fn insert_slot_at_weight(
        &mut self,
        slot: LayerSlot,
        weight: u32,
        weight_of: &dyn Fn(&LayerId) -> Option<u32>,
    ) {
        let pos = self
            .layers
            .iter()
            .position(|held| weight_of(&held.id).is_some_and(|w| w > weight));
        match pos {
            Some(pos) => self.layers.insert(pos, slot),
            None => self.layers.push(slot),
        }
    }

    /// **Whether `id` may be curated out of this pane, and why not when it may
    /// not** — the sentence the stack row's disabled trash can shows on hover.
    ///
    /// **One id is named, and it is named because the pane already names it.**
    /// The radar slot is not just another layer here: its `config` is where
    /// this pane keeps its own site, product, elevation and live-chunk switch
    /// ([`RADAR_SLOT_PANE_KEYS`], which exist precisely because that slot has
    /// two owners), its [`LayerTimeState`] is what [`Self::loop_state`]
    /// returns, and [`Self::overlay_texture_releasable`] exempts its texture by
    /// name. Removing it would not hide a picture, it would delete the pane's
    /// whole selection. So a radar pane keeps its radar layer, and the control
    /// says so rather than being absent or silently doing nothing.
    ///
    /// Everything else is removable, the colour scale included: it is a legend
    /// drawn over the map with no pane state hanging off it, the eye already
    /// hides it, and a user who wants the screen back should be able to have
    /// it.
    pub fn layer_removal_refusal(&self, id: &LayerId) -> Option<&'static str> {
        (*id == known::RADAR).then_some(
            "The radar layer holds this pane's site, product and tilt - hide \
             it with the eye instead",
        )
    }

    /// **Curate `id` out of this pane's stack.**
    ///
    /// Not a disable: the slot leaves the list, so every question that reads
    /// the stack — [`Self::is_overlay_enabled`], the draw loop's per-layer
    /// gate, `Gui::any_pane_has_overlay_enabled` and the poll term hanging off
    /// it — answers "no" structurally rather than by a flag someone has to
    /// remember to write. What the layer held goes with it: the slot carries
    /// its own [`LayerTimeState`] (frames, playhead, `rendered_for` render
    /// marks) and dropping the slot drops all of it, and the texture cache is
    /// released through the very call the disable path ends with
    /// ([`Self::release_disabled_overlay_textures`], whose predicate already
    /// reads "no slot" as releasable), then dropped outright so an unused cache
    /// entry does not outlive the layer.
    ///
    /// Returns `false`, changing nothing, for a layer this pane does not hold
    /// or may not remove.
    pub fn remove_layer(&mut self, id: &LayerId) -> bool {
        if self.layer_removal_refusal(id).is_some() {
            return false;
        }
        if self.layers.take_out(id).is_none() {
            return false;
        }
        self.release_disabled_overlay_textures();
        self.overlay_textures.remove(id);
        true
    }

    /// **Curate `id` into this pane's stack**, at its own draw-order weight,
    /// with whatever configuration it held when it last left.
    ///
    /// The other half of [`Self::remove_layer`], and the catalogue's real
    /// "add". A layer already in the stack is left exactly where it is —
    /// adding twice is not a reorder — and answers `false`.
    pub fn add_layer(
        &mut self,
        registry: &rustdar_overlays::render::overlay_state::OverlayRegistry,
        id: &LayerId,
    ) -> bool {
        if self.layers.holds(id) {
            return false;
        }
        let Some(handler) = registry.handler_by_id(id) else {
            // An id no handler serves has no weight to sort by and nothing to
            // draw; the catalogue cannot offer one, and neither can this.
            return false;
        };
        let weight = handler.draw_order_weight();
        let weights: HashMap<LayerId, u32> = registry
            .handlers()
            .map(|h| (h.id(), h.draw_order_weight()))
            .collect();
        let mut slot = LayerSlot::new(id.clone(), true);
        // The settings it left with, not a fresh default: a removal the user
        // undoes should cost them nothing. `Null` for a layer that saved
        // nothing, which is the same "ask the handler" an absent slot has
        // always meant.
        slot.config = self.layers.saved_config_of_removed(id);
        self.insert_slot_at_weight(slot, weight, &|id| weights.get(id).copied());
        true
    }

    pub fn is_overlay_enabled(&self, id: &LayerId) -> bool {
        self.slot(id).is_some_and(|slot| slot.enabled)
    }

    /// **Which of this pane's layers a 3D view asks for a grid, or why none of
    /// them can give one.**
    ///
    /// The stack is walked **top down** — [`Self::layers`] runs bottom to top,
    /// so this reads it in reverse — and the first slot that is *enabled*,
    /// whose handler answers `SourceHandler::volume` with a 3D half, and whose
    /// own `SourceHandler::current_field` that half says it
    /// `VolumeCapable::builds`, is the answer. Topmost wins for the same reason it wins everywhere else
    /// in a stack: it is the layer drawn over the others, so it is the one the
    /// pane is *about*.
    ///
    /// Every question here is asked **of a handler**, never answered by
    /// matching on an id: this is the seam that stops a 3D pane knowing which
    /// of its layers is radar.
    ///
    /// `Err` carries the sentence the pane paints instead of a picture — never
    /// a blank pane and never a hidden one. It names the layer and the field
    /// wherever it can, because "3D is unavailable" is not a thing a reader
    /// can act on and "Velocity Dealiased on Radar has no vertical structure"
    /// is.
    ///
    /// **Hydrate first.** A handler answers `current_field` out of its own
    /// slot, and for the layer whose selection the *pane* owns, the slot is
    /// only current after [`Self::hydrate_layer_states`] has published it.
    /// Every other caller that asks a handler about a pane already runs that
    /// hydrate; a caller of this that does not would read the selection this
    /// pane had when its file was opened.
    pub fn volume_ask(
        &self,
        registry: &rustdar_overlays::render::overlay_state::OverlayRegistry,
        pane_idx: usize,
    ) -> Result<VolumeAsk, String> {
        let view = self.view(pane_idx);
        // The best refusal this walk found on its way down: an enabled 3D
        // layer whose field has no third dimension is a more useful thing to
        // say than "no layer qualified", and the topmost such layer is the
        // one the reader is looking at.
        let mut wrong_field: Option<(String, String)> = None;
        // A layer whose own current field is not one of its own registered
        // rows. Structurally unreachable through a pane — the constructor
        // starts on a registered id, the load path resolves an unknown one to
        // the default, and the catalogue only ever writes registered ids — but
        // it is a state the type system permits, and a pane that painted
        // nothing rather than saying so would be the silent half of it.
        let mut unregistered: Option<(String, String)> = None;
        let mut switched_off: Vec<&str> = Vec::new();
        for slot in self.layers.iter().rev() {
            let Some(handler) = registry.handler_by_id(&slot.id) else {
                continue;
            };
            let Some(volume) = handler.volume() else {
                continue;
            };
            if !slot.enabled {
                switched_off.push(handler.display_name());
                continue;
            }
            let pane = view.layer(&slot.id);
            let Some(field) = handler.current_field(&pane) else {
                continue;
            };
            let Some(spec) = handler.products().iter().find(|spec| spec.id == field) else {
                if unregistered.is_none() {
                    unregistered =
                        Some((handler.display_name().to_owned(), field.as_str().to_owned()));
                }
                continue;
            };
            if volume.builds(spec) {
                return Ok(VolumeAsk {
                    layer: slot.id.clone(),
                    field,
                });
            }
            if wrong_field.is_none() {
                wrong_field = Some((handler.display_name().to_owned(), spec.name.to_owned()));
            }
        }
        if let Some((layer, field)) = wrong_field {
            return Err(format!(
                "{field} has no vertical structure to render in 3D - pick a field {layer} \
                 measures or derives tilt by tilt",
            ));
        }
        if let Some((layer, field)) = unregistered {
            return Err(format!(
                "{layer} has no field called {field} in this build - pick one it does.",
            ));
        }
        if !switched_off.is_empty() {
            return Err(format!(
                "Turn on {} in this pane to build a 3D volume.",
                switched_off.join(" or "),
            ));
        }
        Err(NO_VOLUME_LAYER.to_owned())
    }

    /// **Which volume this pane's 3D view is about**, given what the site's
    /// merge currently holds.
    ///
    /// A pane on the live feed is about the *newest* data in the merge: that
    /// stamp advances with every sealed sweep, which is what makes the 3D view
    /// rebuild in step with the map beside it. A pane that has navigated back
    /// to an older scan is about **that** scan, and the site's new arrivals are
    /// not its business — which is why an arrival cannot drag a navigated pane
    /// forward in time.
    ///
    /// `None` while the site has no merged volume at all; the pane says the
    /// first download is in flight rather than drawing an empty box.
    ///
    /// The second half of the pair is when the complete base volume under the
    /// merge began, which only the caption reads. It rides along rather than
    /// getting its own accessor because it is the *same* two-branch choice:
    /// splitting it would be two spellings of one decision, free to disagree.
    pub fn volume_stamp(
        &self,
        current: Option<crate::radar_layer::CurrentVolumeStamp>,
    ) -> Option<(VolumeStamp, Option<NaiveDateTime>)> {
        let navigated = (!self.viewing_live)
            .then(|| self.scan_info.as_ref().map(|info| info.timestamp))
            .flatten();
        current.map(|current| match navigated {
            Some(collected) => (
                VolumeStamp {
                    site: self.site.clone(),
                    collected,
                },
                Some(collected),
            ),
            None => (
                VolumeStamp {
                    site: self.site.clone(),
                    collected: current.newest,
                },
                current.base_started,
            ),
        })
    }

    /// **The grid a 3D pane showing `field` at `stamp` is about.**
    ///
    /// The three things that identify a volume in the store, put together in
    /// one place: the draw-time level-trigger and the arrival-path eager
    /// dispatch (WO-M14c) both name a volume for the same pane, and if they
    /// could spell it differently they would build one grid twice under two
    /// keys instead of the second asker attaching to the first.
    pub fn volume_target_for(&self, field: &FieldId, stamp: VolumeStamp) -> VolumeTarget {
        VolumeTarget {
            volume: stamp,
            product: field.clone(),
            region: self.volume().and_then(|volume| volume.region),
        }
    }

    /// **Whether this pane still needs `target` built** — the level-trigger's
    /// own condition, asked by the arrival path too.
    ///
    /// Three refusals, each about a pane that is not owed this build:
    ///
    /// * a pane **not in Volume mode** is owed no volume at all. Building one
    ///   for it would be speculation about a mode it might switch to, which is
    ///   the boundary WO-M14c is drawn at: the arrival path moves the *same*
    ///   work earlier and never invents new work;
    /// * a pane **playing a 3D loop** is showing its playhead's own frame
    ///   grid, so the live volume it would otherwise ask for is a grid nothing
    ///   would put on screen;
    /// * a pane **already rendered for this exact target** has nothing to ask
    ///   for. This is the off-switch that makes the level-trigger quiesce once
    ///   an eager build has landed, and it is why [`Self::volume_target_for`]
    ///   must produce the same key on both paths.
    pub fn volume_build_due(&self, target: &VolumeTarget) -> bool {
        self.active_volume_frame().is_none()
            && self
                .volume()
                .is_some_and(|volume| volume.rendered_for.as_ref() != Some(target))
    }

    /// Turn `id` on or off in this pane's stack.
    ///
    /// **Turning a layer this pane does not hold ON adds it**, at the top —
    /// switching a layer on is an explicit ask for it, and a slotless layer has
    /// no place in the draw order to be toggled into. (The catalogue takes the
    /// better route and calls [`Self::add_layer`], which lands the slot at its
    /// own draw-order weight.)
    ///
    /// **Turning a layer this pane does not hold OFF does nothing**, and that
    /// asymmetry is load-bearing since the stack became curated: a pane that
    /// has removed a layer already draws it in no sense at all, and minting a
    /// disabled slot to record the fact would hand the layer straight back as a
    /// row. `apply_preset` is the caller this matters to — it walks the whole
    /// registry writing `on` for the layers its preset names and `off` for
    /// every other one, and the old unconditional push would have resurrected
    /// every removed layer in the pane the preset was applied to.
    pub fn set_overlay_enabled(&mut self, id: LayerId, enabled: bool) {
        match self.slot_mut(&id) {
            Some(slot) => slot.enabled = enabled,
            None if enabled => self.layers.push(LayerSlot::new(id, true)),
            None => {}
        }
    }

    /// This pane's enabled flags as a map, for the callers that compare whole
    /// sets rather than walk the stack.
    pub fn enabled_map(&self) -> HashMap<LayerId, bool> {
        self.layers
            .iter()
            .map(|slot| (slot.id.clone(), slot.enabled))
            .collect()
    }

    /// Take a whole layer stack — order, flags and configs together. The one
    /// operation layer-link sync needs, and the reason it cannot lose a half:
    /// there are no longer three halves to keep in step.
    /// **Timelines do not travel.** The adopted stack arrives with fresh,
    /// empty [`LayerTimeState`]s (see [`LayerSlot::clone`]); this pane's own
    /// are carried across by id, so a layer-link sync moves flags, order and
    /// configs and leaves every pane where it was on the clock. Before the
    /// timeline lived on the slot it was a separate field this call could not
    /// reach, and that behaviour is what is preserved here.
    pub fn adopt_layers(&mut self, layers: &LayerStack) {
        let mut mine: Vec<(LayerId, LayerTimeState)> = self
            .layers
            .take_slots()
            .into_iter()
            .map(|slot| (slot.id, slot.time))
            .collect();
        // **The whole stack, tombstones included.** A linked group shares one
        // layer arrangement, and a curation is part of the arrangement: a copy
        // that brought the slots but not the removals would let the
        // destination pane's next reconcile hand back every layer the user
        // just removed from the group.
        self.layers.adopt(layers);
        for slot in self.layers.iter_mut() {
            if let Some(pos) = mine.iter().position(|(id, _)| *id == slot.id) {
                slot.time = mine.remove(pos).1;
            }
        }
    }

    /// **Give every slot its live state**, for the handlers that keep one.
    ///
    /// Derived from `slot.config` when the pane has something saved, and from
    /// the handler's own default when it has not — the same two answers the
    /// registry swap produced, except that they land in the pane instead of in
    /// a handler every other pane shares. Idempotent: a slot that already has
    /// state is left alone, so this is safe to call on every frame that is
    /// about to ask a handler about this pane.
    ///
    /// The flag follows the state, not the other way round: a config that says
    /// the layer is on has always won over the slot's own flag, because the
    /// swap deserialized it and `adopt_handler_state` copied the result back.
    pub fn hydrate_layer_states(
        &mut self,
        registry: &rustdar_overlays::render::overlay_state::OverlayRegistry,
        pane_idx: usize,
    ) {
        self.publish_radar_selection();
        for slot in &mut self.layers {
            if slot.state.is_some() {
                continue;
            }
            // The slot's own flag is the fallback: it is what the file said
            // about THIS pane, and a config that names `enabled` still wins.
            slot.state = registry.create_pane_state(&slot.id, &slot.config, slot.enabled);
            let Some(state) = slot.state.as_deref() else {
                // No state means the handler has not moved its fields yet, so
                // its answer still comes off the registry — and the registry
                // may be holding some *other* pane's configs at this moment.
                // Asking it here would write that pane's flag into this one.
                continue;
            };
            let view = PaneRef {
                pane_idx,
                config: &slot.config,
                state: Some(state as &dyn Any),
                slots: &[],
                loading_site: None,
                peers: &[],
            };
            slot.enabled = registry.is_enabled(&slot.id, &view);
        }
    }

    /// **Publish this pane's radar selection into the slot a handler reads
    /// it from.**
    ///
    /// [`RADAR_SLOT_PANE_KEYS`] declares that the radar slot's config carries
    /// this pane's site, product and tilt, and `merge_radar_slot` holds those
    /// three back from the handler so the two owners never collide. Nothing
    /// kept them *current*: the members were written by the config load and
    /// re-derived at the save, and [`Self::set_site`] and its two siblings are
    /// plain assignments by their own written contract. So between a load and
    /// the next reload, every handler reading them saw the selection the pane
    /// had when the file was opened — and a pane that was never loaded from a
    /// file (a fresh pane, a split, a test fixture) had no `"site"` member at
    /// all.
    ///
    /// The one live consumer today is `RadarSitesHandler::prepare_job`, whose
    /// `is_current` flag is the "you are here" marker on the radar-sites
    /// layer; it followed the load-time site. WO-M12 needs the same three
    /// members for the frame-supply contract — `list_frames` and
    /// `create_frame_list_task` are handed a [`PaneRef`] and nothing else —
    /// which is what brought the staleness to light.
    ///
    /// Called from [`Self::hydrate_layer_states`], which every caller already
    /// runs before asking a handler anything about this pane. Writes only when
    /// a member actually differs, so a pane whose selection has not moved
    /// allocates nothing.
    ///
    /// `live_chunks` is deliberately NOT projected: unlike the other three it
    /// has no field on the pane to be projected *from* — the slot's config is
    /// its only home, read by [`Self::radar_live_chunks`] and written by
    /// [`Self::set_radar_live_chunks`].
    fn publish_radar_selection(&mut self) {
        let elevation = if self.selected_elevation.is_finite() {
            self.selected_elevation
        } else {
            0.0
        };
        let product = match serde_json::to_value(&self.selected_product) {
            Ok(product) => product,
            Err(e) => {
                log::error!("this pane's product cannot be published to its slot ({e})");
                return;
            }
        };
        let site = self.site.clone();
        let Some(slot) = self.layers.iter_mut().find(|slot| slot.id == known::RADAR) else {
            return;
        };
        let held = slot.config.as_object();
        // **Compared in the pane's own precision, not the file's.** The tilt is
        // an `f32` on the pane and an f64 in JSON, and widening `0.9f32` gives
        // `0.8999999761581421` — so a value-wise comparison would call every
        // slot stale on the first frame and rewrite a member the file's own
        // round trip had preserved exactly. The save path widens it too, so
        // nothing here changes what is eventually written; it only refuses to
        // churn a member whose meaning has not moved.
        let site_fresh = held.and_then(|m| m.get("site")).and_then(|v| v.as_str()) == Some(&site);
        let product_fresh = held.and_then(|m| m.get("product")) == Some(&product);
        let elevation_fresh = held
            .and_then(|m| m.get("elevation"))
            .and_then(serde_json::Value::as_f64)
            .is_some_and(|held| held as f32 == elevation);
        if site_fresh && product_fresh && elevation_fresh {
            return;
        }
        let mut map = match slot.config.take() {
            serde_json::Value::Object(map) => map,
            _ => serde_json::Map::new(),
        };
        if !site_fresh {
            map.insert("site".to_string(), serde_json::Value::String(site));
        }
        if !product_fresh {
            map.insert("product".to_string(), product);
        }
        if !elevation_fresh {
            map.insert(
                "elevation".to_string(),
                serde_json::Value::from(f64::from(elevation)),
            );
        }
        slot.config = serde_json::Value::Object(map);
    }

    /// Turn `id` on or off **in this pane**, through its own state.
    ///
    /// The slot's flag is what draws and the state's is what persists, so both
    /// move together or the next reopen restores the one that did not.
    pub fn set_layer_enabled(
        &mut self,
        registry: &mut rustdar_overlays::render::overlay_state::OverlayRegistry,
        pane_idx: usize,
        id: &LayerId,
        enabled: bool,
    ) {
        if !self.layers.holds(id) {
            // **Switching a layer this pane does not hold ON adds it**, at its
            // own draw-order weight and with whatever it held when it last
            // left — [`Self::add_layer`] is the one door for that, so the
            // catalogue's tile, the inspector's Show toggle and this all land
            // the slot in the same place. Switching one OFF does nothing:
            // there is nothing to hide, and minting a disabled slot to say so
            // would put a removed layer back in the list as a row.
            if enabled {
                self.add_layer(registry, id);
            } else {
                return;
            }
        }
        let Some(slot) = self.layers.iter_mut().find(|slot| slot.id == *id) else {
            // Unreachable through `add_layer` above, which only declines an id
            // no handler serves — and this pane holds no slot for one of those
            // either.
            self.set_overlay_enabled(id.clone(), enabled);
            return;
        };
        let Some(state) = slot.state.as_deref_mut() else {
            slot.enabled = enabled;
            return;
        };
        let mut write = PaneMut {
            pane_idx,
            state: Some(state as &mut dyn Any),
            peers: &[],
        };
        registry.set_enabled(id, enabled, &mut write);
        // Asked, not assumed: for a layer whose "enabled" is a set, the set
        // is the fact and `enabled` was only the request.
        let on = registry.is_enabled(id, &write.as_ref());
        slot.enabled = on;
    }

    /// Overwrite the **handler-owned** part of this pane's slots with the
    /// registry's fresh serialize, KEEPING every slot whose id no handler
    /// serves: an unknown id's saved state (a newer build's layer) must ride
    /// through verbatim, or the next autosave makes the loss permanent.
    ///
    /// The radar slot's config has two owners: the handler's members are
    /// adopted like any other slot's, and [`RADAR_SLOT_PANE_KEYS`] — this
    /// pane's site, product, tilt and live-chunk switch — are left alone. A
    /// plain overwrite would erase the pane's whole selection on the next
    /// layer toggle.
    pub fn adopt_handler_state(
        &mut self,
        registry: &rustdar_overlays::render::overlay_state::OverlayRegistry,
    ) {
        for slot in &mut self.layers {
            if registry.handler_by_id(&slot.id).is_none() {
                continue;
            }
            // A slot that owns its state is not told what it holds: it is
            // asked, and what it answers is what persists. **There is no
            // other branch** — the config swap is what used to answer for a
            // slot with no state of its own, and it answered with some other
            // pane's. A slot that has not been hydrated yet is left exactly
            // as it is rather than being overwritten from a global.
            let Some(state) = slot.state.as_deref() else {
                continue;
            };
            let view = PaneRef {
                pane_idx: 0,
                config: &serde_json::Value::Null,
                state: Some(state as &dyn Any),
                slots: &[],
                loading_site: None,
                peers: &[],
            };
            slot.enabled = registry.is_enabled(&slot.id, &view);
            let fresh = registry.serialize_pane_state(&slot.id, state);
            if slot.id != known::RADAR {
                slot.config = fresh;
            } else {
                // The radar slot has two owners. The handler's members land
                // beside the pane's, never over them.
                merge_radar_slot(&mut slot.config, &fresh);
            }
        }
        // A registered handler this pane has no slot for: it is *offered* the
        // stack, which takes it only if [`LayerStack::admits`] agrees — a
        // default-on layer this pane has never removed. A layer that ships off
        // waits in the catalogue and one the user removed stays removed, so the
        // save that follows this call can no longer quietly re-complete a stack
        // the user curated.
        let missing: Vec<(LayerId, u32, bool)> = registry
            .handlers()
            .filter(|h| self.layers.admits(&h.id(), h.default_enabled()))
            .map(|h| (h.id(), h.draw_order_weight(), h.default_enabled()))
            .collect();
        if !missing.is_empty() {
            let weights: HashMap<LayerId, u32> = registry
                .handlers()
                .map(|h| (h.id(), h.draw_order_weight()))
                .collect();
            // No config is written for the joining slot: it has saved
            // nothing, and `create_pane_state(slot.enabled)` at the next
            // hydrate is what gives it its state. Seeding it from the
            // registry's serialize is what handed one pane another's.
            self.insert_missing_slots(&missing, &|id| weights.get(id).copied());
        }
    }

    /// The live-chunk switch this pane's radar slot carries, or `None` for a
    /// slot that has never been given one (a fresh pane, or a file written
    /// before the switch fanned out).
    pub fn radar_live_chunks(&self) -> Option<bool> {
        self.slot(&known::RADAR)?
            .config
            .get("live_chunks")?
            .as_bool()
    }

    /// Write the live-chunk switch into this pane's radar slot.
    pub fn set_radar_live_chunks(&mut self, enabled: bool) {
        let Some(slot) = self.slot_mut(&known::RADAR) else {
            return;
        };
        let mut map = match slot.config.take() {
            serde_json::Value::Object(map) => map,
            _ => serde_json::Map::new(),
        };
        map.insert("live_chunks".to_string(), serde_json::Value::Bool(enabled));
        slot.config = serde_json::Value::Object(map);
    }

    pub fn overlay_cache(&self, id: &LayerId) -> Option<&OverlayTextureCache> {
        self.overlay_textures.get(id)
    }

    pub fn overlay_cache_mut(&mut self, id: &LayerId) -> &mut OverlayTextureCache {
        self.overlay_textures.entry(id.clone()).or_default()
    }

    /// Whether `kind`'s texture may be let go, judged against the slot list
    /// that decides it — the single definition of that question.
    pub fn overlay_texture_releasable(layers: &[LayerSlot], id: &LayerId) -> bool {
        // The same fallback [`Self::is_overlay_enabled`] applies: an id with no
        // slot is not drawn, so its texture is releasable.
        *id != known::RADAR && !layers.iter().any(|slot| slot.id == *id && slot.enabled)
    }

    pub fn overlay_texture_is_releasable(&self, id: &LayerId) -> bool {
        Self::overlay_texture_releasable(&self.layers, id)
    }

    /// Let go of the GPU texture of every overlay this pane no longer draws.
    pub fn release_disabled_overlay_textures(&mut self) {
        // Two fields of one struct, borrowed disjointly, which is the whole
        // reason the predicate above is an associated function over the list.
        let layers = &self.layers;
        for (id, cache) in &mut self.overlay_textures {
            if Self::overlay_texture_releasable(layers, id) {
                cache.clear();
            }
        }
    }

    pub fn is_holding_raster(&self) -> bool {
        self.overlay_textures
            .values()
            .any(OverlayTextureCache::is_holding)
    }

    pub fn held_raster_id(&self) -> Option<egui::TextureId> {
        Some(self.overlay_cache(&known::RADAR)?.held_texture()?.id())
    }

    /// Let go of every raster that is still arriving, without showing any.
    pub fn release_held_raster(&mut self) {
        for cache in self.overlay_textures.values_mut() {
            cache.release_hold();
        }
    }

    /// Show the raster this pane is holding, if every texel of it has landed.
    pub fn promote_held_raster(&mut self, delivered: impl Fn(egui::TextureId) -> bool) -> bool {
        let cache = self.overlay_cache_mut(&known::RADAR);
        let Some(held) = cache.take_held_if_delivered(delivered) else {
            return false;
        };
        cache.show(held.data);
        self.data_time = held.data_time;
        true
    }

    /// Show every non-radar overlay picture this pane is holding whose pixels
    /// have all landed.
    pub fn promote_held_overlays(&mut self, delivered: impl Fn(egui::TextureId) -> bool) {
        for (id, cache) in &mut self.overlay_textures {
            if *id == known::RADAR {
                continue;
            }
            if let Some(held) = cache.take_held_if_delivered(&delivered) {
                cache.show(held.data);
            }
        }
    }

    /// Put a freshly placed raster on this pane — now, or when it is whole.
    pub fn place_radar_raster(
        &mut self,
        data: crate::overlay_cache::OverlayTextureData,
        data_time: Option<chrono::NaiveDateTime>,
        already_whole: bool,
    ) {
        let cache = self.overlay_cache_mut(&known::RADAR);
        if already_whole || cache.current().is_none() {
            cache.show(data);
            self.data_time = data_time;
        } else {
            cache.hold(data, data_time);
        }
    }

    pub fn get_rendering_params(&self) -> Option<(FieldId, f32)> {
        // The scan's tilt table is radar's own, keyed by radar's own field
        // type, so the id is resolved through the one door rather than the
        // table being re-keyed: a scan is the radar layer's fact about itself.
        let product = radar_fields::product_for(&self.selected_product)?;
        let elevations = self.scan_info.as_ref()?.product_elevations.get(&product)?;
        let snapped = elevations
            .iter()
            .min_by(|a, b| {
                ((**a - self.selected_elevation).abs())
                    .total_cmp(&((**b - self.selected_elevation).abs()))
            })
            .copied()
            .unwrap_or(self.selected_elevation);
        Some((self.selected_product.clone(), snapped))
    }
}

impl Default for PaneState {
    fn default() -> Self {
        Self::new()
    }
}

/// **How a multi-pane window splits when the count leaves a choice.**
///
/// The default is [`Auto`](Self::Auto), which asks the width class: two panes
/// side by side is right on a desktop and two useless slivers on a phone. The
/// other two are the user's override and they hold at every width — the
/// default must not be a rule.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitOrientation {
    /// Let the width class choose. See [`PaneLayout::for_count`].
    #[default]
    Auto,
    /// One pane per row, stacked, whatever the width.
    Rows,
    /// One row, every pane a column, whatever the width.
    Columns,
}

pub struct PaneLayout {
    /// Number of active panes (1-6 desktop, 1-4 mobile).
    pub pane_count: usize,
    /// Grid configuration. Each element is the number of columns in that row.
    /// e.g., [2, 2] = 2×2 grid, [2, 1] = 2 top + 1 bottom.
    grid: Vec<usize>,
    /// Height ratio for each row (each >= MIN_RATIO, all sum to 1.0).
    row_ratios: Vec<f32>,
    /// Width ratios for columns in each row (each row's ratios sum to 1.0).
    col_ratios: Vec<Vec<f32>>,
    /// The width class this grid was chosen at, so [`PaneLayout::reflow`] can
    /// tell a real width change from a frame that merely re-asked.
    width: crate::ui_layout::WidthClass,
    /// The preference this grid was chosen under, likewise.
    orientation: SplitOrientation,
}

const MIN_RATIO: f32 = 0.15;
/// How far a persisted run of ratios may sit from summing to 1.0 and still be
/// believed. Wide enough for an `f32` round-tripping through JSON's decimal
/// text, narrow enough that a run which does not add up is refused.
const RATIO_SUM_TOLERANCE: f32 = 1e-3;
const DIVIDER_HALF_WIDTH: f32 = 4.0;

/// Whether `ratios` is a believable run of `wanted` divider positions: the
/// right arity, every entry finite and at least [`MIN_RATIO`], and the whole
/// run summing to 1.0.
fn ratios_valid(ratios: &[f32], wanted: usize) -> bool {
    ratios.len() == wanted
        && ratios.iter().all(|r| r.is_finite() && *r >= MIN_RATIO)
        && (ratios.iter().sum::<f32>() - 1.0).abs() <= RATIO_SUM_TOLERANCE
}

/// Height/width ratio at which the color scale bars *take up* the horizontal
/// (bottom-edge) orientation, having been vertical.
const COLOR_SCALE_HORIZONTAL_ENTER: f32 = 1.35;
/// Height/width ratio at which they *give it up* again.
const COLOR_SCALE_HORIZONTAL_EXIT: f32 = 1.05;
/// Ratio used for the very first decision, when there is no previous
/// orientation to keep. Sits in the middle of the band.
const COLOR_SCALE_SEED_RATIO: f32 = 1.2;

/// The color scale bars' orientation for the whole map panel, remembered across
/// frames so it has hysteresis instead of a bare threshold.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ColorScaleOrientation {
    /// `None` until the first usable panel rect has been seen.
    horizontal: Option<bool>,
}

impl ColorScaleOrientation {
    pub fn resolve(&mut self, panel_rect: egui::Rect) -> bool {
        let (w, h) = (panel_rect.width(), panel_rect.height());
        // A degenerate or not-yet-laid-out panel must not seed the memory with
        // a decision that then sticks through the hysteresis band.
        if !(w.is_finite() && h.is_finite()) || w <= 0.0 || h <= 0.0 {
            return self.horizontal.unwrap_or(false);
        }

        let ratio = h / w;
        let horizontal = match self.horizontal {
            None => ratio > COLOR_SCALE_SEED_RATIO,
            // Already horizontal: keep it until the panel is clearly not portrait.
            Some(true) => ratio > COLOR_SCALE_HORIZONTAL_EXIT,
            // Already vertical: take it up only when the panel is clearly portrait.
            Some(false) => ratio > COLOR_SCALE_HORIZONTAL_ENTER,
        };
        self.horizontal = Some(horizontal);
        horizontal
    }
}

impl Default for PaneLayout {
    fn default() -> Self {
        Self::for_count(
            1,
            crate::ui_layout::WidthClass::Expanded,
            SplitOrientation::Auto,
        )
    }
}

impl PaneLayout {
    /// Create a layout for the given pane count, clamped to
    /// `1..=`[`MAX_PANES_DESKTOP`], for the room there is and the split the
    /// user asked for.
    ///
    /// **The width is a runtime argument, never a `cfg`.** One binary resizes
    /// from a phone-shaped window to a desktop one and back within a session,
    /// and the grid has to follow; [`PaneLayout::reflow`] is what re-asks.
    pub(crate) fn for_count(
        count: usize,
        width: crate::ui_layout::WidthClass,
        orientation: SplitOrientation,
    ) -> Self {
        let count = count.clamp(1, MAX_PANES_DESKTOP);
        let grid = Self::grid_for(count, width, orientation);
        let num_rows = grid.len();
        let row_ratios = vec![1.0 / num_rows as f32; num_rows];
        let col_ratios = grid
            .iter()
            .map(|&cols| vec![1.0 / cols as f32; cols])
            .collect();
        Self {
            pane_count: count,
            grid,
            row_ratios,
            col_ratios,
            width,
            orientation,
        }
    }

    /// Which grid `count` already-clamped panes get.
    fn grid_for(
        count: usize,
        width: crate::ui_layout::WidthClass,
        orientation: SplitOrientation,
    ) -> Vec<usize> {
        match orientation {
            SplitOrientation::Rows => return vec![1; count],
            SplitOrientation::Columns => return vec![count],
            SplitOrientation::Auto => {}
        }

        // **The compact table, and the only row that differs is 2.**
        //
        // `2 => [1, 1]`: two columns of a sub-600pt window are under 300pt
        // wide against roughly 600pt of map height. A plan view is a circle,
        // so what a pane can show is set by its *minor* axis — under 300pt
        // side by side against about 300pt each when stacked, and the stacked
        // pair spends its chrome (pill row, scale bars) on the axis it has to
        // spare rather than the one that is binding.
        //
        // `3 => [2, 1]`, the same as the wide table, and this is the
        // deliberate part. Three stacked strips give every pane a ~200pt minor
        // axis and the ~40pt of pill row and bottom margin comes straight off
        // it, leaving ~165pt of usable circle each. `[2, 1]` gives the top
        // pair a ~195pt minor axis with the chrome falling on their ~310pt
        // height instead, and hands the bottom pane the full ~310pt. Two of
        // the three panes come out ahead and the third comes out far ahead.
        // Nothing about three-across applies: `[2, 1]` is never three columns.
        //
        // `4 => [2, 2]`, unchanged: splitting both axes once is the best a
        // squarish window can do for four circles at any width.
        //
        // 5 and 6 are unreachable on a compact width through the picker —
        // `WidthClass::max_panes` offers `MAX_PANES_MOBILE` — but a config
        // naming them still loads. They get the wide table rather than a
        // compact arrangement nobody has argued for.
        if width == crate::ui_layout::WidthClass::Compact && count == 2 {
            return vec![1, 1];
        }

        match count {
            1 => vec![1],
            2 => vec![2],
            3 => vec![2, 1],
            4 => vec![2, 2],
            5 => vec![3, 2],
            6 => vec![3, 3],
            // Unreachable after the clamp above. Left as a total match rather
            // than a panic: a layout is not worth crashing over, and the clamp
            // is what makes this arm dead.
            _ => vec![1],
        }
    }

    /// Re-ask the grid for the room there now is and the split now preferred,
    /// and report whether the grid moved.
    ///
    /// A width change that leaves the grid alone keeps the dragged dividers:
    /// the ratios are the user's, and only a different arity invalidates them.
    pub(crate) fn reflow(
        &mut self,
        width: crate::ui_layout::WidthClass,
        orientation: SplitOrientation,
    ) -> bool {
        if self.width == width && self.orientation == orientation {
            return false;
        }
        let grid = Self::grid_for(self.pane_count, width, orientation);
        if grid == self.grid {
            self.width = width;
            self.orientation = orientation;
            return false;
        }
        *self = Self::for_count(self.pane_count, width, orientation);
        true
    }

    /// The dragged divider positions, for persistence.
    pub(crate) fn ratios(&self) -> (&[f32], &[Vec<f32>]) {
        (&self.row_ratios, &self.col_ratios)
    }

    /// **Take divider positions from outside — a config file — or refuse
    /// them.** Nothing here trusts the caller: the arity has to match the grid
    /// this layout actually has, every ratio has to be at least [`MIN_RATIO`],
    /// and every run has to sum to 1.0. A file failing any of it leaves the
    /// [`Self::for_count`] defaults in place rather than producing a
    /// zero-height pane.
    ///
    /// Returns whether the ratios were adopted.
    pub(crate) fn adopt_ratios(&mut self, rows: &[f32], cols: &[Vec<f32>]) -> bool {
        if !ratios_valid(rows, self.grid.len()) || cols.len() != self.grid.len() {
            return false;
        }
        if !cols
            .iter()
            .zip(self.grid.iter())
            .all(|(run, &wanted)| ratios_valid(run, wanted))
        {
            return false;
        }
        self.row_ratios = rows.to_vec();
        self.col_ratios = cols.to_vec();
        true
    }

    pub fn grid(&self) -> &[usize] {
        &self.grid
    }

    pub fn pane_rect(&self, pane_idx: usize, total_rect: egui::Rect) -> egui::Rect {
        let mut row_y = total_rect.top();
        let mut idx = 0;
        for (row_idx, &cols) in self.grid.iter().enumerate() {
            let row_height = total_rect.height() * self.row_ratios[row_idx];
            if pane_idx < idx + cols {
                let col_in_row = pane_idx - idx;
                let col_x: f32 = self.col_ratios[row_idx][..col_in_row].iter().sum();
                let col_width = total_rect.width() * self.col_ratios[row_idx][col_in_row];
                let min_x = total_rect.left() + total_rect.width() * col_x;
                return egui::Rect::from_min_size(
                    egui::pos2(min_x, row_y),
                    egui::vec2(col_width, row_height),
                );
            }
            row_y += row_height;
            idx += cols;
        }
        // Fallback — shouldn't happen with valid index
        total_rect
    }

    /// Handle draggable dividers between panes. Call AFTER rendering pane maps
    /// so divider interactions take priority over map panning in the overlap zone.
    pub fn handle_dividers(&mut self, ui: &mut egui::Ui, total_rect: egui::Rect) {
        if self.pane_count <= 1 {
            return;
        }

        let mut y = total_rect.top();
        for row_idx in 0..self.grid.len().saturating_sub(1) {
            y += total_rect.height() * self.row_ratios[row_idx];
            let divider_rect = egui::Rect::from_min_max(
                egui::pos2(total_rect.left(), y - DIVIDER_HALF_WIDTH),
                egui::pos2(total_rect.right(), y + DIVIDER_HALF_WIDTH),
            );
            let id = egui::Id::new(("h_div", row_idx));
            drag_divider(
                ui,
                divider_rect,
                id,
                &mut self.row_ratios,
                row_idx,
                total_rect.height(),
                true,
            );
        }

        let mut row_y = total_rect.top();
        for (row_idx, &cols) in self.grid.iter().enumerate() {
            let row_height = total_rect.height() * self.row_ratios[row_idx];
            let mut col_x = total_rect.left();
            for col_idx in 0..cols.saturating_sub(1) {
                col_x += total_rect.width() * self.col_ratios[row_idx][col_idx];
                let divider_rect = egui::Rect::from_min_max(
                    egui::pos2(col_x - DIVIDER_HALF_WIDTH, row_y),
                    egui::pos2(col_x + DIVIDER_HALF_WIDTH, row_y + row_height),
                );
                let id = egui::Id::new(("v_div", row_idx, col_idx));
                drag_divider(
                    ui,
                    divider_rect,
                    id,
                    &mut self.col_ratios[row_idx],
                    col_idx,
                    total_rect.width(),
                    false,
                );
            }
            row_y += row_height;
        }
    }
}

/// Shared divider drag logic: interact, apply ratio delta, set cursor.
/// `use_y_axis = true` for horizontal dividers (row splits), `false` for vertical (column splits).
fn drag_divider(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    id: egui::Id,
    ratios: &mut [f32],
    idx: usize,
    total_extent: f32,
    use_y_axis: bool,
) {
    let response = ui.interact(rect, id, egui::Sense::drag());
    if response.dragged() {
        let delta = if use_y_axis {
            response.drag_delta().y
        } else {
            response.drag_delta().x
        };
        let ratio_delta = delta / total_extent;
        let new_a = ratios[idx] + ratio_delta;
        let new_b = ratios[idx + 1] - ratio_delta;
        if new_a >= MIN_RATIO && new_b >= MIN_RATIO {
            ratios[idx] = new_a;
            ratios[idx + 1] = new_b;
        }
    }
    if response.hovered() || response.dragged() {
        let cursor = if use_y_axis {
            egui::CursorIcon::ResizeVertical
        } else {
            egui::CursorIcon::ResizeHorizontal
        };
        ui.ctx().set_cursor_icon(cursor);
    }
}

/// Which grid a pane count gets, and what a config file may say about it.
#[cfg(test)]
mod split_tests;

#[cfg(test)]
mod render_params_tests;

/// The section loop's identity and the plan-view/section collision it closes.
#[cfg(test)]
mod section_loop_tests;

/// Which layer a 3D pane asks for a grid, and what it says when none can.
#[cfg(test)]
mod volume_ask_tests;

/// Which volume a 3D pane is about, and whether it still needs building.
#[cfg(test)]
mod volume_due_tests;

/// Which layer the loop transport addresses, and what a config says about it.
#[cfg(test)]
mod transport_addressing_tests;

#[cfg(test)]
mod tests;
