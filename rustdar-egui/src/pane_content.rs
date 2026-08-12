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

/// Which of the two things a pane is.
///
/// A *kind* is a different **place**: a map pane looks down at a patch of
/// ground, and a cross-section pane looks at a vertical slice along a line
/// drawn somewhere else. How a pane *draws* the place it is looking at is a
/// separate question, answered by [`MapRender`] — a 3D volume is the same
/// ground as the plan view beside it, seen from a different eye, so it is a
/// render mode of a map pane rather than a kind of its own.
///
/// Serialized into the UI config as the pane's `kind`, so the variant names are
/// part of the on-disk format. `Default` is `Map`, which is what makes a config
/// written before this existed load as a screen full of map panes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PaneKind {
    /// A patch of ground, drawn either as the plan view or as the 3D volume
    /// standing on it. See [`MapRender`].
    #[default]
    Map,
    /// A vertical slice through the volume along a line drawn on a map pane.
    CrossSection,
}

/// How a map pane draws the ground it is looking at.
///
/// The two modes are the same *place* — same site, same viewport, same product,
/// same moment — differing only in where the eye is. That is the whole reason
/// this is a mode rather than a pane kind: switching it must feel like turning
/// a picture over, not like losing the pane and opening another.
///
/// Serialized as the pane's `render` key, so the variant names are on-disk
/// format. `Default` is `Plan`, so a config written before this key existed —
/// or one whose `render` names a mode this build does not know — comes back as
/// the plan view, which is what every pane was.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MapRender {
    /// One sweep, drawn flat and looked down on. What every pane has always
    /// been.
    #[default]
    Plan,
    /// The whole volume over the same ground, raymarched from an orbit camera,
    /// standing on the pane's own map as its floor.
    Volume,
}

impl PaneKind {
    /// What a pane of this kind draws when it is not a map, or `None` because a
    /// map pane's view depends on its [`MapRender`] rather than on its kind.
    ///
    /// The kind alone stopped being able to answer when the 3D view became a
    /// mode: `Map` denotes two different render views. So the whole mapping
    /// lives on [`PaneContent::render_view`], which has the mode in hand, and
    /// this exists only for the callers that hold a kind and nothing else.
    ///
    /// Exhaustive, matching `RadarProduct::wire_code`'s discipline: a third
    /// pane kind fails to compile until it has been classified here.
    pub fn render_view(self) -> Option<RenderView> {
        match self {
            // Two answers, and the kind cannot choose between them.
            Self::Map => None,
            // A section interpolates between the tilts bracketing each sample by
            // beam height. That is vertical structure, which one sweep does not
            // have — and it is the same answer whatever else the pane is doing.
            Self::CrossSection => Some(RenderView::CrossSection),
        }
    }
}

impl MapRender {
    /// What a render dispatched for a map pane in this mode produces.
    ///
    /// Exhaustive for the reason [`PaneKind::render_view`] is: a third mode
    /// must be classified rather than defaulting into a view.
    pub fn render_view(self) -> RenderView {
        match self {
            // One sweep, chosen by `render::find_sweep` out of the product's own
            // moment. Everything else in the volume is irrelevant to it.
            Self::Plan => RenderView::PlanView,
            // A raymarch reads a grid resampled from every cut in the ladder.
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
#[derive(Debug, PartialEq)]
pub enum PaneContent {
    /// A patch of ground. Everything a map pane needs to say *where* it is
    /// looking is already a flat field on the pane; what is in here is how it
    /// draws that place and the state the 3D mode needs to.
    Map(Box<MapPane>),
    CrossSection(Box<CrossSectionPane>),
}

impl Default for PaneContent {
    /// A **plan-view** map, and the module doc is about why that matters far
    /// more than a default usually does: this is the value left in
    /// `Gui::panes[idx]` while a pane is `mem::take`n, so it is what every
    /// all-panes filter reads about the pane currently being drawn.
    ///
    /// Hand-written rather than derived because `Map` now carries state, and
    /// `#[default]` does not apply to a variant with a field. The choice it
    /// encodes is unchanged: the placeholder has to be indistinguishable from
    /// the most ordinary pane there is.
    fn default() -> Self {
        Self::Map(Box::default())
    }
}

impl PaneContent {
    /// Which kind this content *is*. The one place the mapping lives.
    pub fn kind(&self) -> PaneKind {
        match self {
            Self::Map(_) => PaneKind::Map,
            Self::CrossSection(_) => PaneKind::CrossSection,
        }
    }

    /// What a render dispatched for this pane produces.
    ///
    /// The single content → view table, and the only place the mapping lives.
    /// `rustdar_frontend` keys its render cache and its sibling-texture
    /// broadcast on the *view*, not on the pane kind: a cached raster outlives
    /// the pane that asked for it, and the thing that must not be handed to the
    /// wrong consumer is the buffer's shape.
    ///
    /// It reads the *content* rather than the kind because a map pane answers
    /// two different views depending on its [`MapRender`] — that is what a
    /// render mode is. Both halves stay exhaustive, so a third kind or a third
    /// mode fails to compile until it has been classified.
    pub fn render_view(&self) -> RenderView {
        match self {
            Self::Map(map) => map.render.render_view(),
            Self::CrossSection(_) => RenderView::CrossSection,
        }
    }

    /// Empty content of the given kind, as converting a pane produces.
    pub fn for_kind(kind: PaneKind) -> Self {
        match kind {
            PaneKind::Map => Self::Map(Box::default()),
            PaneKind::CrossSection => Self::CrossSection(Box::default()),
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
            // Neither mode of a map pane holds a handle here: the plan view's
            // raster is owned by the frontend's render cache, and the 3D mode's
            // grids live in the application-wide volume store.
            Self::Map(_) => {}
            // The section raster, and **only** the raster. The `CrossSection`
            // behind it is plain memory rather than a GPU handle and it is what
            // a hover reads, so it stays; `rendered_for` stays with it, because
            // together they are what lets `App::restore_section_textures`
            // re-upload the picture that was on the glass instead of walking a
            // 15.6 MB volume again for a volume that may have been evicted.
            // Clearing the key here is the tempting one-line alternative and it
            // is the expensive, fragile one.
            Self::CrossSection(section) => section.texture = None,
        }
    }
}

/// A map pane's own state: how it draws its ground, and what the 3D mode needs
/// in order to.
///
/// # Why the volume state is here rather than behind the mode
///
/// `render: MapRender` and `volume: VolumePane` as two fields makes
/// "`render == Plan` with a camera the user aimed" representable, and that is
/// the point rather than an oversight. Flipping a pane back to the plan view
/// and forward again must return the *same* 3D view — the camera, the floor
/// toggle, the isosurface threshold — because the two modes are two ways of
/// looking at one pane, and a mode switch that forgot half of the pane would
/// make them feel like two panes after all. State behind the enum variant
/// would be destroyed on every toggle.
///
/// The cost is that a plan-view pane carries a `VolumePane` it is not using.
/// It is a few floats behind the `Box` the whole content already sits in, and
/// `PaneContent` is `mem::take`n once per pane per frame — so what is on the
/// hot path is the pointer, not this.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MapPane {
    /// Plan view or 3D volume. See [`MapRender`].
    pub render: MapRender,
    /// State the 3D mode draws from, kept across a return to the plan view.
    pub volume: VolumePane,
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
    /// The coverage pattern **has** arrived — so there is a tilt ladder to cut
    /// along — but no sweep has been sealed onto it yet.
    ///
    /// The mid-flight join one step past
    /// [`AwaitingCoveragePattern`](Self::AwaitingCoveragePattern): the start
    /// chunk has landed and the antenna is still flying the first tilt, with no
    /// archive base yet to fill the ladder from. It lasts as long as one cut
    /// takes — up to about half a minute — and it is the ordinary state for a
    /// site the user has only just switched to on the live feed.
    ///
    /// It had no name, and what it fell through to was
    /// [`ProductMissingFromVolume`](Self::ProductMissingFromVolume): *"this
    /// volume carries no Reflectivity to cut"*. True, and about the wrong
    /// thing. The volume carries no anything; the product is not implicated;
    /// and changing moment — which is what that message invites — does nothing
    /// at all. See `app_render::section_source_refusal` for why a merge can
    /// resolve and still be empty.
    AwaitingFirstSweep,
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
            // One sentence, and deliberately: this is the state a user meets
            // seconds after switching to a live site, and it is nothing going
            // wrong. Naming what is being waited on (a tilt) and roughly how
            // long (under a minute) is everything a reader can act on; the
            // sibling above needs two sentences because a missing VCP message
            // is a genuinely odd thing to have to explain.
            Self::AwaitingFirstSweep => {
                "This volume has only just started - the section appears with its first \
                 completed tilt, within about half a minute."
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
/// The same argument as [`SectionTarget`]: which volume, which moment, and over
/// what ground. The region is in here for exactly the reason the line is in
/// `SectionTarget`. It is an input to the resample, so a grid built for one box
/// is the wrong picture for another, and putting it in the key means the same
/// comparison that notices a new volume notices a re-framed box. Left out,
/// `rendered_for` would still match after the pane was zoomed, no rebuild would
/// be asked for, and the store's `lookup` would hand back the old box's grid — a
/// picture that is wrong and looks right.
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
/// extent to a finite range. With a NaN in there the key would never equal
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
/// # It is the pane's own viewport, not a separate thing to aim
///
/// This is **derived**, every frame, as the ground the pane's own viewport is
/// showing — see [`crate::ui_region::region_for_viewport`]. It used to
/// be dragged out on some *other* map pane and stored, and that arrangement is
/// what produced the defect this replaced: the box and the floor under it were
/// sized by two independent things, so a borrowed viewport smaller than the box
/// left the floor transparent outside it, and a 3D view opened from a tab — with
/// no source map at all — had no floor whatsoever. Deriving the box *from* the
/// viewport makes the floor cover it by construction, which is a property rather
/// than a thing to keep in step.
///
/// Zooming the pane is therefore the only region control there is, and it is the
/// only one there ever needed to be: the drag existed to spend the grid's fixed
/// cells over less ground, and zooming does exactly that.
///
/// Still stored geographically rather than as a pixel rect, for the reason
/// [`SectionLine`] gives — the affine that produced it is gone by the time the
/// resampler runs, and a grid built for one box must be recognisably the wrong
/// key for another.
///
/// # Why the footprint is the viewport's rectangle
///
/// It was the largest **square** inscribed in the viewport, which meant
/// converting a 16:9 pane to 3D took ground away from it:
///
/// > I didn't realize the 3d viewer actually cut the viewport smaller, I hate
/// > that. A user doesn't expect to become more boxed in when they zoom, they
/// > just expect to be closer with the same area available to them.
///
/// The floor strip always covered the whole pane rect; the box merely stood on
/// the middle of it, so the pane went on *showing* its left and right flanks
/// while nothing resampled them. Two extents make the box the ground the pane
/// is showing, which is what the pane already looked like it was promising.
///
/// The one real cost is that the cells are rectangular — a fixed count per axis
/// over unequal ground, so a 16:9 box's east–west cell is 1.78× its
/// north–south one. That is a measured figure and its consequences are measured
/// too; see [`Self::resolution_km`] and
/// [`rustdar_radar::voxel::VoxelShape`]'s anisotropy note.
///
/// [`HalfExtentKm`]: rustdar_radar::voxel::HalfExtentKm
///
/// # The extent is a resolution control, not just a crop
///
/// The grid has a fixed cell count, so a tighter box buys detail rather than
/// saving memory: at 256 cells across, an 80 km half-extent is 0.625 km per
/// cell and a 20 km half-extent is 0.156 km. That is what zooming a 3D pane is
/// *for*, so [`Self::resolution_km`] exists to be *shown* rather than inferred.
///
/// Fields are private because [`Self::new`] is the only writer, and it is what
/// makes two things true downstream: the centre is a point on Earth, and the
/// extent is inside the range `build_voxels` will honour. The second matters
/// more than it looks — `build_voxels` *clamps* the extent rather than refusing
/// it, so a region carrying 5 km would resample 10 km and the pane's own
/// resolution readout would be a lie about the picture beside it.
///
/// [`VoxelRequest`]: rustdar_radar::voxel::VoxelRequest
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VolumeRegion {
    centre: GeoPoint,
    half: rustdar_radar::voxel::HalfExtentKm,
}

/// The half-width a pane falls back to when nothing has sized its box yet,
/// kilometres.
///
/// **Not the box a drawn pane ends up with.** That box is the pane's own
/// viewport ([`crate::ui_region::region_for_viewport`]), bounded above by what
/// the volume actually reaches: [`rustdar_radar::voxel::MAX_HALF_WIDTH_KM`] is
/// now `types::MAX_EXTENT_KM / √2` — the largest square inscribed in the
/// furthest circle a plan view will project — and `build_voxels` answers an
/// unstated width with [`rustdar_radar::voxel::box_half_width_km`] of the
/// volume's own reach, 325.4 km of half-width for a WSR-88D's 460 km
/// reflectivity and 212.1 for its 300 km Doppler moments. Only the resampler,
/// which has the volume, can say which; this crate never sees one.
///
/// This is what stands in until one of those two has spoken: a pane not yet
/// drawn in the 3D mode, or one whose viewport is degenerate — collapsed to
/// nothing by a divider drag, or projecting off Earth.
///
/// [`rustdar_radar::voxel::BASE_HALF_WIDTH_KM`] read back rather than a copy
/// of 230, and it is the resampler's own fallback for exactly the same
/// situation: nothing known about the reach yet. The two answering differently
/// would be a pane whose camera arithmetic is scaled against a box the
/// resampler is not building, which shows up as a pan that drifts against the
/// picture.
///
/// It still clears `types::BASE_EXTENT_KM`, so a pane in this state crops
/// nothing a plan view at the raster's floor would have drawn — but the floor
/// is not guaranteed to cover it, because nothing measured the viewport it
/// would have to cover. That is the correct trade for a state that only exists
/// while a pane has no area to draw in: the alternative is refusing to build at
/// all, which reads as a broken pane rather than as a pane that has not been
/// given any room. There is no picture to be wrong about while it is in force —
/// such a pane is painting its empty state — so this is the width the *camera*
/// is posed against for the frames before the first build lands, and nothing
/// else.
pub const BASE_HALF_WIDTH_KM: f64 = rustdar_radar::voxel::BASE_HALF_WIDTH_KM;

impl VolumeRegion {
    /// A region centred on `centre` reaching `half` either side on each axis,
    /// or `None` if the centre is not a point on Earth or the extent is not
    /// finite.
    ///
    /// The extent is **clamped** where the centre is **refused**, and the
    /// asymmetry is the same one [`OrbitCamera::restore`] draws. A centre that is
    /// NaN or off-Earth means the projector was fed a degenerate viewport or a
    /// config file was hand-edited: there is no nearest sensible answer, and
    /// clamping would launder it, because `f64::clamp` propagates NaN. An
    /// extent past the end of its range is a zoom control that has been wound
    /// to its stop, and stopping is what a control should do.
    ///
    /// # Why the clamp is [`HalfExtentKm::clamped`] and not a `clamp` here
    ///
    /// It was `half_width_km.clamp(MIN_HALF_WIDTH_KM, MAX_HALF_WIDTH_KM)` — a
    /// **third** spelling of a bound the resampler and the renderer already
    /// share one definition of, and the only one of the three that clamps the
    /// axes *independently*.
    ///
    /// That difference is not cosmetic once a region has two axes.
    /// [`HalfExtentKm::clamped`] holds the corner inside
    /// [`MAX_HALF_DIAGONAL_KM`] by scaling **both** axes by one factor, which
    /// keeps the box the shape the pane is framing; per-axis clamping keeps the
    /// corner inside the same bound (`MAX_HALF_WIDTH_KM · √2` *is*
    /// `MAX_HALF_DIAGONAL_KM`, so it cannot do otherwise) but changes the
    /// **aspect ratio** to do it — a 450 × 200 km ask comes back 332 × 200,
    /// 1.66:1 where the viewport is 2.25:1. A stopped zoom is a control at its
    /// stop; a silently reshaped box is a picture of ground the pane is not
    /// showing.
    ///
    /// [`HalfExtentKm::clamped`]: rustdar_radar::voxel::HalfExtentKm::clamped
    /// [`MAX_HALF_DIAGONAL_KM`]: rustdar_radar::voxel::MAX_HALF_DIAGONAL_KM
    pub fn new(centre: GeoPoint, half: rustdar_radar::voxel::HalfExtentKm) -> Option<Self> {
        if !centre.is_on_earth() || !half.is_finite() {
            return None;
        }
        Some(Self {
            centre,
            half: half.clamped(),
        })
    }

    /// Where the box is centred.
    pub fn centre(self) -> GeoPoint {
        self.centre
    }

    /// Half the box's extent on each horizontal axis, kilometres.
    ///
    /// Handed on whole rather than as two `f64`s wherever it crosses a call
    /// boundary, for the reason [`HalfExtentKm`] exists: two adjacent,
    /// same-typed numbers naming different axes transpose without a compiler or
    /// a square fixture noticing.
    ///
    /// [`HalfExtentKm`]: rustdar_radar::voxel::HalfExtentKm
    pub fn half_extent_km(self) -> rustdar_radar::voxel::HalfExtentKm {
        self.half
    }

    /// Half the box's east–west extent, kilometres.
    pub fn half_east_km(self) -> f64 {
        self.half.east_km
    }

    /// Half the box's north–south extent, kilometres.
    pub fn half_north_km(self) -> f64 {
        self.half.north_km
    }

    /// Kilometres per cell east–west and north–south, for `cells` cells along
    /// each axis.
    ///
    /// The numbers the pane shows, and the reason a tight region is worth
    /// picking. Answers `None` for a zero cell count rather than dividing by it.
    ///
    /// Two numbers because the grid's cell count is the same on both axes while
    /// the box's extent is not: a 16:9 box spends the same 256 cells over 1.78×
    /// as much ground east–west as north–south, and one figure would have to
    /// pick an axis to be honest about. What that anisotropy costs the picture
    /// is measured on [`rustdar_radar::voxel::VoxelShape`] — under 1.1% of the
    /// ≥20 dBZ volume across four storm volumes, which is why there is no cap
    /// on the box's aspect.
    pub fn resolution_km(self, cells: usize) -> Option<(f64, f64)> {
        Some((
            resolution_km(self.half.east_km, cells)?,
            resolution_km(self.half.north_km, cells)?,
        ))
    }
}

/// Kilometres per cell across a box of `half_width_km`, `cells` cells wide.
///
/// A free function beside [`VolumeRegion::resolution_km`] rather than only the
/// method, because the caption now prints this for boxes that are **not** a
/// picked region — a pane with no region gets the width the volume's reach
/// earns, and there is no `VolumeRegion` anywhere in that path to ask. One
/// definition, two entry points; a caption dividing by its own copy is how the
/// printed km-per-cell and the resampled one drift apart.
pub fn resolution_km(half_width_km: f64, cells: usize) -> Option<f64> {
    (cells > 0).then(|| 2.0 * half_width_km / cells as f64)
}

/// A pane showing a 3D view of the volume.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VolumePane {
    /// Where the eye is. See [`OrbitCamera`].
    pub camera: OrbitCamera,
    /// The box this pane resolved on its last drawn frame — a **readback**, not
    /// a setting.
    ///
    /// The region is the pane's viewport ([`crate::ui_region::region_for_viewport`]),
    /// and only the render arm can measure it: the rect, the `MapMemory` and the
    /// centre all exist inside the egui pass and nowhere else. But the loop
    /// planner runs *outside* that pass and has to name the same ground — every
    /// frame of a 3D loop is resampled over it — so the arm writes what it
    /// measured here for the planner to read.
    ///
    /// **Nothing may write this to aim the pane.** It is the answer to "what did
    /// the viewport come to", and the way to change it is to move the viewport.
    /// A writer that set it directly would be overruled on the very next frame,
    /// which is the quietest kind of broken control there is. That is also why
    /// it is not persisted: a measurement of a viewport that no longer exists is
    /// worse than no measurement, and the viewport itself *is* persisted, so the
    /// first frame after a restore re-measures it.
    ///
    /// `None` before the pane has been drawn in the 3D mode even once, and for a
    /// viewport too degenerate to measure. Both mean the same thing downstream:
    /// nothing here states a width, so `build_voxels` resolves one from the
    /// volume's own reach ([`rustdar_radar::voxel::box_half_width_km`]) and
    /// [`BASE_HALF_WIDTH_KM`] stands in for the camera until that grid lands.
    pub viewport_box: Option<VolumeRegion>,
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

/// The box's full extent in kilometres along each axis for a pane showing
/// `region` — **what the pane itself can work out**, which is the whole answer
/// for a measured viewport and a stand-in otherwise.
///
/// # Why this is derived rather than read off the grid
///
/// The grid is the truth and the painter reads it there. But the pane needs the
/// box's proportions *before* the painter runs, on the frame a camera drag is
/// folded in — the pan scale is a fraction of the box — and the grid lives
/// behind the painter in another crate. Reading last frame's box would put the
/// pan one frame behind the pointer, which is the exact defect
/// `VolumePainter::paint`'s ordering exists to avoid.
///
/// For a `Some` region the two agree by construction rather than by luck:
/// `build_voxels` spans `2 · east_km` and `2 · north_km` horizontally and
/// `base..top` vertically, from the same clamped extent [`VolumeRegion::new`]
/// holds and the same two constants used here. If they ever disagreed the
/// symptom would be a pan that drifts against the picture, which is why they
/// read one definition each.
///
/// **Both horizontal lanes come from the region, and the second one is the
/// point.** This squared the region's single half-width into `x` and `y` for as
/// long as a region had only one number to give. A camera posed against a
/// square box while the grid is rectangular pans at the wrong speed on one axis
/// and puts the pivot somewhere the user did not aim — on every frame before
/// the first grid arrives, and on every frame the painter answers `None`, which
/// is exactly the window this function exists to cover.
///
/// # The one case this cannot answer alone
///
/// `None` — a pane not yet drawn in the 3D mode, or a viewport too degenerate
/// to measure. `build_voxels` answers an unstated width with
/// [`rustdar_radar::voxel::box_half_width_km`] of the volume's own reach, which
/// is a fact about the *data* — 325.4 km of half-width for a 460 km
/// reflectivity volume, 212.1 for the 300 km Doppler moments — and no type in
/// this crate holds a volume. So [`BASE_HALF_WIDTH_KM`] stands in here, and a
/// caller that has a grid prefers `VolumePainter::box_size_km` over this. There
/// is no picture to be wrong about in the meantime: such a pane is painting its
/// empty state.
///
/// A free function over the region rather than a method on [`VolumePane`],
/// because the region is no longer *on* the pane — it is the pane's viewport,
/// measured on the frame it is needed.
pub fn box_size_km(region: Option<VolumeRegion>) -> [f32; 3] {
    let half = region.map_or(
        rustdar_radar::voxel::HalfExtentKm::square(BASE_HALF_WIDTH_KM),
        VolumeRegion::half_extent_km,
    );
    [
        (2.0 * half.east_km) as f32,
        (2.0 * half.north_km) as f32,
        (rustdar_radar::voxel::DEFAULT_TOP_KM_MSL - rustdar_radar::voxel::DEFAULT_BASE_KM_MSL)
            as f32,
    ]
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
    /// Multiplicative dolly: a factor above 1 brings the eye *in*, dividing
    /// [`OrbitCamera::eye_distance`] by it.
    ///
    /// **No gesture produces one.** Scroll and pinch aim the geography, in both
    /// render modes — `ui_region::zoom_viewport` — so the UI leaves this at 1.0
    /// on every frame, and the eye follows the box for free because
    /// `eye_distance` is a ratio of its half-diagonal. The camera's standoff is
    /// set absolutely instead, through [`OrbitCamera::set_eye_distance`] and the
    /// pane's own control.
    ///
    /// It stays a ratio here because that is what a *delta* means, and because
    /// the refusal and the clamp it carries into [`OrbitCamera::nudge`] are the
    /// camera's invariants rather than the gesture's: a caller of this public
    /// API may still move the eye by a ratio, and when it does, a non-finite or
    /// non-positive factor must refuse the whole delta rather than launder a NaN
    /// into a camera whose staleness key never equals itself.
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
pub const MIN_EYE_DISTANCE: f32 = 0.05;
pub const MAX_EYE_DISTANCE: f32 = 8.0;

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
/// 36:1 wide open: a spherical bound of one half-extent would either let the
/// pivot leave the box sideways or stop it well short of the top face.
const MAX_PIVOT_FRACTION: f32 = 1.0;

/// The vertical exaggeration a 3D pane starts at.
///
/// A wide-open box is 651 km across for a WSR-88D's reflectivity and 18 km
/// tall — **36:1** — and at true
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

    /// Set how far back the eye sits, or leave it exactly as it is.
    ///
    /// # Why this is a control and not a gesture
    ///
    /// The scroll wheel used to drive it, and no longer does: scroll and pinch
    /// aim the *geography* now, in both render modes, which is the one meaning
    /// of "zoom" the application has. That is not a loss of magnification —
    /// tightening the box brings the eye in with it, because this number is a
    /// ratio of the box's half-diagonal rather than a distance — but it does
    /// leave the *framing* with no gesture, and framing is a real judgement: how
    /// much of the pane the box fills, and whether the eye is outside it looking
    /// in or inside it looking out. [`MIN_EYE_DISTANCE`] documents the inside
    /// view as a supported camera, and a change that made it unreachable would
    /// have deleted a shipped capability by omission.
    ///
    /// So it is expressed where every other per-pane judgement is expressed, on
    /// the pane's own controls, which is also the only spelling that works on a
    /// touch screen and in a browser: there is no modifier key to hang a second
    /// zoom on, and shipping one would have been a desktop-only model.
    ///
    /// Refuses a non-finite value and clamps a finite one, for the same reasons
    /// [`Self::set_vertical_exaggeration`] gives.
    pub fn set_eye_distance(&mut self, eye_distance: f32) {
        if !eye_distance.is_finite() {
            return;
        }
        self.eye_distance = eye_distance.clamp(MIN_EYE_DISTANCE, MAX_EYE_DISTANCE);
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
