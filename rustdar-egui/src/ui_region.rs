//! Dragging out the patch of ground a 3D pane resamples, and drawing it back on
//! the map it was dragged on.
//!
//! # The shape of the interaction, and why each part of it is the way it is
//!
//! A menu toggle **arms** the mode; a drag on a map pane then draws a square,
//! and releasing commits it. Every one of those decisions has a failure it is
//! avoiding:
//!
//! * **Armed rather than modeless.** A drag on a map already means pan, and a
//!   region drag is a rare, deliberate act. Overloading the pan gesture would
//!   make every pan a coin flip.
//! * **The anchor is stored geographically, converted on the press frame**, in
//!   [`RegionDrag::begin`] — which runs inside `Map::show`, the only place a
//!   `Projector` exists. A pixel anchor denotes different ground after a mid-drag
//!   wheel zoom, and zoom is *not* suppressed while armed even though pan is. The
//!   same argument [`SectionLine`](crate::pane::SectionLine) makes at length.
//! * **Pan is suppressed unconditionally while armed**, not merely while a drag
//!   is in flight. A press that is going to become a region drag is
//!   indistinguishable from one that is going to become a pan until the pointer
//!   moves, and by then the map has already slid.
//! * **The square is drawn from the first frame of the drag.** The resample takes
//!   a centre and one half-width, so a free rectangle would have to be squared —
//!   and silently squaring a user's drag reads as a bug the first time they drag
//!   a wide box and get a tall one. Pressing sets the centre and dragging sets the
//!   half-width, which is the shape of the request made visible.
//! * **A too-small drag is discarded and the mode stays armed.** A mis-click
//!   while armed should cost nothing, least of all the mode the user just turned
//!   on. The bar is the resampler's own [`MIN_HALF_WIDTH_KM`], so the only
//!   regions that commit are ones that will be honoured rather than clamped.
//! * **The preview stops growing at the resampler's maximum.** The commit goes
//!   through [`VolumeRegion::new`], which clamps the half-width to 230 km — so
//!   an uncapped preview past that point would paint an ever-bigger square and
//!   release the same box every time. [`RegionDrag::extend_to`] caps the drag at
//!   the same constant, so what is drawn is what is resampled at both ends: too
//!   big stops live under the pointer, too small is refused on release.
//! * **The commit is applied after the pane loop.** [`PendingRegion`] is a
//!   record, not an edit. Applying it inside the loop could grow `pane_count`
//!   mid-frame, which changes `pane_rect` for every pane not yet drawn and
//!   desynchronises them from the rects `detect_active_pane_click` has already
//!   hit-tested this frame.
//!
//! # The half-width is the resolution
//!
//! The grid is a fixed cell count, so this drag is not a crop — it is a zoom that
//! spends the same cells over less ground. 80 km is 0.625 km per cell and 20 km
//! is 0.156 km. That is the main reason to pick a region at all, which is why the
//! preview names the figure while the drag is still in flight rather than leaving
//! it to be discovered after a 155 ms rebuild.
//!
//! [`MIN_HALF_WIDTH_KM`]: rustdar_radar::voxel::MIN_HALF_WIDTH_KM

use crate::pane::{GeoPoint, PaneKind, PaneState, VolumeRegion};

/// Kilometres per degree of latitude, the sphere approximation the rest of this
/// crate's map arithmetic already uses (`render_radar_range_ring`).
///
/// Only ever used to turn a *distance in kilometres* back into a screen rect for
/// the preview, never to decide what is resampled: the drag's own half-width
/// comes from [`rustdar_radar::beam::site_bearing_range_km`], which is the
/// codebase's real geodesy. The approximation is therefore worth at most a pixel
/// of preview edge, and never a kilometre of grid.
const KM_PER_DEGREE_LAT: f64 = 111.32;

/// The armed region interaction's yellow: the box in flight, the resolution
/// hint over its top edge, and the active pane's armed hint chip all paint in
/// this one colour — so the chip advertises exactly the box the drag will
/// draw. (The *committed* box drawn back on the map is deliberately not this
/// colour: it is a record, not the gesture.)
pub(crate) const REGION_ARM_COLOR: egui::Color32 = egui::Color32::from_rgb(255, 220, 120);

/// A region drag in flight.
///
/// Geographic, for the reason the module doc gives. Held on the `Gui` rather than
/// on the pane because it is a property of the *gesture*, and a gesture that
/// started on one pane must not be inherited by another when the layout changes
/// under it — which is what `pane_idx` is checked for on every frame of the drag.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RegionDrag {
    /// Which map pane the press landed on. A drag belongs to one pane for its
    /// whole life; the pointer leaving that pane's rect does not end it, because
    /// dragging past the edge of a pane to make a big box is ordinary.
    pane_idx: usize,
    /// The box's centre, fixed on the press frame and never revised.
    centre: GeoPoint,
    /// Half-width in kilometres as the pointer currently stands. Capped at the
    /// resampler's maximum on the way in — see [`Self::extend_to`] — but *not*
    /// held up to its minimum: a too-small drag is refused whole at commit
    /// rather than resized. Zero until the pointer moves.
    half_width_km: f64,
}

impl RegionDrag {
    /// Start a drag centred on `centre`.
    ///
    /// `None` for a press the projector could not place on Earth — which happens
    /// for a pane collapsed to nothing by a divider drag. Refused rather than
    /// clamped, because there is no nearest sensible patch of ground and
    /// `f64::clamp` propagates NaN.
    pub(crate) fn begin(pane_idx: usize, centre: GeoPoint) -> Option<Self> {
        centre.is_on_earth().then_some(Self {
            pane_idx,
            centre,
            half_width_km: 0.0,
        })
    }

    /// Which pane this drag belongs to.
    pub(crate) fn pane_idx(self) -> usize {
        self.pane_idx
    }

    /// The centre the press fixed.
    pub(crate) fn centre(self) -> GeoPoint {
        self.centre
    }

    /// Half-width as it currently stands, kilometres.
    pub(crate) fn half_width_km(self) -> f64 {
        self.half_width_km
    }

    /// Re-measure the half-width against a pointer now over `corner`.
    ///
    /// **Chebyshev, not Euclidean**: the half-width is the larger of the two
    /// axis distances, so the square's *edge* follows the pointer rather than its
    /// corner. Dragging straight right therefore grows the box at the rate the
    /// pointer moves, which is what makes the square read as being pulled out
    /// rather than as tracking something behind the cursor.
    ///
    /// A `corner` that is not on Earth leaves the drag exactly as it was. That is
    /// the same refusal [`Self::begin`] makes, and it matters more here: this runs
    /// every frame, so a single laundered NaN would stick for the rest of the
    /// drag.
    ///
    /// **The result is capped at the resampler's maximum** —
    /// [`MAX_HALF_WIDTH_KM`](rustdar_radar::voxel::MAX_HALF_WIDTH_KM), the same
    /// ceiling [`VolumeRegion::new`] clamps to on commit. The preview box and its
    /// hint read this value straight off the drag, so without the cap a long drag
    /// would paint an ever-bigger square past 230 km and release the same box
    /// every time — what is drawn has to be what is resampled. The *minimum* is
    /// deliberately not applied here: a too-small drag is refused whole by
    /// [`Self::commit`] rather than resized, so the preview honestly shows the
    /// too-small square that is about to be discarded.
    pub(crate) fn extend_to(&mut self, corner: GeoPoint) {
        if !corner.is_on_earth() {
            return;
        }
        let (bearing_deg, range_km) = rustdar_radar::beam::site_bearing_range_km(
            self.centre.lat,
            self.centre.lon,
            corner.lat,
            corner.lon,
        );
        let bearing = bearing_deg.to_radians();
        let east = (range_km * bearing.sin()).abs();
        let north = (range_km * bearing.cos()).abs();
        let half = east.max(north);
        if half.is_finite() {
            self.half_width_km = half.min(rustdar_radar::voxel::MAX_HALF_WIDTH_KM);
        }
    }

    /// The region this drag would commit, or `None` if it is too small to be one.
    ///
    /// The bar is the resampler's own minimum rather than a pixel count, and that
    /// is the useful choice: a drag below it would be *clamped* up by
    /// `build_voxels`, so committing it would resample a box the user did not
    /// draw and the pane's own resolution readout would describe the wrong
    /// picture. Refusing instead means every committed region is one that will be
    /// honoured exactly.
    ///
    /// The mode stays armed when this answers `None` — that decision belongs to
    /// the caller, and it is stated here because it is the reason this returns an
    /// `Option` rather than clamping.
    pub(crate) fn commit(self) -> Option<VolumeRegion> {
        (self.half_width_km >= rustdar_radar::voxel::MIN_HALF_WIDTH_KM)
            .then(|| VolumeRegion::new(self.centre, self.half_width_km))
            .flatten()
    }
}

/// The square's corners as geographic points, `(north-west, south-east)`.
///
/// For drawing only. A free function over a centre and a half-width rather than a
/// method, so that a *committed* region and the preview of the drag that produced
/// it are drawn by the same arithmetic — two versions disagreeing by a pixel
/// would be read as the commit having moved the box.
///
/// The latitude conversion is the flat approximation named on
/// [`KM_PER_DEGREE_LAT`]; the longitude one divides by `cos(lat)` so the box is
/// square in *kilometres* rather than in degrees, which is the whole point — a
/// degree-square box drawn at 35°N would be 22% wider than it is tall and would
/// not be the box that gets resampled.
///
/// `None` at the poles, where `cos(lat)` is zero and every longitude is the same
/// place. No NEXRAD site is within 20° of one; the check is here because the
/// alternative is an infinity in a painter.
pub(crate) fn corners_for(centre: GeoPoint, half_width_km: f64) -> Option<(GeoPoint, GeoPoint)> {
    let d_lat = half_width_km / KM_PER_DEGREE_LAT;
    let cos_lat = centre.lat.to_radians().cos();
    if !(cos_lat.is_finite() && cos_lat.abs() > 1e-6) {
        return None;
    }
    let d_lon = half_width_km / (KM_PER_DEGREE_LAT * cos_lat);
    let nw = GeoPoint {
        lat: centre.lat + d_lat,
        lon: centre.lon - d_lon,
    };
    let se = GeoPoint {
        lat: centre.lat - d_lat,
        lon: centre.lon + d_lon,
    };
    (d_lat.is_finite() && d_lon.is_finite()).then_some((nw, se))
}

/// A committed region, waiting for the pane loop to finish.
///
/// Deferred for the reason the module doc gives: applying it can grow the pane
/// count, and growing it mid-loop desynchronises panes not yet drawn from the
/// rects `detect_active_pane_click` already hit-tested.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PendingRegion {
    /// The map pane it was dragged on — the retarget rule's input, and what a 3D
    /// pane records as its `source_pane` so a second drag on the same map re-aims
    /// the pane already sourced from it.
    pub(crate) source_pane: usize,
    pub(crate) region: VolumeRegion,
}

/// Which pane a committed region should be applied to.
///
/// # The rule, and why it is total
///
/// In order: **re-aim** a 3D pane already sourced from this map; else re-aim a
/// **sourceless** 3D pane — one whose region was never dragged: converted from
/// the menu, reset, or restored with a source index the layout no longer has
/// (an ordinary restore keeps its source; `ui_config` drops only dangling
/// ones); else **grow** the layout and make the new pane a 3D view; else
/// re-aim the lowest-indexed 3D pane there is; else **convert** the
/// highest-indexed pane that is not the map the region was drawn on.
///
/// Every step exists to avoid a specific wrong answer. Re-aiming first is what
/// stops a second drag on the same map opening a second 3D pane — the common case
/// is adjusting a box, not wanting another view of it. A sourceless pane beats
/// growing because it is *nobody's*: a user with exactly one 3D pane that no
/// map feeds who drags a region means "aim that one", and growing instead
/// surprises them with a sibling — whereas a pane sourced from *another* map is
/// that map's to re-aim, so growing still beats stealing it. Growing before
/// re-aiming a pane some other map feeds is what makes the first drag on a
/// single-map layout produce a 3D view beside the map rather than replacing it.
/// Converting last, and converting the *highest* index, is what keeps the map
/// being drawn on — and the user's primary pane — for as long as there is any
/// other pane to spend.
///
/// It is total on purpose: there is no arrangement of panes for which a drag
/// silently does nothing. A gesture that completes and produces no visible change
/// is indistinguishable from one the app failed to receive.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum RegionDestination {
    /// Aim this existing pane, which is already a 3D view.
    Existing(usize),
    /// Grow the layout to this many panes and aim the last one.
    Grow(usize),
    /// Convert this pane to a 3D view and aim it.
    Convert(usize),
}

/// Resolve [`RegionDestination`] for a region dragged on `source_pane`.
///
/// `max_panes` is the layout's ceiling for the current width class; `panes` is
/// the visible slice. `None` only for a layout with no panes at all, which the
/// pane loop cannot produce.
pub(crate) fn destination_for(
    panes: &[PaneState],
    source_pane: usize,
    max_panes: usize,
) -> Option<RegionDestination> {
    // Already sourced from this map: adjust it in place.
    if let Some(idx) = panes.iter().position(|p| {
        p.kind() == PaneKind::Volume
            && p.volume()
                .is_some_and(|v| v.source_pane == Some(source_pane))
    }) {
        return Some(RegionDestination::Existing(idx));
    }
    // A 3D pane nobody aimed — converted from the menu, reset, or restored
    // with a dangling source index — before growing. It is showing the default
    // box, which the first drag is almost certainly trying to replace; a user
    // with exactly one such pane who gets a sibling instead has two 3D views
    // where they asked to aim one. A pane sourced from *another* map is
    // deliberately not matched here: it is that map's to re-aim, and growing
    // beats stealing it. The pane's *site* is no bar: the applier writes the
    // source map's site and moment onto whatever pane this rule answers with,
    // so a sourceless pane left on another site follows the map, rather than
    // resampling its own radar over this map's ground.
    if let Some(idx) = panes.iter().position(|p| {
        p.kind() == PaneKind::Volume && p.volume().is_some_and(|v| v.source_pane.is_none())
    }) {
        return Some(RegionDestination::Existing(idx));
    }
    // Room to open one beside the map. `>` rather than `>=`: the new pane is the
    // one at index `panes.len()`, so the count has to reach `panes.len() + 1`.
    if max_panes > panes.len() {
        return Some(RegionDestination::Grow(panes.len() + 1));
    }
    // Any 3D pane at all, even one aimed from somewhere else. Re-aiming beats
    // converting, because converting destroys a pane the user set up.
    if let Some(idx) = panes.iter().position(|p| p.kind() == PaneKind::Volume) {
        return Some(RegionDestination::Existing(idx));
    }
    // Spend the furthest pane from the one being drawn on, and never that one:
    // taking the map out from under the drag that just happened would leave the
    // user with no idea where the region they drew went.
    (0..panes.len())
        .rev()
        .find(|idx| *idx != source_pane)
        .map(RegionDestination::Convert)
        // A single-pane layout at its ceiling, which only a 1-pane width class
        // produces. The map has to be spent, because there is nothing else.
        .or(Some(RegionDestination::Convert(source_pane)))
}

#[cfg(test)]
mod tests;
