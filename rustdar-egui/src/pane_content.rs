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
//! (`ui_map.rs`, `ui_shell.rs`, and four in `ui.rs`) — so its size is on the
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
use rustdar_radar::xsect::CrossSection;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

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

    /// Whether a pane of this kind can animate a sequence of past volumes.
    ///
    /// A loop is a sequence of *rendered pictures*, one per volume, held as
    /// textures — so the question is not "does this kind draw radar" but "can
    /// one volume's worth of this kind be reduced to a picture that stays
    /// correct while it sits in a list". Two of the three can:
    ///
    /// * A plan view is an `IMAGE_SIZE²` raster of one tilt, positioned by the
    ///   site's coordinates. Nothing about the pane changes what it depicts.
    /// * A cross-section is a `SECTION_WIDTH × SECTION_HEIGHT` raster of one
    ///   line through one volume. The line is part of the loop's identity
    ///   (`crate::pane::SectionLoopKey`); moving it re-cuts every frame, exactly
    ///   as moving the product does for a plan view.
    /// * A **3D volume** can too, and its frame is the one that is not a
    ///   picture. The picture is raymarched live from the eye every frame, so a
    ///   cached *image* would be specific to the camera and one orbit would
    ///   invalidate the whole loop at once. What it caches instead is the
    ///   **input**: each frame is a resident `VOLUME_GRID_CELLS` 3D texture and
    ///   the march swaps which one it samples, at a measured +0.01 ms (+2%) on
    ///   a discrete GPU and +0.31–0.78 ms (+3–4%) on a software rasteriser. So
    ///   orbiting a resident loop costs nothing and a frame's identity is a
    ///   [`VolumeTarget`] rather than a raster. Its grids are held in one
    ///   application-wide store, so the *set* is what the loop owns
    ///   (`crate::pane::VolumeLoopKey` is the rest of its key: the region and
    ///   the storm motion vector, for the reason `SectionLoopKey` carries the
    ///   line and the vector). See
    ///   `rustdar_frontend::constants::VOLUME_LOOP_TEXTURE_BUDGET_BYTES` for
    ///   what it costs and `MAX_LOOP_VOLUME_FRAMES` for how much history it
    ///   buys — 14 frames on desktop at the full grid, rather than 30 at a
    ///   coarser one that would halve the vertical axis a loop exists to watch.
    ///
    /// Exhaustive on purpose, like [`Self::render_view`]: a fourth kind must be
    /// classified here rather than defaulting into — or out of — the loop
    /// machinery. The direction matters, because the two mistakes are not
    /// symmetric. A kind wrongly excluded is a missing feature; a kind wrongly
    /// included is a pane whose frames nothing renders, which under Sync Layers
    /// holds **every other pane's** loop back for ever. That asymmetry is why
    /// `Volume` answered `false` until three things existed: a store a holder
    /// can own a *set* of grids in, a build path that accepts a volume time
    /// that is not the newest, and a pacing budget for the resample. All three
    /// do now, which is what changed the answer — the claim was never that the
    /// memory did not fit.
    pub fn can_loop(self) -> bool {
        match self {
            Self::Map | Self::CrossSection | Self::Volume => true,
        }
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
    /// Called from `Gui::clear_graphics_state`, which is the only place a
    /// pane-held handle is released when the egui context dies (`app.rs`'s
    /// suspend path and `app_render.rs`'s surface-loss path both route through
    /// it). A handle outliving its context is a leak that nothing reports: it is
    /// not a panic, not a blank pane, just memory that never comes back across a
    /// suspend/resume cycle.
    ///
    /// # Releasing is only half of a cycle
    ///
    /// **Every arm that drops a handle owes a path that puts one back**, and the
    /// owed path is a *restore*, not a re-render. Dropping a texture with nothing
    /// to re-upload it leaves a pane that is not blank and not broken — it is
    /// waiting, on a piece of work nobody will ever dispatch, because the
    /// staleness key that would trigger the dispatch is still satisfied. That
    /// state cost the section pane one review cycle: `texture: None` with
    /// `section: Some(..)` and a matching `rendered_for` paints "Cutting the
    /// cross-section…" forever, with the hover readout dead behind it.
    /// `App::restore_section_textures` is the other half, and its doc is where
    /// the argument for re-uploading over re-cutting lives.
    ///
    /// The `match` is exhaustive and by value, so a fourth kind stops the build
    /// here — the same reasoning as `PaneLayout::for_count`'s clamp, which is
    /// that a trap someone has to *remember* at the moment they add a field is a
    /// trap that eventually catches someone. A doc comment on the field only
    /// fires if it is read; this fires either way.
    pub fn release_textures(&mut self) {
        match self {
            Self::Map => {}
            // The section raster, and **only** the raster. The `CrossSection`
            // behind it is plain memory rather than a GPU handle and it is what
            // a hover reads, so it stays; `rendered_for` stays with it, because
            // together they are what lets `App::restore_section_textures`
            // re-upload the picture that was on the glass instead of walking a
            // 15.6 MB volume again for a volume that may have been evicted.
            // Clearing the key here is the tempting one-line alternative and it
            // is the expensive, fragile one.
            Self::CrossSection(section) => section.texture = None,
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
/// # Why the volume time is not enough on the live feed
///
/// [`VolumeStamp::collected`] comes from `ScanInfo::timestamp`, which is the
/// **first** sweep's first radial. On the archive path that is a fine key: the
/// volume arrives whole, so a new time is the only way it ever changes. On the
/// live chunk feed it is a *constant for five to six minutes* — the `Scan` grows
/// sweep by sweep with `sweeps[0]` fixed, so the tilt ladder goes from one rung
/// to fourteen without the stamp moving a millisecond. A section cut from the
/// first chunk therefore stood for the whole volume, showing a one-rung ladder
/// against a map pane full of echo, and only the tilt-curve refusal made it
/// visible at all.
///
/// [`ladder`](Self::ladder) is the missing input: the fingerprint of the tilt
/// ladder the cut would be made from — which sweep every rung takes, under
/// which declared pattern (`rustdar_radar::sampler::ladder_fingerprint`,
/// computed by the App over the merged current volume). It moves for every
/// kind of growth a section can show:
///
/// * **A new elevation**, which adds a rung to the ladder.
/// * **A SAILS repeat of an angle already in the ladder**, which does not add
///   a rung but does change which sweep that rung is *made of* — the sampler
///   chooses newest-first — and that rung is the lowest one, which is the part
///   of a severe-weather section most worth being current.
/// * **A sealed sweep replacing the base volume's copy of its cut** on the
///   merged substrate, which is the ordinary way every rung refreshes.
///
/// And — as load-bearing as the moving — it *holds still* when the picture
/// would not change. The key this replaces was a count of sweeps carrying the
/// moment, and a split cut's Doppler half carries a short-range reflectivity
/// copy: its seal moved the count while the surveillance preference kept every
/// chosen rung exactly where it was, so ~6 of the 18–23 re-cuts per VCP-212
/// volume produced byte-identical pictures. A fingerprint of the *choices*
/// cannot be moved by a seal that changes no choice.
///
/// The obvious alternative — the number of distinct elevation angles the UI
/// knows about, from `ScanInfo::product_elevations` — was tried before either
/// and is **wrong, for a reason that only shows up on the second volume of a
/// session**: `Gui::apply_chunk_scan_info` merges angles and never removes
/// one, so after the first complete volume the count is a constant for the
/// rest of the session. Verified live: it grew 1 → 2 → 3 on a cold start and
/// then sat at 16 for every volume after. The fingerprint is computed off the
/// same resolved volume the payload is extracted from, so the key and the
/// payload cannot describe different things.
///
/// It is deliberately **not** on [`VolumeStamp`], which [`VolumeTarget`] also
/// uses: the 3D pane keys its rebuilds on the published stamp's newest-data
/// time, and widening the shared stamp with a per-moment ladder key would
/// re-cut every product's section when any one moment's ladder moved.
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
    /// The fingerprint of the tilt ladder this cut would be made from, at
    /// dispatch. See the type's docs: this is what makes a live volume re-cut
    /// exactly when a rung's chosen sweep or the declared pattern changes,
    /// and not on the seals that change neither. `0` when no ladder resolves
    /// at all — its own honest value of the key.
    pub ladder: u64,
}

/// Why a section pane has no picture, when it has none.
///
/// Every variant is a state a user can reach without doing anything wrong, and
/// each one has a *different* thing to say. A single "no data" would collapse
/// them, and the collapse is the failure: the two that matter most —
/// [`AwaitingCoveragePattern`](Self::AwaitingCoveragePattern) and
/// [`ProductHasNoVerticalStructure`](Self::ProductHasNoVerticalStructure) — are
/// permanent-looking blanks whose causes are entirely unlike each other and
/// entirely unlike "the volume has not arrived".
///
/// `Ord` is not derived and not wanted: nothing ranks these. The pane holds at
/// most one, written by whoever refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectionUnavailable {
    /// No decoded volume for the pane's site yet — the ordinary startup and
    /// site-switch state.
    AwaitingVolume,
    /// The volume was joined **mid-flight** and its coverage pattern has not
    /// arrived, so it carries no elevation cut table.
    ///
    /// This is the live chunk feed's real behaviour, not a hypothetical:
    /// `chunks.rs` stands in `placeholder_coverage_pattern(0)` until the VCP
    /// message lands, and `VolumeSampler::new` correctly refuses a scan like
    /// that rather than inventing a tilt ladder out of the sweeps' own
    /// elevation numbers. Without a name of its own it reads as a section that
    /// silently does not work on live data.
    AwaitingCoveragePattern,
    /// The pane's product has no vertical structure to slice — the column
    /// integrals, the hybrid-scan composite, the derived velocity fields. See
    /// `rustdar_radar::sampler::samplable`.
    ProductHasNoVerticalStructure(RadarProduct),
    /// The cut was dispatched and answered nothing. Rare, and deliberately
    /// distinct from "not yet": a section that will never appear must not look
    /// like one that is on its way.
    RenderFailed,
    /// **This volume** carries nothing to cut under the pane's product: no
    /// sweep holds the moment, or the derivation refused it — above all
    /// storm-relative velocity with no motion vector from either the override
    /// or the volume's own winds.
    ///
    /// Not the same refusal as
    /// [`ProductHasNoVerticalStructure`](Self::ProductHasNoVerticalStructure),
    /// which is a property of the *product* and permanent. This one is a
    /// property of the volume and resolves when a volume carrying the moment
    /// arrives, which is why the staleness key it is written with carries the
    /// volume stamp.
    ///
    /// It exists because without it this state had no name and no message.
    /// The dispatcher's "no payload" answer was indistinguishable from "the
    /// render budget is full", so the pane wrote no staleness key, re-asked on
    /// every frame, and painted "Cutting the cross-section…" for as long as
    /// the volume stood — a permanent wait, which this codebase shipped once
    /// before and fixed, and which the pane's own doc calls the worst state a
    /// pane can be in.
    ProductMissingFromVolume(RadarProduct),
}

impl SectionUnavailable {
    /// One line, addressed to whoever is looking at the empty pane.
    ///
    /// Says what is missing and, where the user can do something, what. The
    /// mid-flight case is the one that most needs saying out loud — it is not
    /// an error, it resolves on its own, and it is invisible from anywhere else
    /// in the UI.
    pub fn message(self) -> String {
        match self {
            // The cold-start window: a site switch fires the archive fetch
            // immediately, so the first volume is already on its way — and
            // once any volume has landed, a section cuts instantly from the
            // merged current volume and this state is never seen again.
            Self::AwaitingVolume => {
                "Downloading this site's first volume - the section appears the moment it lands"
                    .to_owned()
            }
            Self::AwaitingCoveragePattern => {
                "This volume was joined mid-scan and its coverage pattern has not arrived yet, \
                 so there is no tilt ladder to cut along. It will appear on the next volume."
                    .to_owned()
            }
            Self::ProductHasNoVerticalStructure(product) => format!(
                "{} has no vertical structure to slice - pick a moment the radar measures \
                 tilt by tilt",
                product.name()
            ),
            Self::RenderFailed => "The cross-section could not be cut from this volume".to_owned(),
            Self::ProductMissingFromVolume(product) => format!(
                "This volume carries no {} to cut - the section appears as soon as one \
                 that does arrives. Storm-relative velocity also needs a motion vector, from \
                 the volume's own winds or the override.",
                product.name()
            ),
        }
    }
}

/// A pane showing a vertical cross-section.
///
/// The first three fields are the ones whose *shape* is load-bearing — see
/// [`SectionLine`] for why the endpoints are geographic and [`SectionTarget`]
/// for why the staleness key carries the volume time. The last three are what
/// the render path produces, and `texture` is released in
/// [`PaneContent::release_textures`] — the only place a pane-held
/// `egui::TextureHandle` is dropped when the egui context dies.
///
/// # Why `Debug` is hand-written
///
/// `egui::TextureHandle` has no `Debug`, and `CrossSection` has one that would
/// print megabytes. Both are summarised instead, which is also what makes this
/// type printable in an assertion message at all.
#[derive(Clone, Default, PartialEq)]
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
    /// The cut itself: the picture, the values a hover reads, and the status
    /// plane that says *why* a pixel is blank.
    ///
    /// `Arc` because the three planes are ~18 MB natively and this is read from
    /// a hover on every frame the pointer is over the pane; a clone per frame
    /// would be the most expensive thing in the UI pass.
    ///
    /// Kept when the texture is released, and `App::restore_section_textures`
    /// is what makes the keeping worth anything: a suspend/resume re-uploads
    /// this rather than re-cutting, because the volume behind the cut may have
    /// been evicted by then, which would make the re-cut impossible rather than
    /// merely slow.
    pub section: Option<Arc<CrossSection>>,
    /// The section's raster, uploaded. Dropped by
    /// [`PaneContent::release_textures`] and put back by
    /// `App::restore_section_textures` from
    /// [`section`](Self::section) — the two are a pair, and a release with no
    /// restore is a pane that waits forever.
    pub texture: Option<egui::TextureHandle>,
    /// Why there is no section, when there is none *and* a line has been drawn.
    ///
    /// `None` with no [`line`](Self::line) is the ordinary "not aimed yet"
    /// state, which is not a failure and has its own message. `None` with a
    /// line and no [`section`](Self::section) means a cut is in flight.
    pub unavailable: Option<SectionUnavailable>,
    /// Whether the caption's ⓘ detail — the long-form account of what the
    /// picture is and is not — is expanded.
    ///
    /// View state, not a claim about the data, so it is deliberately **not**
    /// persisted and **not** part of any staleness key: toggling it must never
    /// cost a re-cut. It lives on the pane rather than in egui memory so the
    /// renderer reads and writes it through the same struct everything else
    /// about the pane goes through — and so a test can drive it without
    /// reaching into a private id-keyed store.
    pub detail_open: bool,
}

impl std::fmt::Debug for CrossSectionPane {
    /// Summarised rather than dumped: `section` would print three
    /// multi-megabyte planes, and `texture` has no `Debug` at all.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CrossSectionPane")
            .field("line", &self.line)
            .field("source_pane", &self.source_pane)
            .field("rendered_for", &self.rendered_for)
            .field("section", &self.section.is_some())
            .field("texture", &self.texture.as_ref().map(|t| t.id()))
            .field("unavailable", &self.unavailable)
            .field("detail_open", &self.detail_open)
            .finish()
    }
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
/// The camera is deliberately *not* in here — orbiting, panning and exaggerating
/// all re-draw from the grid already in hand and must not rebuild it. That is the
/// line between the two halves of this feature: the region changes what is
/// *sampled*, the camera only how it is *drawn*.
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
/// **The resampler's own maximum, which is the full surveillance range** —
/// [`rustdar_radar::voxel::MAX_HALF_WIDTH_KM`] matches
/// `rustdar_radar::types::MAX_RANGE_KM`, the 230 km the plan view's raster is
/// drawn to. A pane with no picked region is answering "show me this site's
/// volume", and the earlier 80 km default answered with a crop: echo past
/// 80 km — most of a scan, on a squall-line day — was silently cut off before
/// the edge of the picture beside it, which read as a resample that went wrong
/// rather than as a choice.
///
/// The cost is resolution: 256 cells over 460 km is 1.80 km per cell against
/// 0.63 at the old default. That trade now belongs to the user — the region
/// drag exists precisely to spend the same cells over less ground, and the
/// caption prints the km-per-cell either way.
///
/// Written as the resampler's constant rather than a copy of 230 so that
/// [`VolumeRegion::new`] passes it through un-clamped: the caption,
/// [`VolumePane::box_size_km`] and the resample all describe the same box, and
/// if the resampler's ceiling ever moves, the sourceless default keeps covering
/// the whole scan by construction.
pub const DEFAULT_HALF_WIDTH_KM: f64 = rustdar_radar::voxel::MAX_HALF_WIDTH_KM;

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
    /// Whether this pane has turned the map floor **off**.
    ///
    /// Stored inverted so the derived `Default` — `false` — is the floor
    /// showing, which is the shipped default: the floor is the ground the 2D
    /// map gives the volume, and a pane that opens without it is a box
    /// hanging in the void. The inversion is contained here; everything
    /// downstream reads [`crate::volume_view::VolumeFrameState::floor`],
    /// which is the positive form.
    pub hide_floor: bool,
    /// Whether this pane's Volume Alpha editor window is open.
    ///
    /// Session state, not persisted: the *curves* the editor draws are the
    /// durable thing (per product, in the UI config); an open tool window is
    /// a posture, and restoring it over a pane whose volume has not built yet
    /// would be a window full of "waiting" on every launch. Default `false`
    /// keeps the derived `Default` honest.
    pub alpha_editor_open: bool,
    /// How this pane draws its volume: the lit accumulation or an isosurface.
    ///
    /// Persisted (a pane set to isosurface should come back one), unlike the
    /// camera-adjacent session state around it, because it changes *what kind
    /// of picture* the pane is, not merely how the current one is posed. The
    /// per-product thresholds live on `Gui`, beside the alpha curves and for
    /// the same reason: a threshold drawn for one product must never apply to
    /// another.
    pub view_mode: VolumeViewMode,
}

/// How a 3D pane draws its volume.
///
/// `Default` is the lit volume — today's render, and what every config from
/// before this enum existed loads as through `#[serde(default)]`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum VolumeViewMode {
    /// The alpha-accumulating raymarch: translucent cloud, shaped by the
    /// product's transparency profile and the user's Volume Alpha curve.
    #[default]
    LitVolume,
    /// The first crossing of a per-product threshold, drawn as one opaque,
    /// gradient-lit surface — GR2Analyst's other view mode. The threshold
    /// reads the data, never the alpha curve.
    Isosurface,
}

impl VolumePane {
    /// The box's full extent in kilometres along each axis, as the resample will
    /// produce it.
    ///
    /// # Why the pane derives this rather than reading it off the grid
    ///
    /// The grid is the truth and the painter reads it there. But the pane needs
    /// the box's proportions *before* the painter runs, on the frame a drag is
    /// folded in — the pan scale is a fraction of the box — and the grid lives
    /// behind the painter in another crate. Reading last frame's box would put
    /// the pan one frame behind the pointer, which is the exact defect
    /// `VolumePainter::paint`'s ordering exists to avoid.
    ///
    /// The two agree by construction rather than by luck: `build_voxels` spans
    /// `2 · half_width_km` horizontally and `base..top` vertically, from the same
    /// clamped half-width [`VolumeRegion::new`] holds and the same two constants
    /// used here. If they ever disagreed the symptom would be a pan that drifts
    /// against the picture, which is why they read one definition each.
    pub fn box_size_km(&self) -> [f32; 3] {
        let half_width = self
            .region
            .map_or(DEFAULT_HALF_WIDTH_KM, VolumeRegion::half_width_km);
        [
            (2.0 * half_width) as f32,
            (2.0 * half_width) as f32,
            (rustdar_radar::voxel::DEFAULT_TOP_KM_MSL - rustdar_radar::voxel::DEFAULT_BASE_KM_MSL)
                as f32,
        ]
    }
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
    /// Where to move the pivot, as a fraction of the box's half-extent on each
    /// axis. See [`OrbitCamera::pivot`].
    ///
    /// **Already resolved into world axes by the caller**, not a screen-space
    /// pair to be rotated here. The rotation needs the camera basis *and* the
    /// box's proportions *and* the viewport height, and only
    /// [`crate::volume_view::pan_for_drag`] has all three — so it does the whole
    /// conversion and this carries the answer. The alternative, a screen delta
    /// resolved here, would put a second copy of the camera basis in this module
    /// for the two to drift apart.
    pub pan: [f32; 3],
}

impl Default for OrbitDelta {
    fn default() -> Self {
        Self {
            yaw_deg: 0.0,
            pitch_deg: 0.0,
            zoom_factor: 1.0,
            pan: [0.0; 3],
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
///
/// # The minimum admits the inside of the box
///
/// 0.05 is well inside the corner sphere: at the default whole-scan box it puts
/// the eye a few kilometres from the pivot, which is inside-the-storm close —
/// the zoom GR2Analyst allows and the one a 1.05 floor was refusing. Inside is
/// a supported camera, not an accident: the raymarch clamps its slab entry to
/// zero (`max(near.z, 0.0)` in `slab_entry_exit`), so a ray from inside the box
/// marches forward from the eye rather than from behind it, and
/// `rustdar-frontend`'s silhouette harness renders from an inside eye to prove
/// the GPU agrees.
///
/// Not zero, and not merely to avoid a strange picture: at exactly 0 the eye
/// sits *on* the pivot, the orbit offset is the zero vector, and
/// `volume_view::build_view` finds no forward direction and refuses the frame —
/// a pane that goes blank at the end of the zoom's travel. 0.05 keeps the
/// direction defined with two orders of magnitude to spare.
const MIN_EYE_DISTANCE: f32 = 0.05;
const MAX_EYE_DISTANCE: f32 = 8.0;

/// How far the pivot may be pushed from the box's centre, as a fraction of the
/// box's half-extent on each axis.
///
/// **This is the clamp that stops the box being pushed off screen**, and 1.0 is
/// the value that makes the guarantee exactly rather than approximately: at 1.0
/// the pivot is on the box's own surface, so the point the camera is aimed at —
/// which is the point that lands in the middle of the pane — is always a point
/// of the box. Some of the box is therefore under the centre of the pane at
/// every pan, whatever the yaw, pitch or zoom.
///
/// Expressed per axis rather than as a radius, because the box is a pancake —
/// 25.6:1 at the whole-scan default: a spherical bound of one half-extent would either let the pivot leave
/// the box sideways or stop it well short of the top face.
const MAX_PIVOT_FRACTION: f32 = 1.0;

/// The vertical exaggeration a 3D pane starts at.
///
/// The default box is 460 km wide and 18 km tall — **25.6:1** — and at true
/// proportions it reads as a sheet of paper rather than as a volume with storms
/// standing in it. That is a real property of the atmosphere and the flat view is the honest
/// one, which is why the number is *shown* rather than hidden; but a view whose
/// whole claim is that it shows vertical structure has to make the vertical
/// structure visible, and 3 is where a supercell's overhang and a stratiform
/// sheet become distinguishable at a glance.
///
/// 3 rather than more because it is the bottom of the 3–8 range GR2Analyst-like
/// views are read at, and a default that starts at the bottom of a range is one
/// the user turns *up* on purpose rather than one that has silently
/// been making every storm look like a tower.
pub const DEFAULT_VERTICAL_EXAGGERATION: f32 = 3.0;
/// True proportions. The bottom of the control's travel is 1, never 0: a zero
/// would collapse the box to a plane, which divides by zero in `box_from_world`.
pub const MIN_VERTICAL_EXAGGERATION: f32 = 1.0;
/// Past about 12 the box is taller than it is wide and the orbit stops behaving
/// like an inspection of a storm — and the picture stops being a defensible
/// rendering of anything, because a 15 km updraught drawn 180 km tall is no
/// longer a shape a forecaster can read a height off.
pub const MAX_VERTICAL_EXAGGERATION: f32 = 12.0;

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
    /// The point the orbit turns about and the camera looks at, as a fraction of
    /// the box's half-extent on each axis, each component in
    /// `[-MAX_PIVOT_FRACTION, MAX_PIVOT_FRACTION]`.
    ///
    /// # Why a fraction of the box and not kilometres
    ///
    /// The box's size changes — a region drag re-cuts it from 460 km across to
    /// as little as 20 — and a pivot in kilometres would survive that change by
    /// pointing somewhere else, usually outside the new box. In box fractions the
    /// same stored value means the same *part* of the box whatever the box is,
    /// which is what a user who tightened a region and expected to still be
    /// looking at the storm they aimed at will read as correct. It is also what
    /// makes the clamp above a one-line guarantee rather than an argument about
    /// aspect ratios.
    ///
    /// It is measured against the **exaggerated** box on the vertical axis, so
    /// turning the exaggeration up does not slide the pivot off the top face.
    pivot: [f32; 3],
    /// How much the vertical axis is stretched when the box is drawn, in
    /// `[MIN_VERTICAL_EXAGGERATION, MAX_VERTICAL_EXAGGERATION]`.
    ///
    /// A property of the *camera* rather than of the grid, and that is the whole
    /// design: it changes nothing about what was sampled, so turning it is free
    /// and never triggers a rebuild. It is deliberately not in
    /// [`VolumeTarget`] for the same reason the yaw is not.
    ///
    /// **Everything the pane reports about height stays in real units.** The
    /// stretch is applied to the geometry and to nothing else; the pane's readout
    /// reads the grid's own `z_range_km_msl`, which this never touches. A view
    /// that quietly reported exaggerated heights would be worse than no
    /// exaggeration at all, because the number would look like a measurement.
    vertical_exaggeration: f32,
}

impl Default for OrbitCamera {
    /// Looking north-ish from above the south-west, a little way out, aimed at
    /// the box's centre and stretched by [`DEFAULT_VERTICAL_EXAGGERATION`]: an
    /// angle that shows a storm has height and depth at once, rather than the
    /// plan view the user already has on another pane.
    fn default() -> Self {
        Self {
            yaw_deg: 225.0,
            pitch_deg: 25.0,
            eye_distance: 2.5,
            pivot: [0.0; 3],
            vertical_exaggeration: DEFAULT_VERTICAL_EXAGGERATION,
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
        // Checked with the rest and refused with the rest: a pan arrives from
        // the same pointer state as the orbit, through a division by the
        // viewport height that a pane one frame wide makes infinite.
        if !delta.pan.iter().all(|p| p.is_finite()) {
            return;
        }

        // Only now, with every input known finite, are wrapping and clamping
        // safe: both would otherwise carry a NaN straight through.
        self.yaw_deg = (self.yaw_deg + delta.yaw_deg).rem_euclid(360.0);
        self.pitch_deg = (self.pitch_deg + delta.pitch_deg).clamp(-MAX_PITCH_DEG, MAX_PITCH_DEG);
        self.eye_distance =
            (self.eye_distance / delta.zoom_factor).clamp(MIN_EYE_DISTANCE, MAX_EYE_DISTANCE);
        for (axis, moved) in self.pivot.iter_mut().zip(delta.pan) {
            *axis = (*axis + moved).clamp(-MAX_PIVOT_FRACTION, MAX_PIVOT_FRACTION);
        }
    }

    /// Set the vertical exaggeration, or leave it exactly as it is.
    ///
    /// The one writer for the knob, and it refuses a non-finite value for the
    /// reason the type documentation gives: `f32::clamp` propagates NaN, and a
    /// NaN here would reach `box_from_world` as a divide-by-NaN and hand the GPU
    /// a matrix the driver renders as an empty pane with no error anywhere.
    ///
    /// Finite values are clamped rather than refused — this is a slider, and a
    /// slider that reaches the end of its travel should stop.
    pub fn set_vertical_exaggeration(&mut self, exaggeration: f32) {
        if !exaggeration.is_finite() {
            return;
        }
        self.vertical_exaggeration =
            exaggeration.clamp(MIN_VERTICAL_EXAGGERATION, MAX_VERTICAL_EXAGGERATION);
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
    pub fn restore(
        yaw_deg: f32,
        pitch_deg: f32,
        eye_distance: f32,
        pivot: [f32; 3],
        vertical_exaggeration: f32,
    ) -> Option<Self> {
        if !yaw_deg.is_finite() || !pitch_deg.is_finite() || !eye_distance.is_finite() {
            return None;
        }
        if !pivot.iter().all(|p| p.is_finite()) || !vertical_exaggeration.is_finite() {
            return None;
        }
        let mut pivot = pivot;
        for axis in &mut pivot {
            *axis = axis.clamp(-MAX_PIVOT_FRACTION, MAX_PIVOT_FRACTION);
        }
        Some(Self {
            yaw_deg: yaw_deg.rem_euclid(360.0),
            pitch_deg: pitch_deg.clamp(-MAX_PITCH_DEG, MAX_PITCH_DEG),
            eye_distance: eye_distance.clamp(MIN_EYE_DISTANCE, MAX_EYE_DISTANCE),
            pivot,
            vertical_exaggeration: vertical_exaggeration
                .clamp(MIN_VERTICAL_EXAGGERATION, MAX_VERTICAL_EXAGGERATION),
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

    /// The look-at point, as a fraction of the box's half-extent on each axis,
    /// each component within ±1 and so always a point of the box.
    pub fn pivot(self) -> [f32; 3] {
        self.pivot
    }

    /// How much the vertical axis is stretched when the box is drawn. Never
    /// applied to anything the pane *reports*; see the field.
    pub fn vertical_exaggeration(self) -> f32 {
        self.vertical_exaggeration
    }
}

#[path = "pane_content/tests.rs"]
#[cfg(test)]
mod tests;
