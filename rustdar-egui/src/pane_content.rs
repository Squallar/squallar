//! What a pane *is*, as opposed to what it is looking at.
//!
//! Every pane in rustdar has been a plan-view map. A vertical cross-section and
//! a 3D volume view are two more things a pane can be, and this module is the
//! discriminant plus the state each one needs that a map pane does not.
//!
//! # Why the per-kind state is one field and everything else stays flat
//!
//! [`PaneState`](crate::pane::PaneState) gains exactly one field — `content` —
//! and every field it already had stays where it was. That is not tidiness; it
//! is the decision this whole refactor turns on.
//!
//! There are roughly 53 production loops over "every pane" across 44 functions,
//! and almost all of them read `site`, `scan_info`, `viewing_live`,
//! `map_memory` or `loop_state`. Every one of those is still meaningful for a
//! section or a volume pane: a section is cut from a site's volume, it is
//! either live or parked at a time, it has a viewport, and it can loop. With
//! the fields flat, all of those call sites compile and keep working unchanged.
//!
//! One of them is load-bearing rather than merely convenient.
//! `App::evict_unshown_scans` drops every decoded volume no pane is showing, and
//! it decides that by reading `pane.site` and `pane.scan_info.site` on each pane.
//! A section pane that had its own site tucked inside an enum variant would be
//! invisible to that walk, so the volume it is sampling would be evicted from
//! under it — a use-after-evict-shaped bug, in a pass whose whole job is to know
//! what is on screen. Flat fields mean it keeps protecting a non-map pane with
//! no edit at all.
//!
//! # Why the discriminant is a method and not a field
//!
//! Two representations were rejected:
//!
//! * **`kind: PaneKind` beside two `Option`s.** That makes
//!   `kind == CrossSection && cross_section.is_none()` representable, so every
//!   render frame needs an unwrap or a fallback for a state that should not
//!   exist, and config loading can construct it from a file. Two fields can
//!   disagree; one cannot disagree with itself.
//! * **A full `enum PaneState`.** That is what would have broken
//!   `evict_unshown_scans` and the other ~52 loops above.
//!
//! So the kind is *derived* from `content`
//! ([`PaneContent::kind`]), and `content` is the only place the answer lives.
//!
//! # Why the fat variants are boxed
//!
//! `PaneState` is `std::mem::take`n once per pane per frame — six sites do it
//! (`ui_map.rs`, `ui_chrome.rs`, and four in `ui.rs`) — so its size is on the
//! hot path. Boxing [`CrossSectionPane`] and [`VolumePane`] keeps
//! `size_of::<PaneContent>()` at one pointer plus the tag, which keeps a map
//! pane costing what it costs today however much state the other two kinds
//! accumulate.
//!
//! # `Default` means `Map`, and it is a choice with consequences
//!
//! `PaneContent: Default` is the one bound this module is obliged to satisfy,
//! because `PaneState`'s own `Default` is hand-written and has to fill every
//! field. Nothing about the *types* then dictates which variant that default is:
//! both non-map variants derive `Default` themselves, so a hand-written
//! `impl Default for PaneContent` yielding a section pane compiles perfectly
//! well. Only `derive(Default)`'s `#[default]` attribute narrows it, and that is
//! a property of the macro rather than of anything in the data.
//!
//! It is `Map` because of what a default is *used for* here. Six sites
//! `std::mem::take` a `PaneState`, and a take leaves
//! `PaneContent::default()` sitting in `Gui::panes[idx]` for the rest of the UI
//! pass — where the all-panes filters that key off [`PaneState::is_map`] read it.
//! With a section pane as the default, every one of those filters would silently
//! *exclude* whichever pane is currently being drawn: no render dispatched for
//! it, no sibling texture offered to it, no error to say why. `Map` is the value
//! that makes the placeholder indistinguishable from the pane it stands in for,
//! for every consumer that has not been taught about kinds — which is all of them
//! today and most of them afterwards.
//!
//! [`PaneState::is_map`]: crate::pane::PaneState::is_map
//!
//! **The same choice is the sharpest hazard in the feature**, in the opposite
//! direction: during the UI pass `self.panes[idx]` genuinely reads as a map pane
//! whatever the real pane is. Nothing may branch on kind through
//! `self.panes[..]` or `active_pane()` while a pane is out; branch on the taken
//! value. The compiler cannot help with this, which is why the mitigation is the
//! `last_pane_content` probe: it records what each render arm actually drew, so a
//! branch reading the wrong thing shows up as an arm that ran for the wrong kind
//! rather than as a subtly wrong picture.

use chrono::NaiveDateTime;
use rustdar_radar::types::{RadarProduct, RenderView};
use serde::{Deserialize, Serialize};

/// Which of the three things a pane is.
///
/// Serialized into the UI config as the pane's `kind`, so the variant names are
/// part of the on-disk format. `Default` is `Map`, which is what makes a config
/// written before this existed load as a screen full of map panes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PaneKind {
    /// The plan-view radar map. The only kind that existed before, and the only
    /// one any shipped UI can currently produce.
    #[default]
    Map,
    /// A vertical slice through the volume along a line drawn on a map pane.
    CrossSection,
    /// A 3D view of the whole volume.
    Volume,
}

impl PaneKind {
    /// Whether a pane of this kind reads the *whole* volume rather than one
    /// tilt out of it.
    ///
    /// A plan view needs one sweep; a section and a volume render need every
    /// cut in the ladder, and handing either of them a scan whose cuts were
    /// deliberately skipped does not fail — it fabricates layers that are not
    /// there, quietly.
    ///
    /// This is the *view*-side half of that safety property. The data-side half
    /// is [`RadarProduct::reads_whole_volume`], which asks the same question of a
    /// product. Two questions, one answer: how much of the volume has to arrive.
    ///
    /// **Derived, not decided here.** The classification lives on
    /// [`RenderView::reads_whole_volume`] and this reads it through
    /// [`render_view`](Self::render_view), because a pane kind and the view its
    /// renders produce are the same fact under two names, and two exhaustive
    /// matches saying the same thing is two places for a fourth variant to be
    /// classified differently. The compile-time obligation is not lost: it
    /// simply moved to [`render_view`](Self::render_view), which is also
    /// exhaustive.
    pub fn consumes_whole_volume(self) -> bool {
        self.render_view().reads_whole_volume()
    }

    /// What a render dispatched for a pane of this kind produces.
    ///
    /// The single pane-kind → view table, and the only place the mapping lives.
    /// `rustdar_frontend` keys its render cache and its sibling-texture
    /// broadcast on the *view*, not on the pane kind: a cached raster outlives
    /// the pane that asked for it, and the thing that must not be handed to the
    /// wrong consumer is the buffer's shape.
    ///
    /// Exhaustive, matching `RadarProduct::wire_code`'s discipline: a fourth
    /// pane kind fails to compile until it has been classified here.
    /// `!matches!(self, Self::Map)` in the predicate above would have been
    /// shorter and would have classified a new kind as whole-volume on its own
    /// — the *safe* direction, since a too-wide download wastes bandwidth where
    /// a too-narrow one fabricates structure — but a kind that really did read
    /// one tilt would then quietly widen every download its pane triggers, with
    /// nothing to say so.
    pub fn render_view(self) -> RenderView {
        match self {
            // One sweep, chosen by `render::find_sweep` out of the product's own
            // moment. Everything else in the volume is irrelevant to it.
            Self::Map => RenderView::PlanView,
            // A section interpolates between the tilts bracketing each sample by
            // beam height, and a raymarch reads a grid resampled from every cut.
            // Both are vertical structure, which one sweep does not have.
            Self::CrossSection => RenderView::CrossSection,
            Self::Volume => RenderView::Volume,
        }
    }
}

/// The per-kind state a pane holds, and the sole source of its
/// [`PaneKind`](PaneKind).
///
/// See the module documentation for why this is one field on a pane whose other
/// fields stay flat, why the fat variants are boxed, and why `Default` is
/// `Map`.
#[derive(Debug, Default, PartialEq)]
pub enum PaneContent {
    /// A plan-view map. Carries nothing: everything a map pane needs is already
    /// a flat field on the pane.
    #[default]
    Map,
    CrossSection(Box<CrossSectionPane>),
    Volume(Box<VolumePane>),
}

impl PaneContent {
    /// Which kind this content *is*. The one place the mapping lives.
    pub fn kind(&self) -> PaneKind {
        match self {
            Self::Map => PaneKind::Map,
            Self::CrossSection(_) => PaneKind::CrossSection,
            Self::Volume(_) => PaneKind::Volume,
        }
    }

    /// Empty content of the given kind, as converting a pane produces.
    pub fn for_kind(kind: PaneKind) -> Self {
        match kind {
            PaneKind::Map => Self::Map,
            PaneKind::CrossSection => Self::CrossSection(Box::default()),
            PaneKind::Volume => Self::Volume(Box::default()),
        }
    }

    /// Drop every `egui::TextureHandle` this content holds, because the context
    /// that owns them is going away.
    ///
    /// Every arm is empty today — no per-kind state holds a texture yet — and
    /// this exists anyway, already called from `Gui::clear_graphics_state`. That
    /// is the only place a pane-held handle is released when the egui context
    /// dies (`app.rs`'s suspend path and `app_render.rs`'s surface-loss path both
    /// route through it), and a handle outliving its context is a leak that
    /// nothing reports: it is not a panic, not a blank pane, just memory that
    /// never comes back across a suspend/resume cycle.
    ///
    /// So the wiring is done first and the fields come later, rather than the
    /// other way round. The `match` is exhaustive and by value, so a fourth kind
    /// stops the build here — the same reasoning as `PaneLayout::for_count`'s
    /// clamp, which is that a trap someone has to *remember* at the moment they
    /// add a field is a trap that eventually catches someone. A doc comment on
    /// the field only fires if it is read; this fires either way.
    pub fn release_textures(&mut self) {
        match self {
            Self::Map => {}
            // `CrossSectionPane` will hold the rendered section raster.
            Self::CrossSection(_section) => {}
            // `VolumePane` will hold whatever the volume painter hands back.
            Self::Volume(_volume) => {}
        }
    }
}

/// A point on the ground, in degrees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GeoPoint {
    pub lat: f64,
    pub lon: f64,
}

impl GeoPoint {
    /// Whether this names a point that exists: latitude in `[-90, 90]`,
    /// longitude in `[-180, 180]`.
    ///
    /// Range rather than `is_finite`, and it subsumes it — NaN compares false
    /// against everything and the infinities fall outside the bounds — so one
    /// pair of comparisons rules out both a non-finite coordinate and a finite
    /// one that is nonsense. `lat: 1e9` is finite, walks a perfectly
    /// well-defined great circle, and describes nowhere.
    ///
    /// Not a restriction on where a line may be drawn: a section crossing the
    /// antimeridian is two in-range endpoints, and the great-circle walk between
    /// them handles the wrap. `walkers::Projector::unproject` already answers in
    /// this range, so an out-of-range point means something upstream is wrong
    /// rather than that the user drew somewhere unusual.
    pub fn is_on_earth(self) -> bool {
        (-90.0..=90.0).contains(&self.lat) && (-180.0..=180.0).contains(&self.lon)
    }
}

/// The line a cross-section is cut along, stored **geographically**.
///
/// # Why not screen coordinates
///
/// The user draws this line by dragging across a map pane, so screen positions
/// are what the interaction produces and storing them is the obvious thing.
/// It is also wrong twice over. A pixel pair denotes different ground after any
/// pan, zoom or window resize — including a wheel-zoom *during* the drag, since
/// the draw mode suppresses panning but not zooming — so the section would
/// silently re-cut itself somewhere else. And a pixel pair cannot be persisted:
/// restoring it into a session with a different window size or viewport would
/// place the line over unrelated ground with nothing to say so.
///
/// Geographic endpoints are converted from the pointer inside `Map::show`, on
/// the frame the press happens, where the projector is in hand. After that the
/// line means one thing forever.
///
/// The fields are private because [`Self::new`] is the only writer, and it is
/// what makes two properties true for everything downstream: the endpoints are
/// finite, and they are distinct.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SectionLine {
    a: GeoPoint,
    b: GeoPoint,
}

impl SectionLine {
    /// A section line from `a` to `b`, or `None` for a line that cannot be cut.
    ///
    /// Two refusals, and each one closes a distinct silent failure:
    ///
    /// * **Endpoints that are not points on Earth** ([`GeoPoint::is_on_earth`]).
    ///   They arrive from a projector fed a degenerate viewport, or from a
    ///   config file. Rejecting rather than clamping is the rule throughout this
    ///   crate for the same reason it is on `StormMotionOverride::sample`:
    ///   `f32::clamp` and `f64::clamp` *propagate* NaN, so a clamp launders a bad
    ///   value into a bad value that looks checked. Worse, a NaN endpoint reaches
    ///   [`SectionTarget`], where `NaN != NaN` makes the staleness key never
    ///   match itself — so the pane re-renders its section on every frame,
    ///   forever, with no error anywhere. A finite-but-absurd endpoint is quieter
    ///   still: `lat: 1e9` walks a well-defined great circle over nowhere and the
    ///   section renders as empty coverage, which is indistinguishable from a
    ///   line drawn past the radar's range.
    /// * **Coincident endpoints.** A zero-length line has no bearing, so the
    ///   great-circle walk along it is `0/0` and every column of the raster
    ///   samples the same point. This is the arithmetic bar; the usability bar
    ///   (a drag shorter than a couple of dozen points is a mis-click, not a
    ///   line) belongs to the interaction that produces the drag.
    pub fn new(a: GeoPoint, b: GeoPoint) -> Option<Self> {
        if !a.is_on_earth() || !b.is_on_earth() {
            return None;
        }
        if a == b {
            return None;
        }
        Some(Self { a, b })
    }

    /// The end the section's raster starts at (its left-hand column).
    pub fn a(self) -> GeoPoint {
        self.a
    }

    /// The end the section's raster finishes at (its right-hand column).
    pub fn b(self) -> GeoPoint {
        self.b
    }
}

/// Which volume a rendered section or voxel grid was built from.
///
/// The site is here for the same reason it is on
/// [`RenderTarget`](crate::pane::RenderTarget): the geometry is projected around
/// a site's coordinates, so the same volume time at another site is a different
/// picture. Two sites' volume times colliding to the second is unlikely, not
/// impossible.
#[derive(Clone, Debug, PartialEq)]
pub struct VolumeStamp {
    /// NEXRAD site code the volume belongs to (e.g. "KTLX").
    pub site: String,
    /// When the radar collected the volume (UTC).
    pub collected: NaiveDateTime,
}

/// Everything a rendered cross-section depends on, so that "is what is on
/// screen still the truth?" is one comparison.
///
/// The volume time is the part that makes this work without help. A section is
/// cut from a specific volume, so a new volume for the site makes the image on
/// screen stale by definition — and because the time is *in* the key, that is
/// noticed by the same comparison that notices a moved endpoint. No
/// `reset_panes_for_*` arm has to remember to invalidate section panes, which is
/// exactly the kind of thing that gets remembered for one of the two reset paths
/// and not the other.
///
/// `PartialEq` is derived, floats and all, and that is deliberate: this compares
/// a stored key against a stored key, never against a re-derived value, so
/// bitwise equality is the right test rather than an approximation of one. It is
/// only safe because [`SectionLine::new`] refuses non-finite endpoints — with a
/// NaN in there the key would never equal itself and the section would re-render
/// every frame for the life of the pane.
#[derive(Clone, Debug, PartialEq)]
pub struct SectionTarget {
    pub volume: VolumeStamp,
    /// The moment the section was cut from. Not every product is samplable —
    /// column integrals and the hybrid-scan composite have no vertical
    /// structure to slice — so this is narrower than the pane's product picker.
    pub product: RadarProduct,
    pub line: SectionLine,
}

/// A pane showing a vertical cross-section.
///
/// Minimal on purpose: nothing populates it yet. The two fields are the ones
/// whose *shape* is load-bearing — see [`SectionLine`] for why the endpoints are
/// geographic and [`SectionTarget`] for why the staleness key carries the volume
/// time. The rendered raster itself lands with the render path that produces it,
/// along with its release in `Gui::clear_graphics_state` — the only place a
/// pane-held `egui::TextureHandle` is dropped when the egui context dies, and
/// therefore the place a new texture-owning field has to be added in the same
/// commit that adds the field.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CrossSectionPane {
    /// The line to cut along, or `None` until the user has drawn one. A section
    /// pane with no line is an ordinary, expected state: it is what a pane looks
    /// like between being converted and being aimed.
    pub line: Option<SectionLine>,
    /// Which map pane the line was drawn on, or `None` for a section that has
    /// never been aimed.
    ///
    /// Persisted, and validated against the pane count on load: an index past the
    /// end of the layout is how a config saved from a wider split comes back on a
    /// narrower one, and a stale index would name a pane that is now something
    /// else entirely.
    ///
    /// Nothing sets it yet. It is here because it is the retarget rule's input —
    /// a second line drawn on the same map should re-aim the section already
    /// sourced from it rather than convert another pane — and a section restored
    /// from a config without it would be retargeted as though it had come from
    /// nowhere.
    pub source_pane: Option<usize>,
    /// What the section currently on screen was rendered for, or `None` before
    /// the first render. Compared against the current volume and line to decide
    /// whether to render again.
    pub rendered_for: Option<SectionTarget>,
}

/// Everything a built voxel grid depends on.
///
/// The same argument as [`SectionTarget`]: which volume, which moment, and —
/// since a region can be picked — over what ground. The region is in here for
/// exactly the reason the line is in `SectionTarget`. It is an input to the
/// resample, so a grid built for one box is the wrong picture for another, and
/// putting it in the key means the same comparison that notices a new volume
/// notices a re-dragged box. Left out, `rendered_for` would still match after a
/// region change, no rebuild would be asked for, and the store's `lookup` would
/// hand back the old box's grid — a picture that is wrong and looks right.
///
/// The camera is deliberately *not* in here — orbiting re-draws from the grid
/// already in hand and must not rebuild it. That is the line between the two
/// halves of this feature: the region changes what is *sampled*, the camera only
/// how it is *drawn*.
///
/// `None` for the region means the pane's default box about its site, and it is
/// a distinct key from any picked region — which is right, because it denotes a
/// different box and follows the site rather than the ground.
///
/// `PartialEq` is derived, `f64`s and all, on the same reasoning `SectionTarget`
/// gives: this compares a stored key against a stored key, and it is only safe
/// because [`VolumeRegion::new`] refuses a non-finite centre and clamps the
/// half-width to a finite range. With a NaN in there the key would never equal
/// itself and the pane would rebuild an 8 MiB grid every frame forever, with a
/// hot CPU as its only symptom.
#[derive(Clone, Debug, PartialEq)]
pub struct VolumeTarget {
    pub volume: VolumeStamp,
    pub product: RadarProduct,
    /// The ground to resample, or `None` for the default box about the site.
    pub region: Option<VolumeRegion>,
}

/// The patch of ground a 3D pane resamples, stored **geographically**.
///
/// # Why geographic, and why a square
///
/// The same argument [`SectionLine`] makes: the user picks this by dragging on a
/// map, so a pixel rect is what the interaction produces — and a pixel rect
/// denotes different ground after a wheel zoom, cannot be persisted across a
/// window resize, and would silently re-aim the box if the map were panned.
/// Converted to a centre and a half-width on the press frame, it means one thing
/// forever.
///
/// A **square** because that is what [`VoxelRequest`] takes: one
/// `half_width_km` for both horizontal axes, over a grid whose cell counts are
/// fixed. A free rectangle would have to be either squared silently — which
/// reads as a bug the first time a user drags a wide box and gets a tall one —
/// or honoured with a non-uniform grid, which is a different resample. The
/// interaction draws the square from the first frame of the drag so that the
/// shape is never a surprise.
///
/// # The half-width is a resolution control, not just a crop
///
/// The grid has a fixed cell count, so shrinking the box buys detail rather than
/// saving memory: at 256 cells across, an 80 km half-width is 0.625 km per cell
/// and a 20 km half-width is 0.156 km. That is the main reason to pick a region
/// at all, so [`Self::resolution_km`] exists to be *shown* rather than inferred.
///
/// Fields are private because [`Self::new`] is the only writer, and it is what
/// makes two things true downstream: the centre is a point on Earth, and the
/// half-width is inside the range `build_voxels` will honour. The second matters
/// more than it looks — `build_voxels` *clamps* the half-width rather than
/// refusing it, so a region carrying 5 km would resample 10 km and the pane's
/// own resolution readout would be a lie about the picture beside it.
///
/// [`VoxelRequest`]: rustdar_radar::voxel::VoxelRequest
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VolumeRegion {
    centre: GeoPoint,
    half_width_km: f64,
}

/// The half-width a pane starts with, kilometres.
///
/// 80 km, and the reasoning is `handle_prepare_volume`'s: it is the point where
/// the resolution gain is real (0.63 km per cell against 1.80 at the 230 km
/// limit) but the box is not yet so tight that a storm walks out of it between
/// volumes. It is also what the pane used before a region could be picked, so an
/// existing user's view does not move the day the control appears.
pub const DEFAULT_HALF_WIDTH_KM: f64 = 80.0;

impl VolumeRegion {
    /// A region centred on `centre` with `half_width_km` either side, or `None`
    /// if the centre is not a point on Earth.
    ///
    /// The half-width is **clamped** where the centre is **refused**, and the
    /// asymmetry is the same one [`OrbitCamera::restore`] draws. A centre that is
    /// NaN or off-Earth means the projector was fed a degenerate viewport or a
    /// config file was hand-edited: there is no nearest sensible answer, and
    /// clamping would launder it, because `f64::clamp` propagates NaN. A
    /// half-width past the end of its range is a zoom control that has been wound
    /// to its stop, and stopping is what a control should do.
    ///
    /// The clamp is against `build_voxels`' own bounds rather than a copy of
    /// them, so the number this holds is the number that will be resampled.
    pub fn new(centre: GeoPoint, half_width_km: f64) -> Option<Self> {
        if !centre.is_on_earth() || !half_width_km.is_finite() {
            return None;
        }
        Some(Self {
            centre,
            half_width_km: half_width_km.clamp(
                rustdar_radar::voxel::MIN_HALF_WIDTH_KM,
                rustdar_radar::voxel::MAX_HALF_WIDTH_KM,
            ),
        })
    }

    /// The region a pane falls back to: [`DEFAULT_HALF_WIDTH_KM`] about a site.
    ///
    /// Takes the centre rather than deriving it, because the pane does not know
    /// where its site is — `rustdar_radar::sites` is the frontend's lookup, and
    /// the one caller that has a site already has its coordinates.
    pub fn centred_on(centre: GeoPoint) -> Option<Self> {
        Self::new(centre, DEFAULT_HALF_WIDTH_KM)
    }

    /// Where the box is centred.
    pub fn centre(self) -> GeoPoint {
        self.centre
    }

    /// Half the box's east–west and north–south extent, kilometres.
    pub fn half_width_km(self) -> f64 {
        self.half_width_km
    }

    /// Kilometres per cell along a horizontal axis, for `cells` cells across.
    ///
    /// The number the pane shows, and the reason a tight region is worth
    /// picking. Answers `None` for a zero cell count rather than dividing by it.
    pub fn resolution_km(self, cells: usize) -> Option<f64> {
        (cells > 0).then(|| 2.0 * self.half_width_km / cells as f64)
    }
}

/// A pane showing a 3D view of the volume.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VolumePane {
    /// Where the eye is. See [`OrbitCamera`].
    pub camera: OrbitCamera,
    /// The patch of ground to resample, or `None` to use the default box about
    /// the site.
    ///
    /// `None` is an ordinary state, not a missing value: a 3D pane works before
    /// anyone picks a region, and the reset control puts it back here. Keeping it
    /// an `Option` rather than filling in a site-centred default at construction
    /// is what lets the pane follow its site when the site changes — a filled-in
    /// default would silently pin the box over the *old* site's ground, which
    /// looks exactly like a resample that went wrong.
    pub region: Option<VolumeRegion>,
    /// Which map pane the region was dragged on, or `None` for a region that was
    /// never picked.
    ///
    /// The retarget rule's input, and the same field `CrossSectionPane` carries
    /// for the same reason: a second region dragged on the same map should re-aim
    /// the 3D pane already sourced from it rather than convert another pane.
    /// Validated against the pane count on load, because an index past the end of
    /// the layout is how a config saved from a wider split comes back.
    pub source_pane: Option<usize>,
    /// Which volume the grid on screen was built from, or `None` before the
    /// first build.
    pub rendered_for: Option<VolumeTarget>,
}

/// A movement of the orbit camera: two angles and a zoom factor.
///
/// A struct rather than three `f32` parameters for the same reason
/// [`BroadcastSweep`](crate::pane::BroadcastSweep) is one: `yaw_deg` and
/// `pitch_deg` are the same type, adjacent, and both plausible in either
/// position, so a swap would compile and merely feel wrong to use.
///
/// `Default` is "the camera did not move", which is why it is hand-written:
/// `zoom_factor` is multiplicative, as every zoom input in this codebase is
/// (egui's `zoom_delta`, walkers' pinch), so its neutral value is 1.0 and a
/// derived `Default` would collapse the camera onto the volume instead.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OrbitDelta {
    /// Rotation about the vertical axis, degrees. Positive is counter-clockwise
    /// seen from above.
    pub yaw_deg: f32,
    /// Change in elevation above the horizontal, degrees. Positive raises the
    /// eye.
    pub pitch_deg: f32,
    /// Multiplicative zoom, in egui's own sense: a spreading pinch reports a
    /// factor above 1, which brings the eye *in*.
    pub zoom_factor: f32,
}

impl Default for OrbitDelta {
    fn default() -> Self {
        Self {
            yaw_deg: 0.0,
            pitch_deg: 0.0,
            zoom_factor: 1.0,
        }
    }
}

/// Pitch is held just inside vertical. At exactly ±90° the view direction is
/// parallel to the up vector, the camera basis is degenerate and the image rolls
/// arbitrarily as the last representable digit of yaw changes.
const MAX_PITCH_DEG: f32 = 89.0;
/// Eye distance is in multiples of the volume box's half-diagonal, so the camera
/// never has to know the grid's dimensions and the same limits hold for every
/// grid-spec rung. 1.0 is the eye on the box's corner sphere.
const MIN_EYE_DISTANCE: f32 = 1.05;
const MAX_EYE_DISTANCE: f32 = 8.0;

/// Where the eye is, for a 3D pane: an orbit about the centre of the volume.
///
/// # One writer, and it refuses rather than clamps
///
/// The fields are private and [`Self::nudge`] is the only way to move the
/// camera. It rejects a non-finite [`OrbitDelta`] outright instead of clamping
/// it into range, and the distinction is not stylistic:
///
/// * `f32::clamp` **propagates NaN** — `f32::NAN.clamp(0.0, 1.0)` is NaN — so a
///   clamp on the way in would launder a bad delta into a bad camera that looks
///   as though it had been checked. `rem_euclid`, which wraps the yaw, does the
///   same.
/// * A NaN camera is not merely a wrong picture. `NaN != NaN`, so the frame
///   comparison that decides whether the view needs re-rendering fires on every
///   single frame from then on, for the life of the pane, and the only symptom
///   is a hot GPU. There is no error and nothing to look at.
///
/// A delta arrives from a pointer, a pinch or a wheel, and those can be
/// non-finite: `zoom_delta` is a ratio, and a zero or degenerate gesture span
/// divides by zero. So the boundary is here, and the only thing past it is
/// arithmetic on finite numbers.
///
/// The matrices built from this camera live in `volume_view.rs` with the rest of
/// the projection math; this is the state half, and it lives with the pane state
/// because that is what is persisted and `mem::take`n.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OrbitCamera {
    /// Azimuth about the vertical axis, degrees in `[0, 360)`.
    yaw_deg: f32,
    /// Elevation above the horizontal, degrees in `[-MAX_PITCH_DEG,
    /// MAX_PITCH_DEG]`.
    pitch_deg: f32,
    /// Eye distance in multiples of the volume box's half-diagonal.
    eye_distance: f32,
}

impl Default for OrbitCamera {
    /// Looking north-ish from above the south-west, a little way out: an angle
    /// that shows a storm has height and depth at once, rather than the plan
    /// view the user already has on another pane.
    fn default() -> Self {
        Self {
            yaw_deg: 225.0,
            pitch_deg: 25.0,
            eye_distance: 2.5,
        }
    }
}

impl OrbitCamera {
    /// Move the camera by `delta`, or leave it exactly as it is.
    ///
    /// The whole delta is refused if any part of it is unusable — a non-finite
    /// angle, or a `zoom_factor` that is not finite and positive. Partial
    /// application is deliberately not offered: a gesture that produced one bad
    /// number produced it from the same pointer state as the others, so honoring
    /// the rest of it is honoring half a garbled input.
    ///
    /// See the type documentation for why this refuses rather than clamps.
    pub fn nudge(&mut self, delta: OrbitDelta) {
        if !delta.yaw_deg.is_finite() || !delta.pitch_deg.is_finite() {
            return;
        }
        if !delta.zoom_factor.is_finite() || delta.zoom_factor <= 0.0 {
            return;
        }

        // Only now, with every input known finite, are wrapping and clamping
        // safe: both would otherwise carry a NaN straight through.
        self.yaw_deg = (self.yaw_deg + delta.yaw_deg).rem_euclid(360.0);
        self.pitch_deg = (self.pitch_deg + delta.pitch_deg).clamp(-MAX_PITCH_DEG, MAX_PITCH_DEG);
        self.eye_distance =
            (self.eye_distance / delta.zoom_factor).clamp(MIN_EYE_DISTANCE, MAX_EYE_DISTANCE);
    }

    /// Rebuild a camera from persisted angles, or `None` if they are unusable.
    ///
    /// The second and last constructor, and the counterpart to the three
    /// accessors below — which is what keeps the fields private while still
    /// letting a camera survive a save and load.
    ///
    /// Refuses non-finite values rather than clamping them, for the reason the
    /// type documentation gives at length: `f32::clamp` and `rem_euclid` both
    /// *propagate* NaN, so a clamp on the way in launders a bad number into a bad
    /// camera that looks as though it had been checked — and a NaN camera is not
    /// a wrong picture but a re-render comparison that fires on every frame for
    /// the life of the pane, with a hot GPU as its only symptom.
    ///
    /// Finite-but-out-of-range values are wrapped and clamped instead of refused,
    /// through the same two expressions [`Self::nudge`] uses so the invariants
    /// keep one description. Only a hand-edited or version-skewed config can
    /// produce one, and `ui_config`'s `restore_viewport` reasons the same way
    /// about a saved zoom: there is nothing to propagate, and the nearest legal
    /// camera is a better answer than discarding the pane's kind over a number.
    pub fn restore(yaw_deg: f32, pitch_deg: f32, eye_distance: f32) -> Option<Self> {
        if !yaw_deg.is_finite() || !pitch_deg.is_finite() || !eye_distance.is_finite() {
            return None;
        }
        Some(Self {
            yaw_deg: yaw_deg.rem_euclid(360.0),
            pitch_deg: pitch_deg.clamp(-MAX_PITCH_DEG, MAX_PITCH_DEG),
            eye_distance: eye_distance.clamp(MIN_EYE_DISTANCE, MAX_EYE_DISTANCE),
        })
    }

    /// Azimuth about the vertical axis, degrees in `[0, 360)`.
    pub fn yaw_deg(self) -> f32 {
        self.yaw_deg
    }

    /// Elevation above the horizontal, degrees, never quite ±90.
    pub fn pitch_deg(self) -> f32 {
        self.pitch_deg
    }

    /// Eye distance in multiples of the volume box's half-diagonal.
    pub fn eye_distance(self) -> f32 {
        self.eye_distance
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(lat: f64, lon: f64) -> GeoPoint {
        GeoPoint { lat, lon }
    }

    /// The kind is derived from the content, so the two cannot disagree — which
    /// is the entire reason it is a method.
    #[test]
    fn every_content_variant_reports_its_own_kind() {
        assert_eq!(PaneContent::Map.kind(), PaneKind::Map);
        for kind in [PaneKind::Map, PaneKind::CrossSection, PaneKind::Volume] {
            assert_eq!(PaneContent::for_kind(kind).kind(), kind);
        }
    }

    /// `Default` is `Map` — a choice, not something the types force: both other
    /// variants derive `Default` too, so only `derive(Default)`'s `#[default]`
    /// attribute picks this one, and a hand-written impl yielding a section pane
    /// would compile.
    ///
    /// Pinned because of what the value is *for*. Six `mem::take` sites leave it
    /// in `Gui::panes[idx]` for the rest of the UI pass, and the all-panes
    /// filters that key off `PaneState::is_map` read that slot — so a default
    /// section pane would make every one of them silently skip whichever pane is
    /// being drawn, with no error to say why.
    #[test]
    fn the_default_content_is_a_map() {
        assert_eq!(PaneContent::default().kind(), PaneKind::Map);
        assert_eq!(PaneKind::default(), PaneKind::Map);
    }

    /// A plan view reads one sweep; the other two read the whole ladder, and
    /// giving either of them a volume with cuts deliberately skipped fabricates
    /// layers rather than failing.
    #[test]
    fn only_a_map_pane_is_content_with_one_tilt() {
        assert!(!PaneKind::Map.consumes_whole_volume());
        assert!(PaneKind::CrossSection.consumes_whole_volume());
        assert!(PaneKind::Volume.consumes_whole_volume());
        // And the predicate is the view's, not a second copy of it: every kind
        // agrees with the view it maps to, so the two names cannot come to
        // give different answers.
        for kind in [PaneKind::Map, PaneKind::CrossSection, PaneKind::Volume] {
            assert_eq!(
                kind.consumes_whole_volume(),
                kind.render_view().reads_whole_volume(),
                "{kind:?} answers the whole-volume question twice, differently",
            );
        }
    }

    /// A line that cannot be cut is not representable. Every refusal matters:
    /// a NaN endpoint would make [`SectionTarget`] never equal itself and
    /// re-render the pane on every frame forever, a finite-but-absurd one would
    /// render as empty coverage that looks like an out-of-range line, and a
    /// zero-length line has no bearing to walk along.
    #[test]
    fn a_section_line_refuses_endpoints_it_cannot_be_cut_along() {
        assert!(SectionLine::new(point(35.3, -97.3), point(35.6, -97.0)).is_some());

        // Non-finite, and finite-but-nowhere. The second group is the one a
        // bare `is_finite` guard let through.
        for bad_lat in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1e9, 90.001] {
            assert!(
                SectionLine::new(point(bad_lat, -97.3), point(35.6, -97.0)).is_none(),
                "{bad_lat} latitude accepted"
            );
        }
        for bad_lon in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1e9, 180.001] {
            assert!(
                SectionLine::new(point(35.3, -97.3), point(35.6, bad_lon)).is_none(),
                "{bad_lon} longitude accepted"
            );
        }

        // The bounds are inclusive: a pole and the antimeridian are places.
        assert!(SectionLine::new(point(90.0, 180.0), point(-90.0, -180.0)).is_some());

        assert!(
            SectionLine::new(point(35.3, -97.3), point(35.3, -97.3)).is_none(),
            "a zero-length line has no bearing: every column would sample one point"
        );
    }

    /// `release_textures` is total over the kinds, and callable on each.
    ///
    /// Every arm is empty today; the point is that the call site in
    /// `Gui::clear_graphics_state` is already wired, so the field that needs
    /// releasing lands inside a function that is already called on every
    /// suspend and every surface loss. A `match` with no wildcard is what makes
    /// a fourth kind stop the build rather than leak quietly.
    #[test]
    fn releasing_textures_is_total_over_the_kinds() {
        for kind in [PaneKind::Map, PaneKind::CrossSection, PaneKind::Volume] {
            let mut content = PaneContent::for_kind(kind);
            content.release_textures();
            assert_eq!(
                content.kind(),
                kind,
                "releasing a pane's textures must not change what kind it is"
            );
        }
    }

    /// The staleness key notices a new volume with no help from any reset path,
    /// because the volume time is in the key.
    #[test]
    fn a_section_target_goes_stale_when_the_volume_does() {
        let line = SectionLine::new(point(35.3, -97.3), point(35.6, -97.0)).expect("valid line");
        let at = |minute: u32| {
            chrono::NaiveDate::from_ymd_opt(2026, 7, 29)
                .unwrap()
                .and_hms_opt(18, minute, 0)
                .unwrap()
        };
        let target = |site: &str, minute: u32| SectionTarget {
            volume: VolumeStamp {
                site: site.to_owned(),
                collected: at(minute),
            },
            product: RadarProduct::Reflectivity,
            line,
        };

        assert_eq!(target("KTLX", 30), target("KTLX", 30));
        assert_ne!(
            target("KTLX", 30),
            target("KTLX", 36),
            "a new volume for the site makes the section on screen stale"
        );
        assert_ne!(
            target("KTLX", 30),
            target("KOUN", 30),
            "the same volume time at another site is a different picture"
        );
    }

    /// The camera's one writer refuses a non-finite delta rather than clamping
    /// it. Clamping would carry the NaN through (`f32::clamp` propagates it),
    /// and a NaN camera makes the re-render comparison fire on every frame for
    /// the life of the pane, silently.
    #[test]
    fn a_non_finite_nudge_leaves_the_camera_exactly_where_it_was() {
        // The premise, stated so nobody "simplifies" the guard back into a clamp.
        assert!(f32::NAN.clamp(-89.0, 89.0).is_nan());

        let start = OrbitCamera::default();
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            for delta in [
                OrbitDelta {
                    yaw_deg: bad,
                    ..Default::default()
                },
                OrbitDelta {
                    pitch_deg: bad,
                    ..Default::default()
                },
                OrbitDelta {
                    zoom_factor: bad,
                    ..Default::default()
                },
            ] {
                let mut camera = start;
                camera.nudge(delta);
                assert_eq!(camera, start, "{delta:?} moved the camera");
                assert!(camera.yaw_deg().is_finite());
                assert!(camera.pitch_deg().is_finite());
                assert!(camera.eye_distance().is_finite());
            }
        }

        // A zero or negative zoom factor is refused for the same reason: it is
        // a ratio, and a degenerate gesture span produces one.
        for factor in [0.0, -1.0] {
            let mut camera = start;
            camera.nudge(OrbitDelta {
                zoom_factor: factor,
                ..Default::default()
            });
            assert_eq!(camera, start, "zoom factor {factor} moved the camera");
        }
    }

    /// A persisted camera comes back exactly, and a corrupt one comes back as
    /// nothing.
    ///
    /// `restore` is the only way a camera can be built from numbers off disk, so
    /// it is where a hand-edited or version-skewed config is stopped. The refusal
    /// half matters for the same reason [`OrbitCamera::nudge`]'s does: a NaN
    /// camera makes the re-render comparison fire every frame for ever, silently.
    #[test]
    fn a_restored_camera_is_the_one_that_was_saved_or_none_at_all() {
        let start = OrbitCamera::default();
        let round_tripped =
            OrbitCamera::restore(start.yaw_deg(), start.pitch_deg(), start.eye_distance())
                .expect("a camera's own values must restore");
        assert_eq!(round_tripped, start);

        // A camera that had been moved, so the round trip is not just the default
        // agreeing with itself.
        let mut moved = start;
        moved.nudge(OrbitDelta {
            yaw_deg: -47.5,
            pitch_deg: 12.25,
            zoom_factor: 1.5,
        });
        assert_ne!(moved, start, "precondition: the nudge must have moved it");
        assert_eq!(
            OrbitCamera::restore(moved.yaw_deg(), moved.pitch_deg(), moved.eye_distance()),
            Some(moved)
        );

        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(OrbitCamera::restore(bad, 25.0, 2.5), None, "yaw {bad}");
            assert_eq!(OrbitCamera::restore(225.0, bad, 2.5), None, "pitch {bad}");
            assert_eq!(
                OrbitCamera::restore(225.0, 25.0, bad),
                None,
                "distance {bad}"
            );
        }

        // Finite but out of range: wrapped and clamped rather than refused, and
        // through the same expressions `nudge` uses — so a restored camera cannot
        // hold a value `nudge` would never produce.
        let stretched = OrbitCamera::restore(-30.0, 1_000.0, 0.001).expect("finite, so restorable");
        assert_eq!(stretched.yaw_deg(), 330.0);
        assert_eq!(stretched.pitch_deg(), MAX_PITCH_DEG);
        assert_eq!(stretched.eye_distance(), MIN_EYE_DISTANCE);
    }

    /// A finite nudge does move it, and lands inside the limits — so the test
    /// above is about the refusal rather than about a camera that never moves.
    #[test]
    fn a_finite_nudge_moves_the_camera_and_stays_in_range() {
        let mut camera = OrbitCamera::default();
        camera.nudge(OrbitDelta {
            yaw_deg: 30.0,
            pitch_deg: 10.0,
            zoom_factor: 2.0,
        });
        assert_eq!(camera.yaw_deg(), 255.0);
        assert_eq!(camera.pitch_deg(), 35.0);
        assert!(camera.eye_distance() < OrbitCamera::default().eye_distance());

        // Yaw wraps rather than clamping — a camera that stuck at 360 could not
        // be spun all the way round.
        camera.nudge(OrbitDelta {
            yaw_deg: 200.0,
            ..Default::default()
        });
        assert_eq!(camera.yaw_deg(), 95.0);

        // Pitch and distance clamp, and stop just short of vertical: at exactly
        // ±90 the camera basis is degenerate and the image rolls arbitrarily.
        camera.nudge(OrbitDelta {
            pitch_deg: 1_000.0,
            zoom_factor: 0.000_01,
            ..Default::default()
        });
        assert_eq!(camera.pitch_deg(), MAX_PITCH_DEG);
        assert_eq!(camera.eye_distance(), MAX_EYE_DISTANCE);

        camera.nudge(OrbitDelta {
            pitch_deg: -10_000.0,
            zoom_factor: 100_000.0,
            ..Default::default()
        });
        assert_eq!(camera.pitch_deg(), -MAX_PITCH_DEG);
        assert_eq!(camera.eye_distance(), MIN_EYE_DISTANCE);
    }
}
