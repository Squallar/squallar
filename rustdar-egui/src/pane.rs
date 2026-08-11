use crate::overlay_cache::OverlayTextureCache;
use chrono::NaiveDateTime;
use rustdar_overlays::render::overlay_state::OverlayKind;
use rustdar_radar::sites::RadarSite;
use rustdar_radar::types::{RadarProduct, RenderView, ScanInfo};
use std::collections::HashMap;
use std::sync::Arc;
use walkers::MapMemory;

#[path = "pane_content.rs"]
mod content;

// Re-exported so a pane's kind is named where a pane is named:
// `rustdar_egui::pane::PaneKind`, alongside `LoopPhase` and `RenderTarget`. The
// split into a second file is about how much there is to say about each half,
// not about them being different things.
pub use content::{
    CrossSectionPane, DEFAULT_HALF_WIDTH_KM, DEFAULT_VERTICAL_EXAGGERATION, GeoPoint,
    MAX_VERTICAL_EXAGGERATION, MIN_VERTICAL_EXAGGERATION, OrbitCamera, OrbitDelta, PaneContent,
    PaneKind, SectionLine, SectionTarget, SectionUnavailable, VolumePane, VolumeRegion,
    VolumeStamp, VolumeTarget, VolumeViewMode,
};

const DEFAULT_PANE_ZOOM: f64 = 4.0;

/// Identifies a pane in the multi-pane layout.
pub type PaneId = usize;

/// Holds the radar image texture and its associated metadata.
#[derive(Clone)]
pub struct RadarImageData {
    pub texture: egui::TextureHandle,
    pub lat: f64,
    pub lon: f64,
    pub max_range_km: f64,
    pub value_data: Arc<Vec<f32>>,
}

/// Holds a rendered cross-section raster and the little that has to travel with
/// it for the pane to draw honest axes over it.
///
/// The counterpart of [`RadarImageData`] for a section loop, and it carries the
/// same amount of truth: what the picture is *of*, so the frame beneath the
/// pointer and the frame on the glass cannot describe different volumes.
///
/// # What it deliberately does not carry
///
/// The `CrossSection` behind the raster is ~18 MB natively — an RGBA plane, an
/// `f32` value plane and a status plane — and only the value and status planes
/// serve the hover readout. Retaining them per frame would cost
/// `MAX_LOOP_RENDER_BUDGET` × 18 MB (540 MB on desktop) to answer a question
/// nobody can ask of a moving picture. So a section loop frame drops them, and
/// the pane's hover readout goes quiet while a loop runs — exactly as a plan-view
/// loop frame's does, which carries an empty `value_data` for the same reason
/// (see `App::rendered_image`).
///
/// [`axes`](Self::axes) and [`tilt_elevations_deg`](Self::tilt_elevations_deg)
/// are kept because they are *labels on this picture*: the height and distance
/// scales, and the tilt ladder drawn over it. A few dozen floats, and without
/// them a loop would animate the raster under the previous frame's axes.
#[derive(Clone)]
pub struct SectionImageData {
    pub texture: egui::TextureHandle,
    /// The height/distance scales this raster was cut against.
    pub axes: rustdar_radar::xsect::SectionAxes,
    /// Where the ladder's rungs are, in degrees of beam elevation.
    pub tilt_elevations_deg: Vec<f64>,
    /// The fingerprint of the tilt ladder this frame was cut from, from
    /// [`rustdar_radar::sampler::ladder_fingerprint`].
    ///
    /// **The one thing that can change under a loop frame.** A frame's scan is
    /// normally immutable once downloaded, which is what makes a loop frame's
    /// picture permanent — but the newest frame is not: the live poll re-caches
    /// its volume under the same `(site, timestamp)` key as more of it seals, so
    /// a section cut from a two-rung ladder can sit at the head of a loop while
    /// the real volume grows to fourteen.
    ///
    /// This is the *same* notion of section staleness the live pane already
    /// uses — `SectionTarget::ladder` — reused rather than reinvented, and for
    /// the identical reason: it moves when a rung's chosen sweep or the declared
    /// pattern moves, and holds still for the seals that change neither.
    pub ladder: u64,
}

/// A finished loop frame's picture, in whichever shape its pane draws.
///
/// # Why the two shapes are one field
///
/// Everything about the loop machinery that is *not* the picture — the render
/// set, the eviction, the readiness settle, the playhead's skip-to-the-next-
/// textured-frame — asks only "does this frame have an image yet?". Keeping the
/// two kinds in two `Option` fields would make that question ambiguous and give
/// a frame a state where both are set, which is a frame depicting two volumes.
/// One field has neither problem, and the arms are what force every consumer to
/// say which shape it means.
///
/// That is the loop path's half of the axis [`RenderCacheKey`]'s doc describes:
/// a plan view and a section of the same product at the same site are the same
/// `(site, product, elevation)` and completely different pictures. On the static
/// path they would collide in an LRU; here they would collide in a **broadcast**
/// — see [`LoopPlaybackState::view`].
///
/// [`RenderCacheKey`]: https://docs.rs/rustdar-frontend
#[derive(Clone)]
pub enum LoopFrameImage {
    /// An `IMAGE_SIZE²` plan-view raster, positioned by the site's coordinates.
    PlanView(RadarImageData),
    /// A `SECTION_WIDTH × SECTION_HEIGHT` vertical slice along the loop's line.
    Section(SectionImageData),
    /// A **resident voxel grid**, named rather than held.
    ///
    /// The odd one out, and deliberately so: the other two variants carry the
    /// picture, and this one carries what the picture is raymarched *from*. A
    /// 3D pane's image is a function of the camera, so a cached raster would
    /// be invalidated by one orbit; the grid is not, and swapping which
    /// resident grid the march samples is a `set_bind_group` and a 192-byte
    /// uniform write. See `PaneKind::can_loop`.
    ///
    /// It is a name rather than an `Arc<VoxelGrid>` because the grids live in
    /// the frontend's single `VolumeStore`, keyed by target and shared between
    /// panes — two 3D panes orbiting one volume already share one build and
    /// one upload, and a loop frame holding its own handle would defeat that
    /// and put a `rustdar_radar` type in a pane.
    Volume(VolumeFrameGrid),
}

/// The resident grid one 3D loop frame marches, named by what built it.
///
/// `target` is the whole identity — site, volume time, product and region — and
/// is what the painter is aimed at while this frame is the playhead's. `id` is
/// the `VolumeStore` id its GPU upload is keyed by, carried so a consumer can
/// tell two frames apart without comparing a `String` and a `NaiveDateTime`,
/// and so a test can assert that the resident set is exactly the frame list.
#[derive(Clone, Debug, PartialEq)]
pub struct VolumeFrameGrid {
    /// The store id of the resident grid. Unique per build; the GPU side keeps
    /// exactly the ids the store is still holding.
    pub id: u64,
    /// Everything the grid was built from.
    pub target: VolumeTarget,
}

impl LoopFrameImage {
    /// Which view this picture is, so a consumer that holds one can be checked
    /// against the loop it is about to be placed in.
    pub fn view(&self) -> RenderView {
        match self {
            Self::PlanView(_) => RenderView::PlanView,
            Self::Section(_) => RenderView::CrossSection,
            Self::Volume(_) => RenderView::Volume,
        }
    }

    /// The plan-view image, or `None` if this frame is a section.
    pub fn plan_view(&self) -> Option<&RadarImageData> {
        match self {
            Self::PlanView(image) => Some(image),
            Self::Section(_) | Self::Volume(_) => None,
        }
    }

    /// The section image, or `None` if this frame is a plan view.
    pub fn section(&self) -> Option<&SectionImageData> {
        match self {
            Self::Section(image) => Some(image),
            Self::PlanView(_) | Self::Volume(_) => None,
        }
    }

    /// The resident grid this frame marches, or `None` if it is a raster.
    pub fn volume(&self) -> Option<&VolumeFrameGrid> {
        match self {
            Self::Volume(grid) => Some(grid),
            Self::PlanView(_) | Self::Section(_) => None,
        }
    }
}

/// A single rendered frame in a radar loop.
pub struct LoopFrame {
    /// UTC timestamp of this scan.
    pub timestamp: NaiveDateTime,
    /// Rendered picture, `None` if not yet rendered or evicted.
    ///
    /// Named for what it is rather than for the texture inside it: a section
    /// frame carries axes and a ladder fingerprint beside its handle, and the
    /// consumers that ask whether a frame is drawn must not have to know which
    /// kind they are looking at to ask.
    pub image: Option<LoopFrameImage>,
    /// True while a background render is in progress for this frame.
    pub render_in_flight: bool,
    /// True once a render for this frame has been attempted and produced nothing
    /// (no matching sweep for the selected product/elevation, or the render itself
    /// failed). Terminal for this frame's current scan data: the dispatcher stops
    /// retrying it, and it no longer holds up loop readiness. Without this, an
    /// unrenderable frame would either be re-spawned every frame forever or wedge
    /// the loop in `Rendering` permanently.
    pub render_failed: bool,
}

/// Tolerance for comparing two selected elevation angles. Shared with the render
/// dispatcher, which uses it when deciding whether two panes' selections are the
/// same and whether a queued render already covers a frame.
pub const ELEVATION_TOLERANCE: f32 = 0.01;

/// Every input `render_radar_to_image` is given *except the scan itself*: the radar
/// site whose coordinates set the projection, and the product/elevation selection
/// that picks the sweep out of that scan.
///
/// This is the render target key. It is stored on `LoopPlaybackState::rendered_for`,
/// stamped onto every dispatched render, and compared on arrival so a result
/// produced for one target is never painted onto frames keyed to another.
///
/// It identifies an image only together with the scan, and it cannot check the scan
/// itself: the key derives from the loop, not from what the loop was handed. That the
/// scan is the right one is enforced upstream instead, at every point a scan can enter
/// a loop — `LoopDownloadManager` keys its cache on `(site, timestamp)`; a polled scan
/// is appended only to loops on its own site; a scan listing is refused unless it names
/// the site the loop is on, and the site it was listed for travels with the queue so
/// the downloads it produces are filed under it. Those four together are what make a
/// frame's scan and its target name the same radar; the key cannot check any of them.
///
/// What the target does not pin even then is the *sweep*: `elevation` is the selection,
/// and each scan snaps it to whatever sweep it carries. Anything handing one loop's
/// finished image to another has to compare that separately; see
/// [`LoopPlaybackState::frame_accepting_broadcast`].
///
/// `site` is the site the loop's *geometry* was captured for — the same lookup that
/// produced `LoopPlaybackState::site_lat`/`site_lon`, which is what
/// `render_radar_to_image` actually projects with. It is deliberately not the pane's
/// live `site` field: the two can drift (a pane's site is re-synced from the active
/// pane without rebuilding its loop), and it is the geometry the image depends on.
///
/// No `PartialEq` on purpose — `elevation` is an `f32` carried straight from a combo
/// box, so `==` would be the wrong comparison. Use [`RenderTarget::matches`].
#[derive(Clone, Debug)]
pub struct RenderTarget {
    /// NEXRAD site code supplying the projection geometry (e.g. "KTLX").
    pub site: String,
    pub product: RadarProduct,
    /// The pane's *selected* elevation, not the per-scan snapped sweep angle.
    pub elevation: f32,
}

impl RenderTarget {
    pub fn new(site: impl Into<String>, product: RadarProduct, elevation: f32) -> Self {
        Self {
            site: site.into(),
            product,
            elevation,
        }
    }

    /// Whether this target names the same image as `site`/`product`/`elevation`.
    /// Site and product are exact; elevation is compared within
    /// `ELEVATION_TOLERANCE`, since the selection is an `f32` that round-trips
    /// through the UI and the scan's own sweep angles.
    ///
    /// Takes the parts loose so a caller that already holds them — notably
    /// `retarget_renders`, which runs for every looping pane every frame — can ask
    /// without allocating a `RenderTarget` just to throw it away.
    pub fn matches_parts(&self, site: &str, product: RadarProduct, elevation: f32) -> bool {
        self.site == site
            && self.product == product
            && (self.elevation - elevation).abs() <= ELEVATION_TOLERANCE
    }

    /// Whether two targets name the same image.
    pub fn matches(&self, other: &RenderTarget) -> bool {
        self.matches_parts(&other.site, other.product, other.elevation)
    }
}

/// What a **section** loop's frames were cut for, beyond what
/// [`RenderTarget`] already says.
///
/// The two together are one key split across two fields, and they are written
/// by one function ([`LoopPlaybackState::retarget_renders_for`]) precisely so
/// they cannot disagree. The split exists because every pre-existing loop site
/// reads the plan-view half and none of them should have to learn about this
/// one.
///
/// # Both fields are things a picture depends on and a `RenderTarget` cannot say
///
/// * **The line.** A section is a slice along two geographic endpoints. Redraw
///   the line and every frame in the loop is a picture of somewhere else — the
///   exact counterpart of changing the product on a plan-view loop, which
///   `retarget_renders` already discards every frame for.
/// * **The storm motion vector.** A storm-relative section is not a slice of a
///   measured moment: the derivation runs on the way out of the volume, so the
///   picture *is* a function of the vector. This is the same field, for the same
///   reason, as `render_dispatch::SectionInputKey::storm_motion` — which exists
///   because leaving it out shipped the previous vector's field to the worker
///   and redrew the section visibly wrong, with nothing saying so. A loop keyed
///   on time alone would reintroduce that bug once per frame instead of once.
///
/// Bits rather than `f32`s, exactly as `SectionInputKey` stores them, so the
/// comparison is reflexive: a NaN vector would never equal itself and every
/// frame of the loop would be re-cut on every dispatch pass, for ever, with a
/// hot CPU as the only symptom.
///
/// `PartialEq` is derived and compares a stored key against a stored key, never
/// against a re-derived value. That is only safe because
/// [`SectionLine::new`](crate::pane::SectionLine::new) refuses non-finite
/// endpoints.
#[derive(Clone, Debug, PartialEq)]
pub struct SectionLoopKey {
    /// The line every frame is cut along.
    pub line: SectionLine,
    /// The storm motion vector the frames were derived with, as raw bits, and
    /// `None` for every product that does not read one.
    pub storm_motion: Option<(u32, u32)>,
}

impl SectionLoopKey {
    /// The key for `line` under the storm motion vector `motion`, in the same
    /// `(speed_kt, direction_from_deg)` form the extraction is handed.
    ///
    /// The `to_bits` conversion lives here rather than at the call sites for the
    /// reason `SectionInputKey::of` gives: a second copy of it is a second
    /// chance to compare `f32`s that are not reflexive.
    pub fn new(line: SectionLine, motion: Option<(f32, f32)>) -> Self {
        Self {
            line,
            storm_motion: motion.map(|(speed, direction)| (speed.to_bits(), direction.to_bits())),
        }
    }
}

/// The 3D counterpart of [`SectionLoopKey`]: what a resident voxel grid depends
/// on that a [`RenderTarget`] cannot say.
///
/// * **The region.** A grid is resampled over a box of ground, and the region
///   picker exists to spend a fixed cell count over less of it. Re-drag the box
///   and every frame in the loop is a resample of somewhere else — the exact
///   counterpart of redrawing a section's line, and the same reason
///   [`VolumeTarget`] carries it.
/// * **The storm motion vector.** A storm-relative grid is derived on the way
///   out of the volume, so the grid *is* a function of the vector, and the
///   vector is not in the target that keys it. Without it here, an override
///   edit would leave thirteen grids painting the previous vector's field with
///   nothing saying so — the same defect `SectionLoopKey::storm_motion` and
///   `render_dispatch::SectionInputKey::storm_motion` exist for.
///
/// Bits rather than `f32`s, for [`SectionLoopKey`]'s reason: a NaN vector would
/// never equal itself and every grid in the loop would be rebuilt on every
/// dispatch pass for ever — 89 ms of resample apiece, with a hot CPU as the
/// only symptom. The region is compared as itself, which is safe because
/// [`VolumeRegion::new`] refuses a non-finite centre.
///
/// The **elevation** is deliberately absent, and that is a difference from
/// every other loop kind. A grid is resampled from the whole ladder, so the
/// pane's tilt selection changes nothing about it; keying on the tilt would
/// throw away fourteen grids and pay ~2 s to rebuild them identically the first
/// time a user nudged the elevation slider on a 3D pane. See
/// [`LoopPlaybackState::retarget_renders_keyed`], which is where that exception
/// is applied.
#[derive(Clone, Debug, PartialEq)]
pub struct VolumeLoopKey {
    /// The ground every frame is resampled over, or `None` for the default box
    /// about the site.
    pub region: Option<VolumeRegion>,
    /// The storm motion vector the grids were derived with, as raw bits, and
    /// `None` for every product that does not read one.
    pub storm_motion: Option<(u32, u32)>,
}

impl VolumeLoopKey {
    /// The key for `region` under the storm motion vector `motion`, in the same
    /// `(speed_kt, direction_from_deg)` form the extraction is handed.
    pub fn new(region: Option<VolumeRegion>, motion: Option<(f32, f32)>) -> Self {
        Self {
            region,
            storm_motion: motion.map(|(speed, direction)| (speed.to_bits(), direction.to_bits())),
        }
    }
}

/// The view-specific half of a loop's render key.
///
/// One field on [`LoopPlaybackState`] rather than one per view, so that the
/// invariant [`SectionLoopKey`]'s doc states — the halves of a key are written
/// together and cannot disagree — holds across *three* kinds rather than being
/// re-established for each. A loop that changed kind without changing product
/// would otherwise keep a key describing the old kind's picture.
///
/// A plan-view loop has no view half at all, which is [`Option::None`] at the
/// use site rather than a third variant: there is nothing to store, and a
/// variant holding nothing would be a thing to forget to compare.
#[derive(Clone, Debug, PartialEq)]
pub enum LoopViewKey {
    /// See [`SectionLoopKey`].
    Section(SectionLoopKey),
    /// See [`VolumeLoopKey`].
    Volume(VolumeLoopKey),
}

/// The two sweep angles a sibling broadcast has to reconcile.
///
/// A [`RenderTarget`] carries the *selected* elevation; the renderer is given that
/// selection snapped to a sweep the frame's own scan actually carries, and two scans
/// can snap one selection to different sweeps. So an image arriving from another pane
/// is described by a sweep the receiver has to compare against its own, which is a
/// different question from "do we want the same product and elevation".
///
/// The two are a struct rather than a pair of `f32` parameters so they cannot be
/// passed in the wrong order — they are the same type, adjacent, and both plausible.
#[derive(Clone, Copy, Debug)]
pub struct BroadcastSweep {
    /// The sweep angle the incoming image depicts.
    pub rendered: f32,
    /// The sweep the receiving loop's *own* scan for this frame resolves the same
    /// selection to, or `None` if it has no scan for the frame yet (or that scan
    /// carries no sweep for the product). `None` refuses the image: an unverifiable
    /// hand-off is not better than the local render that will follow once the scan
    /// is there.
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
    /// Loop mode is disabled (single-frame mode).
    Inactive,
    /// Loop is enabled and waiting for the scan listing to complete.
    FetchingScanList,
    /// Scans listed and downloads/renders started, waiting to reach render budget.
    Rendering,
    /// Sufficient frames have rendered to allow playback, but playing is not started.
    Ready,
    /// Loop is actively playing/animating forward through frames.
    Playing,
    /// User paused the active loop (has enough rendered frames).
    Paused,
}

/// Per-pane loop playback state.
///
/// Always present on every pane. In single-frame mode (`phase == LoopPhase::Inactive`),
/// `frames` holds at most one entry — the current static radar image. When the
/// user enables loop mode, the phase transitions and multiple historical frames
/// are fetched and rendered.
pub struct LoopPlaybackState {
    /// The current phase of the loop playback lifecycle.
    pub phase: LoopPhase,
    /// Index of the currently displayed frame in `frames`.
    pub current_frame: usize,
    /// Ordered list of frames (oldest-first).
    pub frames: Vec<LoopFrame>,
    /// Lookback duration in seconds that was requested.
    pub lookback_secs: u64,
    /// Instant of the last frame advance (for animation timing).
    pub last_advance: Option<web_time::Instant>,
    /// NEXRAD site code the loop's geometry belongs to, captured at loop creation
    /// from the same lookup as `site_lat`/`site_lon`. Every frame in this loop is
    /// rendered and positioned with those coordinates, so this — not the pane's
    /// live `site` field — is the site half of the render target.
    pub site: String,
    /// Radar site latitude, captured at loop creation for rendering.
    pub site_lat: f64,
    /// Radar site longitude, captured at loop creation for rendering.
    pub site_lon: f64,
    /// The [`RenderTarget`] every frame's render state was produced for, or `None`
    /// before the first dispatch. The user can change the pane's product or
    /// elevation at any time, and both pieces of per-frame render state are
    /// judgements about that selection — a `texture` shows that product, and a
    /// `render_failed` flag means "this scan carries no sweep for that product".
    /// When the selection moves, both are stale; see `retarget_renders`.
    ///
    /// The site rides along because it is a render input like any other, and every
    /// path that hands one loop's image to another pane has to check it. A loop is
    /// rebuilt from scratch when the pane changes site, so this half never moves
    /// under a live loop — it exists so that results and sibling textures carrying
    /// another site's geometry are rejected by construction rather than by luck.
    ///
    /// It does not make a frame's image fully determined on its own — the scan
    /// supplies the rest, and the sweep it snaps the selection to is not in here.
    /// See [`RenderTarget`].
    pub rendered_for: Option<RenderTarget>,
    /// Which kind of picture this loop's frames are, fixed for the life of the
    /// state by the pane kind that started it.
    ///
    /// # This is the loop path's view axis, and it is a safety field
    ///
    /// [`RenderTarget`] is `(site, product, elevation)` with **no view term**,
    /// and that was complete while a loop frame could only ever be a plan-view
    /// tilt. It is not complete now: a section pane and a map pane on the same
    /// site, product and elevation produce two `RenderTarget`s that
    /// [`RenderTarget::matches`] says are the same — and the loop-frame
    /// broadcast in `App::poll_loop_render_results` hands one pane's finished
    /// texture to every sibling whose loop `is_rendered_for` the result's
    /// target. Without this field a 2048² plan view is placed into a section
    /// pane's frame list, and the pane animates a map where a vertical slice
    /// should be, with every caption and axis around it describing the section.
    ///
    /// So the acceptance, donation and result-placement predicates all test it
    /// — and each tests it against the *result's* view, not against the
    /// receiving pane's kind, because a pane and a result can both be sections
    /// and still disagree about which. That is the same lesson `RenderCacheKey`
    /// learnt on the static path, where the collision is a wrong-shaped buffer
    /// handed to `ColorImage::from_rgba_unmultiplied`'s `assert_eq!` on the main
    /// thread rather than a wrong picture.
    ///
    /// [`RenderView::PlanView`] for a loop built before the field existed and
    /// for [`LoopPlaybackState::new`], which is the inactive state.
    pub view: RenderView,
    /// The view-specific half of the render key, or `None` for a plan-view
    /// loop.
    ///
    /// See [`LoopViewKey`] for what is in it and why. Written only by
    /// [`Self::retarget_renders_keyed`], alongside [`Self::rendered_for`], so
    /// the two halves of the key cannot describe different pictures. Read
    /// through [`Self::section_key`] and [`Self::volume_key`], which answer
    /// `None` for a key of the other kind — so a caller that asks for its own
    /// kind's key can never be handed another's.
    pub view_key: Option<LoopViewKey>,
}

/// Per-pane state: each pane independently selects a radar product,
/// elevation, layer toggles, and maintains its own map viewport.
///
/// Every field below is flat, including for a pane that is not a map: what a
/// pane is looking at (site, time, product, viewport, loop) is the same set of
/// questions whether it draws a plan view, a vertical section or a volume. Only
/// [`content`](Self::content) differs by kind, and it is the *only* field that
/// does. See [`PaneContent`]'s module documentation for why — the short version
/// is that ~53 all-panes loops keep working unchanged, one of which
/// (`App::evict_unshown_scans`) is what stops a non-map pane's volume being
/// freed out from under it.
pub struct PaneState {
    /// NEXRAD site code this pane is viewing (e.g. "KTLX").
    pub site: String,
    /// Product/elevation metadata for this pane's site.
    pub scan_info: Option<ScanInfo>,
    /// When the data behind this pane's current radar image was collected (UTC).
    ///
    /// One field for every product, whatever it is derived from: the volume time
    /// for a product read off the Level II scan, the
    /// [`rustdar_radar::level3::ProductStamp`] time for one fetched from the
    /// Level III bucket. The status bar draws it the same way either way — a
    /// product whose age is reported under a different label, or only sometimes, is
    /// a product the user can identify as coming from somewhere else.
    ///
    /// It is the time of *what is drawn*, not of the freshest data in hand, and
    /// that is the point. `level3::latest_key` falls back to the previous UTC day,
    /// so a site down since yesterday paints a field up to ~48 h old over a live
    /// basemap; nothing else on screen says so, because the scan line beside it
    /// describes the Level II volume.
    ///
    /// `None` before any render has reached the pane, and for a bucket key whose
    /// tail does not parse — an *unknown* time, which the bar reports by drawing
    /// nothing rather than by guessing.
    pub data_time: Option<NaiveDateTime>,
    pub selected_product: RadarProduct,
    pub selected_elevation: f32,
    /// Whether this pane is viewing the latest (live) data.
    pub viewing_live: bool,
    /// Time navigation step size in seconds (0 = single scan mode).
    pub time_step_secs: i64,
    /// Whether this pane follows shared time (plan §3.7). Persisted; default
    /// **true** — every pane before the field existed behaved as linked.
    ///
    /// Off means frozen: the pane is left out of
    /// [`Gui::time_sync_targets`](crate::Gui), so the loop fan-out skips it
    /// and `propagate_layer_sync` leaves its `viewing_live`/`time_step_secs`
    /// alone. It is deliberately *not* consulted by the site-wide scan
    /// delivery (`set_scan_info_for_site`): the volume a site holds is shared
    /// state, and what this flag freezes is the pane's own time posture.
    pub time_link: bool,
    /// Whether this pane's viewport belongs to the linked group (M11's
    /// per-pane sync model — the successor to the retired `Gui`-global
    /// `viewport_sync`). Persisted; default **true**.
    ///
    /// The linked group is the visible map panes with this on: a pan or zoom
    /// on a linked pane moves the group, a change on an unlinked pane moves
    /// only itself, and an unlinked pane is never written by the group's
    /// convergence (`Gui::sync_viewports`).
    pub viewport_link: bool,
    /// Whether this pane's layer state belongs to the linked group (M11 —
    /// the successor to the retired `Gui`-global `sync_layers`). Persisted;
    /// default **true**.
    ///
    /// `propagate_layer_sync` converges site, scan, product, tilt, draw
    /// order and overlay state from a linked **source** to linked
    /// **targets** only: an unlinked pane's edits stay its own, and the
    /// group's edits leave it alone. The time pair inside that pass keeps
    /// its own gate ([`Self::time_link`]).
    pub layer_link: bool,
    pub hover_value: Option<String>,
    /// Hover tooltip text from overlay handlers (e.g. model data CIN value).
    pub overlay_hover_value: Option<String>,
    pub last_hover_pos: Option<egui::Pos2>,
    pub map_memory: MapMemory,
    /// Per-overlay-type texture caches (background-rendered), keyed by `OverlayKind`.
    /// Only texture overlay kinds (SPC, NWS, discussions) have cache entries.
    pub overlay_textures: HashMap<OverlayKind, OverlayTextureCache>,
    /// Per-pane draw order (bottom to top). Controls the visual stacking of all
    /// map layers. Persisted across sessions.
    pub draw_order: Vec<OverlayKind>,
    /// Per-pane overlay enabled state (master visibility for each overlay kind).
    /// Propagated from a layer-linked active pane to the other layer-linked
    /// panes by `propagate_layer_sync`.
    pub enabled_overlays: HashMap<OverlayKind, bool>,
    /// Per-pane overlay handler config snapshots (serialized handler state per kind).
    /// Swapped into/out of the global OverlayRegistry around access points so each
    /// pane can independently configure overlay sub-controls (categories, day, etc.).
    pub overlay_configs: HashMap<OverlayKind, serde_json::Value>,
    /// Radar display state. Always present; in single-frame mode holds at most
    /// one frame (the current static radar image). In multi-frame mode holds
    /// the full animated loop.
    pub loop_state: LoopPlaybackState,
    /// Which site is currently being loaded for this pane (transient loading indicator).
    pub loading_site: Option<String>,
    /// Generation counter for RadarSites texture invalidation.
    /// Bumped when site, loading_site, or theme changes.
    pub radar_sites_render_gen: u64,
    /// What kind of pane this is, and the state that kind needs.
    ///
    /// The single source of [`Self::kind`] — there is deliberately no `kind`
    /// field beside this one, because two fields can disagree and a mismatched
    /// pair is a state every render frame would then have to have an opinion
    /// about.
    ///
    /// **Nothing may read this through `Gui::panes[..]` or `Gui::active_pane()`
    /// during the UI pass.** Six places `std::mem::take` a pane for the duration
    /// of a draw, and a taken slot holds `PaneState::default()`, which is a
    /// *map* pane whatever the real one is. Branch on the taken value instead.
    pub content: PaneContent,
}

impl Default for LoopPlaybackState {
    fn default() -> Self {
        Self::new()
    }
}

impl LoopPlaybackState {
    /// Create a default single-frame (non-loop) state.
    ///
    /// The site fields are placeholders: the state is `Inactive` with no frames, so
    /// nothing is ever rendered or accepted against them.
    pub fn new() -> Self {
        Self {
            phase: LoopPhase::Inactive,
            current_frame: 0,
            frames: Vec::new(),
            lookback_secs: 0,
            last_advance: None,
            site: String::new(),
            site_lat: 0.0,
            site_lon: 0.0,
            rendered_for: None,
            view: RenderView::PlanView,
            view_key: None,
        }
    }

    /// Create a new initialized loop state starting the fetch phase.
    ///
    /// Takes the whole [`RadarSite`] rather than a code and a pair of coordinates:
    /// the code is what the render target is compared on and the coordinates are what
    /// frames are actually projected with, so they have to describe the same site. As
    /// separate parameters a caller could pass the pane's site code alongside another
    /// site's coordinates, and every later comparison would be exact and wrong.
    ///
    /// `view` is the pane kind's [`PaneKind::render_view`], captured once here
    /// rather than re-derived at each consumer. A pane cannot change kind under
    /// a running loop — [`PaneState::set_content`] tears the loop down for a kind
    /// that cannot animate, and rebuilds it for one that can — so this is fixed
    /// for the life of the state, exactly like [`Self::site`]. See
    /// [`Self::view`] for what it protects.
    pub fn new_for_loop(lookback_secs: u64, site: &RadarSite, view: RenderView) -> Self {
        Self {
            phase: LoopPhase::FetchingScanList,
            current_frame: 0,
            frames: Vec::new(),
            lookback_secs,
            last_advance: None,
            site: site.name.to_string(),
            site_lat: site.lat,
            site_lon: site.lon,
            rendered_for: None,
            view,
            view_key: None,
        }
    }

    /// True if the loop is active (`new_for_loop` was called; single frame mode uses `Inactive`).
    pub fn is_active(&self) -> bool {
        !matches!(self.phase, LoopPhase::Inactive)
    }

    /// True if actively playing back frames.
    pub fn is_playing(&self) -> bool {
        matches!(self.phase, LoopPhase::Playing)
    }

    /// True if enough frames have rendered for playback to be enabled.
    pub fn is_render_ready(&self) -> bool {
        matches!(
            self.phase,
            LoopPhase::Ready | LoopPhase::Playing | LoopPhase::Paused
        )
    }

    /// True during the initial scan list fetch.
    pub fn is_fetching(&self) -> bool {
        matches!(self.phase, LoopPhase::FetchingScanList)
    }

    /// True if playback was previously started (could be paused or playing).
    pub fn has_playback_started(&self) -> bool {
        matches!(self.phase, LoopPhase::Playing | LoopPhase::Paused)
    }

    /// True if the frames' render state is keyed to exactly this target.
    pub fn is_rendered_for(&self, target: &RenderTarget) -> bool {
        self.rendered_for
            .as_ref()
            .is_some_and(|t| t.matches(target))
    }

    /// The index of the frame a finished render for `timestamp`, produced for
    /// `target`, must be written to — or `None` if the result has to be dropped.
    ///
    /// Two independent ways a result goes stale, and both must be checked:
    ///
    /// - The pane retargeted while the render ran, so the image depicts a site,
    ///   product or elevation the frames are no longer keyed to. Checking "is the
    ///   frame still marked in flight?" cannot catch this: `retarget_renders` clears
    ///   the mark, but the very same dispatch pass re-spawns the frame for the new
    ///   target and marks it again, so the older render's result arrives to a frame
    ///   that *is* in flight. Comparing the target catches it, and a late result that
    ///   still matches the current target is safe to apply: the target fixes every
    ///   render input except the scan, and the scan for a given `(site, timestamp)`
    ///   does not change under a live loop, so the pending render would produce the
    ///   same image. The `(site, timestamp)` qualifier is load-bearing rather than
    ///   pedantic: that is the cache's key, and it is what makes "the scan does not
    ///   change" true. Under a timestamp-only key another site's scan could replace
    ///   this one at any moment and the sentence above would be false.
    /// - The frame is not expecting a result at all: the frame list was rebuilt, the
    ///   graphics state was cleared, or a sibling pane already supplied the texture.
    ///
    /// Returns the *index* rather than a yes/no so the caller cannot look the frame up
    /// a second time and land somewhere else. Timestamps are unique across a frame
    /// list today, but only incidentally — a predicate answering "is some frame with
    /// this timestamp in flight?" paired with a caller fetching "the frame with this
    /// timestamp" is two lookups that are free to disagree, and the frame the
    /// predicate cleared would then stay marked in flight forever.
    /// - The result is a **plan view and this loop is not** (or the reverse).
    ///   [`RenderTarget`] carries no view term, so a section loop and a map loop
    ///   on one site, product and elevation produce targets that `matches` calls
    ///   equal. See [`Self::view`].
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

    /// [`Self::frame_awaiting_render_result`] as a mutable borrow of the frame itself.
    ///
    /// This is what callers use. Handing back the frame rather than its index leaves
    /// nothing for a caller to re-derive: the borrow of `self` is live for as long as
    /// the frame is held, so "look the frame up again by timestamp" is not expressible
    /// at the call site. The index form stays public so the choice can be asserted
    /// directly in tests.
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
    ///
    /// Two panes showing the same site at the same product and elevation render
    /// byte-identical images, so one render can serve both. The site is what makes
    /// that true, and it is not implied by the panes agreeing on product and
    /// elevation: `propagate_layer_sync` converges `PaneState::site` across panes but
    /// never rebuilds their loops, so two panes can agree on every visible control
    /// while their loops still carry different geometry. Handing an image across that
    /// gap positions it at coordinates it was not projected for.
    ///
    /// `sweep` closes the one input the target does not name. `target.elevation` is the
    /// user's selection; what got rendered is that selection snapped to a sweep the
    /// *donor's* scan carries, and the receiver's own scan for this frame may snap it
    /// somewhere else. Accepting then is doubly wrong: the frame takes an image of the
    /// wrong tilt, *and* the receiver's own in-flight render is dropped as redundant by
    /// the caller — so nothing ever corrects it. The dispatcher's suppression test
    /// (`render_already_queued`) already compares the snapped sweep, and suppression is
    /// a promise of acceptance, so the two have to weigh the same thing.
    ///
    /// Only untextured frames qualify — a frame that already has an image is not
    /// improved by an identical one, and overwriting it would churn texture handles.
    ///
    /// The [`view`](Self::view) test is first and is the one that is not about
    /// tilts at all: the incoming image is a plan-view raster, and a section
    /// loop whose target happens to match would otherwise animate a map inside a
    /// vertical slice's axes.
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

    /// [`Self::frame_accepting_broadcast`] as a mutable borrow of the frame itself,
    /// for the same reason as [`Self::frame_awaiting_render_result_mut`].
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
    ///
    /// The mirror of [`Self::frame_accepting_broadcast`] — the dispatcher looks for a
    /// donor *before* rendering, the response path pushes to receivers *after* — and
    /// it must apply the same test, including the site. If the two disagree the
    /// dispatcher suppresses a pane's own render on the promise of a broadcast the
    /// response path then refuses, and the frame is served by neither.
    ///
    /// It takes no sweep argument, unlike acceptance, and that asymmetry is confined to
    /// this direction: a donation is copied on the spot, so there is no promise for a
    /// later test to break. The promise pair is the *other* one — `render_already_queued`
    /// suppressing a render because a sibling's is queued, then that sibling's result
    /// being offered to this loop — and both halves of it compare the sweep. Two loops
    /// that pass this test are on one site and so share one `(site, timestamp)` cache
    /// entry, which is what makes their scans, and therefore their snapped sweeps, the
    /// same to begin with.
    ///
    /// The [`view`](Self::view) test is the same one acceptance applies, and for
    /// the same reason — this is the *before* half of the pair, so if the two
    /// disagreed the dispatcher would suppress a pane's own render on the
    /// promise of a donation the placement path then refuses.
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
    ///
    /// Both halves, always, because they are one key: `rendered_for` carries the
    /// site and product a section was cut under and `section_key` carries the
    /// line and the storm motion vector, and a frame that agrees on one half and
    /// not the other is a picture of a different slice.
    pub fn is_cut_for(&self, target: &RenderTarget, key: &SectionLoopKey) -> bool {
        self.is_rendered_for(target) && self.section_key() == Some(key)
    }

    /// The section half of this loop's key, or `None` — including for a loop
    /// whose key is a *volume* key, which is the point of asking through here
    /// rather than matching on [`Self::view_key`] at each site.
    pub fn section_key(&self) -> Option<&SectionLoopKey> {
        match &self.view_key {
            Some(LoopViewKey::Section(key)) => Some(key),
            Some(LoopViewKey::Volume(_)) | None => None,
        }
    }

    /// The volume half of this loop's key, or `None`. See
    /// [`Self::section_key`].
    pub fn volume_key(&self) -> Option<&VolumeLoopKey> {
        match &self.view_key {
            Some(LoopViewKey::Volume(key)) => Some(key),
            Some(LoopViewKey::Section(_)) | None => None,
        }
    }

    /// The index of the frame awaiting a **section** result for
    /// `timestamp`/`target`/`key`, or `None` if this loop is not owed one.
    ///
    /// [`Self::frame_awaiting_render_result`] for sections, with the same
    /// contract and the same view guard read the other way round. It exists as a
    /// separate function rather than a parameter on that one because the two
    /// take different keys — a plan-view result is identified by a snapped sweep
    /// and a section result by a line and a motion vector — and a single
    /// function taking both would have to accept `None` for whichever half did
    /// not apply, which is a signature that lets a caller pass neither.
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

    /// [`Self::frame_awaiting_section_result`] as a mutable borrow of the frame,
    /// for the reason [`Self::frame_awaiting_render_result_mut`] gives.
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
    ///
    /// The section form of [`Self::frame_accepting_broadcast`]. It takes no
    /// sweep, and the asymmetry is not an oversight: a plan view is one tilt out
    /// of a volume, so two loops agreeing on the selection can still snap it to
    /// different sweeps — a section is cut across *every* tilt, so there is no
    /// snapping to disagree about. What replaces it is `ladder`, the fingerprint
    /// of the ladder the incoming raster was cut from, which must match the
    /// ladder this loop's own scan for the frame resolves to. Two loops sharing
    /// one `(site, timestamp)` cache entry share one scan and so one ladder;
    /// the test is here because the newest frame's scan can be re-cached as more
    /// of the volume seals, and a raster cut from the two-rung version must not
    /// be handed to a loop that is about to cut the fourteen-rung one.
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

    /// [`Self::frame_accepting_section_broadcast`] as a mutable borrow.
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
    ///
    /// The mirror of [`Self::frame_accepting_section_broadcast`], applying the
    /// same tests for the reason [`Self::frame_donatable_to`] gives — a donation
    /// suppresses a render, so the two halves must weigh the same things. The
    /// ladder comes off the candidate frame's own picture rather than from the
    /// caller, because that is what a donation actually copies.
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
    ///
    /// Both pieces of per-frame render state are only meaningful relative to a
    /// selection: a `texture` depicts one product at one elevation, and a
    /// `render_failed` flag records that the frame's scan carries no sweep for that
    /// product. The user can change either at any time from the pane's combo boxes,
    /// which write straight through to the pane. Without this, a frame retired under
    /// a product that only some scans carry would stay blank forever after switching
    /// to a product every scan has — and readiness counts retired frames as settled,
    /// so playback would animate with permanent holes.
    ///
    /// In-flight renders are un-marked as well, since nothing is owed to a frame whose
    /// target moved. That alone does *not* make their results stale — the same dispatch
    /// pass re-spawns and re-marks the frame — so rejecting them is
    /// `frame_awaiting_render_result`'s job, via the target stamped on the response.
    ///
    /// Only the product and elevation are parameters: the target's site is the loop's
    /// own `site`, which is fixed for the life of a `LoopPlaybackState`. A pane that
    /// changes site gets a whole new loop state rather than a retarget.
    pub fn retarget_renders(&mut self, product: RadarProduct, elevation: f32) -> bool {
        self.retarget_renders_for(product, elevation, None)
    }

    /// [`Self::retarget_renders`] including the section half of the key.
    ///
    /// `section` is `None` for a plan-view loop and `Some` for a section loop.
    /// Passing `None` on a section loop would therefore *also* count as a
    /// change and discard every frame — which is the safe direction, and is what
    /// a caller that has lost the line should get.
    pub fn retarget_renders_for(
        &mut self,
        product: RadarProduct,
        elevation: f32,
        section: Option<SectionLoopKey>,
    ) -> bool {
        self.retarget_renders_keyed(product, elevation, section.map(LoopViewKey::Section))
    }

    /// [`Self::retarget_renders`] including whichever view half this loop has.
    ///
    /// **The only writer of either half**, which is what makes them one key
    /// rather than two fields that can disagree: a caller cannot move the line
    /// or the region without the frames keyed to the old one being discarded,
    /// because there is no way to write [`Self::view_key`] that does not come
    /// through here.
    ///
    /// # A volume loop ignores the elevation, and only a volume loop may
    ///
    /// The tilt is part of the target for the two raster kinds because it
    /// chooses which sweep is drawn. A voxel grid is resampled from the whole
    /// ladder, so it chooses nothing — and keying on it would discard the
    /// entire resident set and pay ~2 s of rebuild the first time a user
    /// nudged the elevation slider on a 3D pane, for fourteen grids that would
    /// come back byte-identical. The stored `rendered_for` still carries the
    /// pane's selection, so the sibling broadcast and every result-acceptance
    /// test are unchanged; what this drops is only the *comparison*, and only
    /// for the kind whose picture the tilt cannot move.
    pub fn retarget_renders_keyed(
        &mut self,
        product: RadarProduct,
        elevation: f32,
        key: Option<LoopViewKey>,
    ) -> bool {
        let tilt_matters = !matches!(key, Some(LoopViewKey::Volume(_)));
        // Runs for every looping pane every frame, and almost always finds no change,
        // so ask before building a target rather than allocating one to throw away.
        if self.rendered_for.as_ref().is_some_and(|t| {
            if tilt_matters {
                t.matches_parts(&self.site, product, elevation)
            } else {
                t.site == self.site && t.product == product
            }
        }) && self.view_key == key
        {
            return false;
        }

        // Nothing to discard before the first dispatch — frames start blank.
        let had_previous_target = self.rendered_for.is_some();
        self.rendered_for = Some(RenderTarget::new(self.site.clone(), product, elevation));
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

    /// Drop textures outside the intended render set once more than `budget` frames
    /// are textured, capping loop memory.
    ///
    /// Deliberately shares `render_set_indices` with the dispatcher and the readiness
    /// check: an eviction rule that disagreed with the dispatcher could drop the
    /// texture of a frame that is about to be re-rendered, churning renders forever.
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
    ///
    /// This is the "intended render set". The dispatcher spawns renders for exactly
    /// these frames, and readiness waits for exactly these frames, so both must use
    /// this function — if they disagree, readiness can fire over frames that were
    /// never rendered. `budget` is clamped to the frame count.
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
    ///
    /// This is deliberately *not* "nothing is in flight right now". The concurrent
    /// render budget is shared with static pane renders, so a batch of loop frames
    /// can be starved: only some spawn, those finish, and for an instant nothing is
    /// in flight even though most of the set is still blank. Treating that as ready
    /// makes playback animate mostly-empty frames. A frame is settled only if it has
    /// a texture, or nothing is going to produce one for it (no render in flight, and
    /// either it has been ruled out via `render_failed` or its scan has not
    /// downloaded yet — the latter is gated separately by the download check).
    ///
    /// `scan_available` reports whether the frame's scan data has been downloaded;
    /// that cache lives outside the pane, so the caller supplies it.
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
    pub fn new() -> Self {
        Self::with_site("KTLX".to_string())
    }

    /// Create a new pane viewing the given site.
    pub fn with_site(site: String) -> Self {
        let mut map_memory = MapMemory::default();
        let _ = map_memory.set_zoom(DEFAULT_PANE_ZOOM);
        Self {
            site,
            scan_info: None,
            data_time: None,
            selected_product: RadarProduct::Reflectivity,
            selected_elevation: 0.0,
            viewing_live: true,
            time_step_secs: 600,
            time_link: true,
            viewport_link: true,
            layer_link: true,
            hover_value: None,
            overlay_hover_value: None,
            last_hover_pos: None,
            map_memory,
            overlay_textures: OverlayKind::all()
                .iter()
                .map(|&k| (k, OverlayTextureCache::new()))
                .collect(),
            draw_order: OverlayKind::default_draw_order(),
            enabled_overlays: HashMap::new(),
            overlay_configs: HashMap::new(),
            loop_state: LoopPlaybackState::new(),
            loading_site: None,
            radar_sites_render_gen: 0,
            content: PaneContent::Map,
        }
    }

    /// What kind of pane this is.
    ///
    /// Derived from [`Self::content`] rather than stored beside it. See the
    /// warning on that field: during the UI pass this answers `Map` for a pane
    /// that has been `mem::take`n, so read it from the value that was taken.
    pub fn kind(&self) -> PaneKind {
        self.content.kind()
    }

    /// Whether this is the plan-view map pane every pane used to be.
    ///
    /// The predicate the all-panes loops that are *only* about maps filter on —
    /// render dispatch, the sibling texture broadcast, loop synchronisation.
    pub fn is_map(&self) -> bool {
        matches!(self.content, PaneContent::Map)
    }

    /// Whether this pane can animate a sequence of past volumes.
    ///
    /// [`PaneKind::can_loop`] is where the classification by kind lives; this
    /// adds the one thing a kind cannot answer.
    ///
    /// # A section pane must be aimed before it can loop
    ///
    /// A cross-section is cut along a line, and until the user has drawn one
    /// there is nothing for a frame to be a picture *of*. A loop enabled in that
    /// state would fill with frames nothing can cut, and because their volumes
    /// have downloaded perfectly well, `render_set_settled` would never call the
    /// batch settled: the loop would sit in `Rendering` for the session, with
    /// the transport showing `0/n` and no way to tell that the fix is to draw a
    /// line. Refusing here — where the timeline's toggle and
    /// `Gui::loop_sync_targets` both read it — is what keeps that state
    /// unreachable, and the pane's own empty state already says what to do.
    ///
    /// `is_none_or` rather than a match on the kind: a map pane has no section
    /// state and answers on its kind alone.
    pub fn can_loop(&self) -> bool {
        self.kind().can_loop() && self.cross_section().is_none_or(|s| s.line.is_some())
    }

    /// This pane's cross-section state, or `None` if it is not a section pane.
    pub fn cross_section(&self) -> Option<&CrossSectionPane> {
        match &self.content {
            PaneContent::CrossSection(section) => Some(section),
            _ => None,
        }
    }

    /// [`Self::cross_section`], mutably.
    pub fn cross_section_mut(&mut self) -> Option<&mut CrossSectionPane> {
        match &mut self.content {
            PaneContent::CrossSection(section) => Some(section),
            _ => None,
        }
    }

    /// This pane's 3D volume state, or `None` if it is not a volume pane.
    pub fn volume(&self) -> Option<&VolumePane> {
        match &self.content {
            PaneContent::Volume(volume) => Some(volume),
            _ => None,
        }
    }

    /// [`Self::volume`], mutably.
    pub fn volume_mut(&mut self) -> Option<&mut VolumePane> {
        match &mut self.content {
            PaneContent::Volume(volume) => Some(volume),
            _ => None,
        }
    }

    /// Convert this pane to `kind`, keeping everything about *what it is looking
    /// at*: its site, its scan, its product and elevation selection, its
    /// viewport and its layer toggles.
    ///
    /// That is a property of the representation rather than of this function —
    /// only `content` is written, and every other field is flat — which is what
    /// makes converting a pane feel like changing a view rather than like losing
    /// one. A user who has panned to a storm and picked a tilt has said
    /// something; asking for a section of it is not a reason to forget any of
    /// it.
    ///
    /// Converting to the kind it already is does nothing at all, rather than
    /// replacing the per-kind state with a fresh one: re-selecting the current
    /// kind from a menu must not discard a drawn section line or a camera the
    /// user has spent a while aiming.
    ///
    /// # The one exception: an animation loop is torn down
    ///
    /// A loop frame *is* a rendered plan-view tilt, so a pane with no plan view
    /// has nothing to animate — and a loop left running on one is not merely idle,
    /// it is actively harmful in five separate ways, every one of them silent:
    ///
    /// * `App::sync_loop_playback_start` holds **every** looping pane back until
    ///   all of them are render-ready, and a converted pane can never become
    ///   ready — nothing renders its frames and nothing marks them failed. With
    ///   Sync Layers on, one converted pane would stop every map pane's loop from
    ///   ever starting. A deadlock, in the other panes.
    /// * Its queue goes on consuming the *shared* download budget, starving the
    ///   live panes it sits beside.
    /// * The status readout says "Rendering n/m" for ever, with no loop transport
    ///   drawn on this pane to cancel it — the layers panel does not offer one to
    ///   a non-map pane.
    /// * `Gui::any_loop_active` stays true, so the event loop keeps waking at loop
    ///   frame rate for an animation nobody can see.
    /// * Its frame textures are held until the egui context dies.
    ///
    /// So the invariant is that **a non-map pane never has an active loop**, and
    /// it is enforced here, at the transition, rather than by a filter at each of
    /// those five consumers. `SwitchRadarSite` already resets `loop_state` for the
    /// same reason, which is why a site switch happens to cure this.
    ///
    /// One half of the teardown is out of reach from here: the host's
    /// `LoopDownloadManager` holds this pane's download queue by index, and a
    /// `PaneState` cannot see it. `App::dispatch_loop_renders` drops it, which also
    /// covers a pane that reached a non-map kind by some route that never called
    /// this — a restored config, or a future auto-create.
    pub fn set_kind(&mut self, kind: PaneKind) {
        if self.kind() == kind {
            return;
        }
        self.set_content(PaneContent::for_kind(kind));
    }

    /// Replace this pane's per-kind content wholesale, as the config loader does
    /// when it has both the kind and the state in hand.
    ///
    /// The one writer of `content` that enforces what a kind change implies, so
    /// every route to a non-map pane — the menu, a restored config, a test
    /// fixture — arrives with the same invariants. See [`Self::set_kind`].
    /// # Why a *kind change* tears the loop down, not just a change to a kind
    /// that cannot loop
    ///
    /// A loop's frames are pictures of one shape, and
    /// [`LoopPlaybackState::view`] is the field that says which. Converting a
    /// looping map pane into a section pane leaves a frame list full of
    /// `IMAGE_SIZE` square plan-view rasters under a state that now claims to be
    /// a section — every acceptance and donation predicate would refuse them,
    /// which is the point of that field, so nothing would be *drawn* wrongly;
    /// but the pane would animate a list of frames nothing can fill and hold
    /// `MAX_LOOP_RENDER_BUDGET` textures alive to do it.
    ///
    /// So the rule is the wider one: any kind change resets the loop, and a kind
    /// that cannot loop at all resets it as well — the second clause covers the
    /// path where a caller replaces one `Volume` content with another.
    pub fn set_content(&mut self, content: PaneContent) {
        let previous = self.kind();
        self.content = content;
        if self.kind() != previous || !self.kind().can_loop() {
            self.loop_state = LoopPlaybackState::new();
        }
    }

    /// The currently active radar image (from loop frame or static render).
    pub fn active_image(&self) -> Option<&RadarImageData> {
        self.active_loop_image().and_then(LoopFrameImage::plan_view)
    }

    /// The playing frame's cross-section raster, or `None` when this pane is not
    /// animating a section.
    ///
    /// The section counterpart of [`Self::active_image`], and separate from it
    /// for the reason [`LoopFrameImage`] exists: the two are different shapes
    /// with different labels around them, and a caller that asked for "the
    /// image" without saying which would draw a plan view into a section's axes.
    pub fn active_section_image(&self) -> Option<&SectionImageData> {
        self.active_loop_image().and_then(LoopFrameImage::section)
    }

    /// The playing frame's resident voxel grid, or `None` when this pane is not
    /// animating a 3D volume — including while its loop is still filling.
    ///
    /// `None` is what makes the 3D pane keep showing the *live* volume until
    /// the loop is ready, exactly as a map pane goes on showing its static
    /// render: the caller aims the painter at this target when there is one and
    /// at the live stamp when there is not, and there is no third state in
    /// which the pane is blank because a loop was switched on.
    pub fn active_volume_frame(&self) -> Option<&VolumeFrameGrid> {
        self.active_loop_image().and_then(LoopFrameImage::volume)
    }

    /// Whichever picture the loop's playhead is on, before it is narrowed to a
    /// kind. One lookup, so the two accessors above cannot walk to different
    /// frames.
    fn active_loop_image(&self) -> Option<&LoopFrameImage> {
        self.loop_state
            .frames
            .get(self.loop_state.current_frame)
            .and_then(|f| f.image.as_ref())
    }

    /// When the data behind the image *currently on screen* was collected.
    ///
    /// Under an active loop that is the playing frame's own volume time, not
    /// [`data_time`](Self::data_time), which describes the static render the
    /// animation replaced — captioning someone else's picture. The status bar used
    /// to draw nothing at all while a loop ran for exactly that reason; answering
    /// the question properly is better, and it is the same answer whichever
    /// datasource the loop reads, since a frame *is* a volume.
    ///
    /// `None` when there is nothing to say: no render has landed, or the loop's
    /// playhead is on a frame that no longer exists.
    pub fn data_time_on_screen(&self) -> Option<NaiveDateTime> {
        if self.loop_state.is_active() {
            return self
                .loop_state
                .frames
                .get(self.loop_state.current_frame)
                .map(|f| f.timestamp);
        }
        self.data_time
    }

    /// What the radar image on screen depicts, **when that is not what this pane
    /// has selected** — the product and sweep the pixels really are, so a caller
    /// can say so.
    ///
    /// `None` means the pane is showing what it claims to be showing, or is
    /// showing nothing at all. Both are honest states with nothing to report; the
    /// case this exists for is the third one, where a product switch leaves the
    /// previous product's image up while the color scale, the tilt picker and the
    /// hover readout have all already moved to the new selection. The label
    /// claiming something the pixels do not show is a correctness problem, not a
    /// cosmetic one, and one that lasts as long as a render — longer for a
    /// Level III product whose object has not landed yet.
    ///
    /// Read off [`crate::overlay_cache::RadarTextureMeta`], which travels *with*
    /// the texture, so this cannot outlive or lag the image it describes: the two
    /// are placed together by `apply_render_to_pane` and dropped together whenever
    /// the radar cache is cleared. That is also what keeps it from firing on a
    /// routine refresh — a new volume for the site clears the dispatcher's
    /// `last_rendered` and re-renders, but the image on screen still depicts the
    /// selected product, so there is nothing to disown.
    ///
    /// The elevation is compared within [`ELEVATION_TOLERANCE`] against the
    /// *snapped* selection from [`get_rendering_params`](Self::get_rendering_params)
    /// — the same value the render was dispatched with — so a selection the scan
    /// snaps onto the sweep already drawn is not a mismatch.
    ///
    /// `None` under an active loop, and not because the question does not arise
    /// there: `LoopPlaybackState::retarget_renders` drops *every* frame texture
    /// the instant the selection moves, so a looping pane never holds a frame
    /// depicting the old product. There is no stale image to disown, and the
    /// loop's own phase chrome covers the wait.
    pub fn stale_image_on_screen(&self) -> Option<(RadarProduct, f32)> {
        if self.loop_state.is_active() {
            return None;
        }
        let meta = self
            .overlay_cache(OverlayKind::Radar)?
            .current
            .as_ref()?
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
            None => meta.product == self.selected_product,
        };
        (!matches_selection).then_some((meta.product, meta.elevation))
    }

    /// Whether this overlay is enabled for this pane.
    ///
    /// Falls back to `false` if the kind has no entry (uninitialised pane).
    pub fn is_overlay_enabled(&self, kind: OverlayKind) -> bool {
        self.enabled_overlays.get(&kind).copied().unwrap_or(false)
    }

    /// Set the per-pane enabled state for a given overlay kind.
    pub fn set_overlay_enabled(&mut self, kind: OverlayKind, enabled: bool) {
        self.enabled_overlays.insert(kind, enabled);
    }

    /// Get the overlay texture cache for a given kind (read-only).
    pub fn overlay_cache(&self, kind: OverlayKind) -> Option<&OverlayTextureCache> {
        self.overlay_textures.get(&kind)
    }

    /// Get the overlay texture cache for a given kind, inserting a default if absent.
    pub fn overlay_cache_mut(&mut self, kind: OverlayKind) -> &mut OverlayTextureCache {
        self.overlay_textures.entry(kind).or_default()
    }

    /// Get rendering params for this pane (product + closest elevation).
    ///
    /// Three cases, and the middle one is what keeps a Level III pane behaving like
    /// a Level II one:
    ///
    /// * The product has tilts — the selection snaps to the nearest.
    /// * The product is **listed with no tilts yet.** The selection stands as it is.
    ///   Only Level III products reach this: `ScanInfo::from_scan` lists them the
    ///   moment a volume loads and fills their angle in from the object's PDB when
    ///   the fetch lands, so there is a window — reopened by every archive poll,
    ///   which rebuilds `ScanInfo` from the volume alone — in which the product is
    ///   selectable and has no angle. Answering `None` there made that window
    ///   visible: `dispatch_pane_renders` took its no-params branch, no render was
    ///   ever dispatched, and the pane went on showing the *previous* product's
    ///   image, captioned as the new one, until the fetch happened to land. A
    ///   Level II product switch holds the old image too — for as long as its render
    ///   takes — so standing the selection up immediately makes the two paths
    ///   indistinguishable rather than merely faster.
    /// * The product is not listed at all — this pane's scan does not offer it, and
    ///   there is nothing to render.
    pub fn get_rendering_params(&self) -> Option<(RadarProduct, f32)> {
        let elevations = self
            .scan_info
            .as_ref()?
            .product_elevations
            .get(&self.selected_product)?;
        let snapped = elevations
            .iter()
            .min_by(|a, b| {
                ((**a - self.selected_elevation).abs())
                    .total_cmp(&((**b - self.selected_elevation).abs()))
            })
            .copied()
            .unwrap_or(self.selected_elevation);
        Some((self.selected_product, snapped))
    }
}

impl Default for PaneState {
    fn default() -> Self {
        Self::new()
    }
}

/// Maximum number of panes on desktop.
pub const MAX_PANES_DESKTOP: usize = 6;

/// Maximum number of panes on mobile.
pub const MAX_PANES_MOBILE: usize = 4;

/// Defines how panes are arranged in a grid layout.
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
}

const MIN_RATIO: f32 = 0.15;
const DIVIDER_HALF_WIDTH: f32 = 4.0;

/// Height/width ratio at which the color scale bars *take up* the horizontal
/// (bottom-edge) orientation, having been vertical.
const COLOR_SCALE_HORIZONTAL_ENTER: f32 = 1.35;
/// Height/width ratio at which they *give it up* again.
///
/// The gap between this and [`COLOR_SCALE_HORIZONTAL_ENTER`] is the whole point:
/// a single threshold — whatever its value — is a point the layout can be parked
/// on or dragged across, and 1.2 sat 4% away from a 16:10 laptop's two-pane
/// split and landed exactly on a 4:3 five-pane one. A ratio inside this band
/// changes nothing at all; only leaving it flips the bars.
const COLOR_SCALE_HORIZONTAL_EXIT: f32 = 1.05;
/// Ratio used for the very first decision, when there is no previous
/// orientation to keep. Sits in the middle of the band.
const COLOR_SCALE_SEED_RATIO: f32 = 1.2;

/// The color scale bars' orientation for the whole map panel, remembered across
/// frames so it has hysteresis instead of a bare threshold.
///
/// # Why the panel and not each pane
///
/// The orientation used to be decided per pane, from the pane's own rect. That
/// is a defensible reading of "the bar should span the pane's shorter axis", but
/// it has two failures a threshold cannot fix:
///
/// * **Mixed orientations on one screen.** A three-pane `[2, 1]` grid on a
///   portrait phone gives two tall panes (h/w ≈ 2.0) and one wide one
///   (h/w ≈ 1.0), so the same screen showed two bottom bars and one right-hand
///   bar. No threshold helps: the panes genuinely disagree.
/// * **Divider drags.** Dragging a divider changes pane rects continuously, so
///   any per-pane threshold is something the user can scrub back and forth
///   across, hopping the bars mid-drag.
///
/// Keying on the panel — the rect the whole grid is laid out in — fixes both
/// outright. Every pane on a screen agrees by construction, and the panel rect
/// does not move when a divider is dragged, so dragging cannot flip anything at
/// all. What is left is window resizes and device rotation, which is what the
/// hysteresis band above is for.
///
/// The single-pane case, which is the overwhelmingly common one on every
/// platform, is unchanged: there the panel *is* the pane.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ColorScaleOrientation {
    /// `None` until the first usable panel rect has been seen.
    horizontal: Option<bool>,
}

impl ColorScaleOrientation {
    /// Resolve the orientation for this frame's `panel_rect`, remembering it.
    ///
    /// Returns `true` for horizontal bars along the bottom edge, `false` for
    /// vertical bars along the right edge. Call once per frame, before the pane
    /// loop, and pass the result to every pane.
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
        Self::for_count(1)
    }
}

impl PaneLayout {
    /// Create a layout for the given pane count, clamped to
    /// `1..=`[`MAX_PANES_DESKTOP`].
    ///
    /// # Why the clamp is here and not at the callers
    ///
    /// The table below covers exactly 1..=6, and its rows sum to the count in
    /// every arm — that agreement between `grid` and `pane_count` is what the
    /// rest of this type is built on. A count outside the table used to fall
    /// through to a one-row, one-column grid while `pane_count` still stored
    /// the raw number, and that pairing is worse than either half alone:
    ///
    /// * [`Self::pane_rect`] walks the grid looking for the row that holds
    ///   `pane_idx` and hands back `total_rect` when it runs out of rows. With
    ///   a one-cell grid, *every* index from 1 upward drew over the whole
    ///   panel.
    /// * `detect_active_pane_click` hit-tests those same rects in order, so
    ///   every rect contained every pointer position: clicking anywhere made
    ///   pane 1 active, clicking again made pane 0 active, and panes 2 and up
    ///   could never be reached at all.
    ///
    /// Neither shows up as an error, a panic or a blank screen — the panes are
    /// all drawn, just all in the same place.
    ///
    /// Every production caller clamps before it gets here today
    /// (`load_ui_config` to `WidthClass::max_panes_absolute`,
    /// the pane picker to the width class's own maximum), so this is currently
    /// unreachable. It is clamped here anyway because "the caller clamped" is a
    /// property of each call site rather than of this type, and the next writer
    /// of `pane_count` — the pane a drawn cross-section auto-creates — is one
    /// commit away. Making the trap unrepresentable costs one line; remembering
    /// it at every future writer costs it forever.
    pub fn for_count(count: usize) -> Self {
        let count = count.clamp(1, MAX_PANES_DESKTOP);
        let grid = match count {
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
        };
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
        }
    }

    /// Get the grid configuration.
    pub fn grid(&self) -> &[usize] {
        &self.grid
    }

    /// Compute the rect for the pane at the given index within the given total rect.
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

        // Horizontal dividers (between rows)
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

        // Vertical dividers (between columns in each row)
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

#[cfg(test)]
mod render_params_tests;

/// The section loop's identity and the plan-view/section collision it closes.
#[cfg(test)]
mod section_loop_tests;

#[cfg(test)]
mod tests;
