//! What a pane *is*, as opposed to what it is looking at: the discriminant plus
//! the state a cross-section or 3D pane needs that a map pane does not.

use chrono::NaiveDateTime;
use rustdar_radar::types::{RadarProduct, RenderView};
use rustdar_radar::xsect::CrossSection;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Which of the two things a pane is. How a pane *draws* that place is a
/// separate question answered by [`MapRender`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PaneKind {
    /// A patch of ground, drawn either as the plan view or as the 3D volume
    /// standing on it. See [`MapRender`].
    #[default]
    Map,
    /// A vertical slice through the volume along a line drawn on a map pane.
    CrossSection,
}

/// How a map pane draws the ground it is looking at: the same *place* — site,
/// viewport, product, moment — differing only in where the eye is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MapRender {
    /// One sweep, drawn flat and looked down on.
    #[default]
    Plan,
    /// The whole volume over the same ground, raymarched from an orbit camera,
    /// standing on the pane's own map as its floor.
    Volume,
}

impl PaneKind {
    /// What a pane of this kind draws when it is not a map, or `None` because a
    /// map pane's view depends on its [`MapRender`] rather than on its kind.
    pub fn render_view(self) -> Option<RenderView> {
        match self {
            // Two answers, and the kind cannot choose between them.
            Self::Map => None,
            // A section interpolates between the tilts bracketing each sample by
            // beam height — vertical structure, which one sweep does not have.
            Self::CrossSection => Some(RenderView::CrossSection),
        }
    }
}

impl MapRender {
    /// What a render dispatched for a map pane in this mode produces.
    /// Exhaustive: a third mode must be classified rather than defaulting.
    pub fn render_view(self) -> RenderView {
        match self {
            // One sweep, chosen by `render::find_sweep` out of the product's moment.
            Self::Plan => RenderView::PlanView,
            // A raymarch reads a grid resampled from every cut in the ladder.
            Self::Volume => RenderView::Volume,
        }
    }
}

/// The per-kind state a pane holds, and the sole source of its [`PaneKind`].
#[derive(Debug, PartialEq)]
pub enum PaneContent {
    /// A patch of ground. Where it is looking is already a flat field on the
    /// pane; what is in here is how it draws that place.
    Map(Box<MapPane>),
    CrossSection(Box<CrossSectionPane>),
}

impl Default for PaneContent {
    /// A **plan-view** map: this is the value left in `Gui::panes[idx]` while a
    /// pane is `mem::take`n, so it is what every all-panes filter reads about
    /// the pane currently being drawn. See the module doc.
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

    /// What a render dispatched for this pane produces — the single content →
    /// view table. `rustdar_app` keys its render cache and sibling-texture
    /// broadcast on the *view*, not the pane kind: a cached raster outlives the
    /// pane that asked for it.
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
    /// that owns them is going away. Called from `Gui::clear_graphics_state`.
    pub fn release_textures(&mut self) {
        match self {
            // Neither mode of a map pane holds a handle here: the plan view's
            // raster is in the frontend's render cache, the 3D mode's grids in
            // the application-wide volume store.
            Self::Map(_) => {}
            // The section raster, and **only** the raster. The `CrossSection`
            // behind it is plain memory and is what a hover reads; `rendered_for`
            // stays with it so the restore can re-upload rather than re-cut.
            Self::CrossSection(section) => section.texture = None,
        }
    }
}

/// A map pane's own state: how it draws its ground, and what the 3D mode needs
/// in order to.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MapPane {
    /// Plan view or 3D volume. See [`MapRender`].
    pub render: MapRender,
    /// State the 3D mode draws from, kept across a return to the plan view.
    pub volume: VolumePane,
}

use rustdar_geo::GeoPoint;

/// The line a cross-section is cut along, stored **geographically**.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SectionLine {
    a: GeoPoint,
    b: GeoPoint,
}

impl SectionLine {
    /// A section line from `a` to `b`, or `None` for a line that cannot be cut.
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
#[derive(Clone, Debug, PartialEq)]
pub struct VolumeStamp {
    /// NEXRAD site code the volume belongs to (e.g. "KTLX").
    pub site: String,
    /// When the radar collected the volume (UTC).
    pub collected: NaiveDateTime,
}

/// Everything a rendered cross-section depends on, so that "is what is on
/// screen still the truth?" is one comparison.
#[derive(Clone, Debug, PartialEq)]
pub struct SectionTarget {
    pub volume: VolumeStamp,
    /// The moment the section was cut from. Not every product is samplable, so
    /// this is narrower than the pane's product picker.
    pub product: RadarProduct,
    pub line: SectionLine,
    /// The fingerprint of the tilt ladder this cut would be made from, at
    /// dispatch. See the type's docs. `0` when no ladder resolves at all.
    pub ladder: u64,
}

/// Why a section pane has no picture, when it has none. Every variant is a state
/// a user can reach without doing anything wrong, and each has a *different*
/// thing to say. The pane holds at most one, written by whoever refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectionUnavailable {
    /// No decoded volume for the pane's site yet — the ordinary startup and
    /// site-switch state.
    AwaitingVolume,
    /// The volume was joined **mid-flight** and its coverage pattern has not
    /// arrived, so it carries no elevation cut table. `chunks.rs` stands in
    /// `placeholder_coverage_pattern(0)` until the VCP message lands, and
    /// `VolumeSampler::new` refuses a scan like that.
    AwaitingCoveragePattern,
    /// The coverage pattern **has** arrived — so there is a tilt ladder to cut
    /// along — but no sweep has been sealed onto it yet. Lasts as long as one
    /// cut takes, up to about half a minute. See
    /// `app_render::section_source_refusal`.
    AwaitingFirstSweep,
    /// The pane's product has no vertical structure to slice — the column
    /// integrals, the hybrid-scan composite, the derived velocity fields. See
    /// `rustdar_radar::sampler::samplable`.
    ProductHasNoVerticalStructure(RadarProduct),
    /// The cut was dispatched and answered nothing. Deliberately distinct from
    /// "not yet": a section that will never appear must not look like one that is.
    RenderFailed,
    /// **This volume** carries nothing to cut under the pane's product: no sweep
    /// holds the moment, or the derivation refused it. A property of the volume,
    /// not of the product — which is why its staleness key carries the volume
    /// stamp — unlike [`ProductHasNoVerticalStructure`](Self::ProductHasNoVerticalStructure).
    ProductMissingFromVolume(RadarProduct),
}

impl SectionUnavailable {
    /// One line, addressed to whoever is looking at the empty pane. Says what is
    /// missing and, where the user can do something, what.
    pub fn message(self) -> String {
        match self {
            // The cold-start window: a site switch fires the archive fetch
            // immediately, and once any volume lands this is never seen again.
            Self::AwaitingVolume => {
                "Downloading this site's first volume - the section appears the moment it lands"
                    .to_owned()
            }
            Self::AwaitingCoveragePattern => {
                "This volume was joined mid-scan and its coverage pattern has not arrived yet, \
                 so there is no tilt ladder to cut along. It will appear on the next volume."
                    .to_owned()
            }
            // One sentence, deliberately: this is the state a user meets seconds
            // after switching to a live site, and it is nothing going wrong.
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
#[derive(Clone, Default, PartialEq)]
pub struct CrossSectionPane {
    /// The line to cut along, or `None` until the user has drawn one — the
    /// ordinary state between being converted and being aimed.
    pub line: Option<SectionLine>,
    /// Which map pane the line was drawn on, or `None` for a section that has
    /// never been aimed. Persisted, and validated against the pane count on
    /// load: an index past the end of the layout is how a config saved from a
    /// wider split comes back. Nothing sets it yet; it is the retarget rule's
    /// input.
    pub source_pane: Option<usize>,
    /// What the section currently on screen was rendered for, or `None` before
    /// the first render. Compared against the current volume and line to decide
    /// whether to render again.
    pub rendered_for: Option<SectionTarget>,
    /// The cut itself: the picture, the values a hover reads, and the status
    /// plane that says *why* a pixel is blank. `Arc` because the three planes
    /// are ~18 MB natively and a hover reads this every frame. Kept when the
    /// texture is released, so the restore can re-upload rather than re-cut.
    pub section: Option<Arc<CrossSection>>,
    /// The section's raster, uploaded. Dropped by
    /// [`PaneContent::release_textures`] and put back by
    /// `App::restore_section_textures` from [`section`](Self::section).
    pub texture: Option<egui::TextureHandle>,
    /// Why there is no section, when there is none *and* a line has been drawn.
    pub unavailable: Option<SectionUnavailable>,
    /// Whether the caption's ⓘ detail is expanded. View state, so deliberately
    /// **not** persisted and **not** part of any staleness key: toggling it must
    /// never cost a re-cut.
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

/// Everything a built voxel grid depends on — the same argument as
/// [`SectionTarget`].
#[derive(Clone, Debug, PartialEq)]
pub struct VolumeTarget {
    pub volume: VolumeStamp,
    pub product: RadarProduct,
    /// The ground to resample, or `None` for the default box about the site.
    pub region: Option<VolumeRegion>,
}

/// The patch of ground a 3D pane resamples, stored **geographically**.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VolumeRegion {
    centre: GeoPoint,
    half: rustdar_radar::voxel::HalfExtentKm,
}

/// The half-width a pane falls back to when nothing has sized its box yet,
/// kilometres.
pub const BASE_HALF_WIDTH_KM: f64 = rustdar_radar::voxel::BASE_HALF_WIDTH_KM;

impl VolumeRegion {
    /// A region centred on `centre` reaching `half` either side on each axis,
    /// or `None` if the centre is not a point on Earth or the extent is not
    /// finite.
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

    /// Half the box's extent on each horizontal axis, kilometres. Handed on
    /// whole rather than as two `f64`s: two adjacent, same-typed numbers naming
    /// different axes transpose unnoticed.
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
    /// each axis. `None` for a zero cell count rather than dividing by it.
    pub fn resolution_km(self, cells: usize) -> Option<(f64, f64)> {
        Some((
            resolution_km(self.half.east_km, cells)?,
            resolution_km(self.half.north_km, cells)?,
        ))
    }
}

/// Kilometres per cell across a box of `half_width_km`, `cells` cells wide.
pub fn resolution_km(half_width_km: f64, cells: usize) -> Option<f64> {
    (cells > 0).then(|| 2.0 * half_width_km / cells as f64)
}

/// A pane showing a 3D view of the volume.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VolumePane {
    /// Where the eye is. See [`OrbitCamera`].
    pub camera: OrbitCamera,
    /// The ground this pane resamples — a **stored choice**, not a readback.
    pub region: Option<VolumeRegion>,
    /// Which map pane the region was dragged out on, or `None` for a pane nobody
    /// aimed.
    pub source_pane: Option<crate::pane::PaneId>,
    /// Which volume the grid on screen was built from, or `None` before the
    /// first build.
    pub rendered_for: Option<VolumeTarget>,
    /// Whether this pane has turned the map floor **off**.
    pub hide_floor: bool,
    /// Whether this pane's Volume Alpha editor window is open. Session state,
    /// not persisted: the *curves* are the durable thing (per product, in the UI
    /// config); an open tool window is a posture.
    pub alpha_editor_open: bool,
    /// How this pane draws its volume: the lit accumulation or an isosurface.
    pub view_mode: VolumeViewMode,
}

/// How a 3D pane draws its volume. `Default` is the lit volume — what every
/// config from before this enum existed loads as through `#[serde(default)]`.
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
/// `region` — **what the pane itself can work out**.
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

/// A movement of the orbit camera: two angles and a zoom factor. A struct rather
/// than three `f32` parameters because `yaw_deg` and `pitch_deg` are the same
/// type, adjacent, and plausible in either position. `Default` is "the camera
/// did not move", hand-written because `zoom_factor` is multiplicative.
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
    pub zoom_factor: f32,
    /// Where to move the pivot, as a fraction of the box's half-extent on each
    /// axis. See [`OrbitCamera::pivot`]. **Already resolved into world axes by
    /// the caller** — the rotation needs the camera basis, the box's proportions
    /// and the viewport height, and only
    /// [`crate::volume_view::pan_for_drag`] has all three.
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
/// Eye distance is in multiples of the volume box's *framing radius* — half the
/// diagonal of the square its north–south extent stands on
/// (`volume_view::framing_radius_km`). 1.0 is the eye on the ground corner of a
/// square box.
pub const MIN_EYE_DISTANCE: f32 = 0.05;
pub const MAX_EYE_DISTANCE: f32 = 8.0;

/// How far the pivot may be pushed from the box's centre, as a fraction of the
/// box's half-extent on each axis. 1.0 makes the guarantee exactly: the pivot is
/// on the box's own surface, so the point the camera is aimed at is always a
/// point of the box. Per axis rather than as a radius, because the box is a
/// 36:1 pancake.
const MAX_PIVOT_FRACTION: f32 = 1.0;

/// The vertical exaggeration a 3D pane starts at. A wide-open box is 651 km
/// across for a WSR-88D's reflectivity and 18 km tall — **36:1** — and at true
/// proportions it reads as a sheet of paper. 3 is where a supercell's overhang
/// and a stratiform sheet become distinguishable, and the bottom of the 3–8
/// range GR2Analyst-like views are read at.
pub const DEFAULT_VERTICAL_EXAGGERATION: f32 = 3.0;
/// True proportions. The bottom of the control's travel is 1, never 0: a zero
/// would collapse the box to a plane, which divides by zero in `box_from_world`.
pub const MIN_VERTICAL_EXAGGERATION: f32 = 1.0;
/// Past about 12 the box is taller than it is wide: a 15 km updraught drawn
/// 180 km tall is no longer a shape a forecaster can read a height off.
pub const MAX_VERTICAL_EXAGGERATION: f32 = 12.0;

/// Where the eye is, for a 3D pane: an orbit about the centre of the volume.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OrbitCamera {
    /// Azimuth about the vertical axis, degrees in `[0, 360)`.
    yaw_deg: f32,
    /// Elevation above the horizontal, degrees in `[-MAX_PITCH_DEG,
    /// MAX_PITCH_DEG]`.
    pitch_deg: f32,
    /// Eye distance in multiples of the volume box's framing radius.
    eye_distance: f32,
    /// The point the orbit turns about and the camera looks at, as a fraction of
    /// the box's half-extent on each axis, each component in
    /// `[-MAX_PIVOT_FRACTION, MAX_PIVOT_FRACTION]`.
    pivot: [f32; 3],
    /// How much the vertical axis is stretched when the box is drawn, in
    /// `[MIN_VERTICAL_EXAGGERATION, MAX_VERTICAL_EXAGGERATION]`.
    vertical_exaggeration: f32,
}

impl Default for OrbitCamera {
    /// Looking north-ish from above the south-west, aimed at the box's centre
    /// and stretched by [`DEFAULT_VERTICAL_EXAGGERATION`].
    fn default() -> Self {
        Self {
            yaw_deg: 225.0,
            pitch_deg: 25.0,
            eye_distance: crate::volume_view::eye_distance_for_plan_scale(),
            pivot: [0.0; 3],
            vertical_exaggeration: DEFAULT_VERTICAL_EXAGGERATION,
        }
    }
}

impl OrbitCamera {
    /// Move the camera by `delta`, or leave it exactly as it is.
    pub fn nudge(&mut self, delta: OrbitDelta) {
        if !delta.yaw_deg.is_finite() || !delta.pitch_deg.is_finite() {
            return;
        }
        if !delta.zoom_factor.is_finite() || delta.zoom_factor <= 0.0 {
            return;
        }
        // Checked with the rest and refused with the rest: a pan arrives through
        // a division by the viewport height that a one-frame-wide pane makes
        // infinite.
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
    pub fn set_eye_distance(&mut self, eye_distance: f32) {
        if !eye_distance.is_finite() {
            return;
        }
        self.eye_distance = eye_distance.clamp(MIN_EYE_DISTANCE, MAX_EYE_DISTANCE);
    }

    /// Set the vertical exaggeration, or leave it exactly as it is.
    pub fn set_vertical_exaggeration(&mut self, exaggeration: f32) {
        if !exaggeration.is_finite() {
            return;
        }
        self.vertical_exaggeration =
            exaggeration.clamp(MIN_VERTICAL_EXAGGERATION, MAX_VERTICAL_EXAGGERATION);
    }

    /// Rebuild a camera from persisted angles, or `None` if they are unusable.
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

    /// Eye distance in multiples of the volume box's framing radius.
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
