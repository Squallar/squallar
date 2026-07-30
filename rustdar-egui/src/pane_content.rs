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
//! # `Default` means `Map`, and that is forced
//!
//! `PaneState` owns `egui::TextureHandle`s and `egui::TextureHandle` is not
//! `Default`, so `PaneState`'s hand-written `Default` is the only one there can
//! be; `PaneContent: Default` is therefore the single bound this module has to
//! satisfy, and the only variant that can supply it is the one holding no
//! textures. So `PaneContent::default() == Map`.
//!
//! **That is also the sharpest hazard in the feature.** Those six `mem::take`
//! sites mean that during the UI pass `self.panes[idx]` is a *default* pane —
//! i.e. reads as a map pane — whatever the real pane is. Nothing may branch on
//! kind through `self.panes[..]` or `active_pane()` while the pane is out;
//! branch on the taken value. The compiler cannot help with this, which is why
//! the mitigation is the `last_pane_content` probe: it records what each render
//! arm actually drew, so a branch reading the wrong thing shows up as an arm
//! that ran for the wrong kind rather than as a subtly wrong picture.

use chrono::NaiveDateTime;
use rustdar_radar::types::RadarProduct;
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
    /// there, quietly. This is the UI-side half of that safety property; the
    /// data-side half is `render_dispatch::needs_whole_volume`, which asks the
    /// same question of a *product*. One predicate, two names, because they are
    /// two different questions with one answer.
    pub fn consumes_whole_volume(self) -> bool {
        !matches!(self, Self::Map)
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
}

/// A point on the ground, in degrees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GeoPoint {
    pub lat: f64,
    pub lon: f64,
}

impl GeoPoint {
    /// Whether both coordinates are finite. Non-finite coordinates are refused
    /// at the boundary rather than clamped; see [`SectionLine::new`].
    pub fn is_finite(self) -> bool {
        self.lat.is_finite() && self.lon.is_finite()
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
    /// * **Non-finite endpoints.** They arrive from a projector fed a degenerate
    ///   viewport, or from a config file. Rejecting rather than clamping is the
    ///   rule throughout this crate for the same reason it is on
    ///   `StormMotionOverride::sample`: `f32::clamp` and `f64::clamp` *propagate*
    ///   NaN, so a clamp launders a bad value into a bad value that looks
    ///   checked. Worse, a NaN endpoint reaches [`SectionTarget`], where
    ///   `NaN != NaN` makes the staleness key never match itself — so the pane
    ///   re-renders its section on every frame, forever, with no error anywhere.
    /// * **Coincident endpoints.** A zero-length line has no bearing, so the
    ///   great-circle walk along it is `0/0` and every column of the raster
    ///   samples the same point. This is the arithmetic bar; the usability bar
    ///   (a drag shorter than a couple of dozen points is a mis-click, not a
    ///   line) belongs to the interaction that produces the drag.
    pub fn new(a: GeoPoint, b: GeoPoint) -> Option<Self> {
        if !a.is_finite() || !b.is_finite() {
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
    /// What the section currently on screen was rendered for, or `None` before
    /// the first render. Compared against the current volume and line to decide
    /// whether to render again.
    pub rendered_for: Option<SectionTarget>,
}

/// Everything a built voxel grid depends on.
///
/// The same argument as [`SectionTarget`], minus the geometry: a volume render
/// covers the whole scan, so the only inputs are which volume and which moment.
/// The camera is deliberately *not* in here — orbiting re-draws from the grid
/// already in hand and must not rebuild it.
#[derive(Clone, Debug, PartialEq)]
pub struct VolumeTarget {
    pub volume: VolumeStamp,
    pub product: RadarProduct,
}

/// A pane showing a 3D view of the volume.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VolumePane {
    /// Where the eye is. See [`OrbitCamera`].
    pub camera: OrbitCamera,
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

    /// `Default` has to be `Map`: `PaneState` owns `egui::TextureHandle`s, which
    /// are not `Default`, so a default pane can only be the kind that holds
    /// none. Pinned because six `mem::take` sites depend on it and because the
    /// hazard it creates — a pane reading as `Map` mid-frame — is only
    /// understandable if this is known to be deliberate.
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
    }

    /// A line that cannot be cut is not representable. Both refusals matter:
    /// a NaN endpoint would make [`SectionTarget`] never equal itself and
    /// re-render the pane on every frame forever, and a zero-length line has no
    /// bearing to walk along.
    #[test]
    fn a_section_line_refuses_endpoints_it_cannot_be_cut_along() {
        assert!(SectionLine::new(point(35.3, -97.3), point(35.6, -97.0)).is_some());

        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(
                SectionLine::new(point(bad, -97.3), point(35.6, -97.0)).is_none(),
                "{bad} latitude accepted"
            );
            assert!(
                SectionLine::new(point(35.3, -97.3), point(35.6, bad)).is_none(),
                "{bad} longitude accepted"
            );
        }

        assert!(
            SectionLine::new(point(35.3, -97.3), point(35.3, -97.3)).is_none(),
            "a zero-length line has no bearing: every column would sample one point"
        );
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
