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
mod tests {
    use super::*;

    fn point(lat: f64, lon: f64) -> GeoPoint {
        GeoPoint { lat, lon }
    }

    /// The press fixes a centre; the drag only ever sets the half-width.
    ///
    /// Pinned because the obvious alternative — corner-to-corner, with the centre
    /// recomputed each frame — is what a rectangle drag does, and it is what this
    /// would drift into. Under it the box would slide as it grew, and a user
    /// aiming at a storm would watch it walk off the centre they pressed on.
    #[test]
    fn the_press_fixes_the_centre_and_the_drag_only_grows_the_box() {
        let centre = point(35.3, -97.3);
        let mut drag = RegionDrag::begin(0, centre).expect("a point on Earth");
        drag.extend_to(point(35.6, -97.3));
        assert_eq!(drag.centre(), centre, "the centre must not move");
        let first = drag.half_width_km();
        drag.extend_to(point(36.0, -97.3));
        assert_eq!(drag.centre(), centre, "the centre must still not move");
        assert!(
            drag.half_width_km() > first,
            "dragging further must grow the box: {first} then {}",
            drag.half_width_km(),
        );
    }

    /// The half-width is the larger axis distance, not the diagonal.
    ///
    /// The mutation this closes is `east.max(north)` becoming a hypotenuse or a
    /// `min`. Both still produce a square that grows with the drag, so the pane
    /// looks right; what changes is where the edge sits relative to the pointer,
    /// which is the whole feel of the gesture. A diagonal drag is the only input
    /// that tells the three apart, so it is the one used here.
    #[test]
    fn the_half_width_is_the_larger_axis_and_the_edge_follows_the_pointer() {
        let centre = point(35.0, -97.0);
        // Roughly 55 km north and 18 km east, so the two axes are far apart.
        let corner = point(35.5, -96.8);
        let mut drag = RegionDrag::begin(0, centre).expect("a point on Earth");
        drag.extend_to(corner);

        let mut north_only = RegionDrag::begin(0, centre).expect("a point on Earth");
        north_only.extend_to(point(corner.lat, centre.lon));

        assert!(
            (drag.half_width_km() - north_only.half_width_km()).abs() < 0.5,
            "the larger axis alone must set the half-width: diagonal {} vs north {}",
            drag.half_width_km(),
            north_only.half_width_km(),
        );
    }

    /// A drag under the resampler's minimum commits nothing.
    ///
    /// The bar is `MIN_HALF_WIDTH_KM` and not a pixel count precisely because
    /// `build_voxels` *clamps* below it: a 3 km drag that committed would silently
    /// resample 10 km, and the pane's resolution readout would then describe a box
    /// the user never drew.
    #[test]
    fn a_drag_below_the_resamplers_minimum_commits_nothing() {
        let centre = point(35.3, -97.3);
        let min = rustdar_radar::voxel::MIN_HALF_WIDTH_KM;

        let mut tiny = RegionDrag::begin(0, centre).expect("a point on Earth");
        tiny.extend_to(point(
            centre.lat + (min * 0.5) / KM_PER_DEGREE_LAT,
            centre.lon,
        ));
        assert!(
            tiny.commit().is_none(),
            "a drag at half the minimum must be discarded, not clamped up",
        );

        let mut big = RegionDrag::begin(0, centre).expect("a point on Earth");
        big.extend_to(point(
            centre.lat + (min * 2.0) / KM_PER_DEGREE_LAT,
            centre.lon,
        ));
        let committed = big.commit().expect("a drag well over the minimum commits");
        assert!(
            (committed.half_width_km() - min * 2.0).abs() < 1.0,
            "a committed region must carry the half-width that was dragged, not a clamped one: {}",
            committed.half_width_km(),
        );
    }

    /// A drag past the resampler's maximum previews the box it will commit.
    ///
    /// The commit has always gone through `VolumeRegion::new`, which clamps the
    /// half-width to [`rustdar_radar::voxel::MAX_HALF_WIDTH_KM`] — so an
    /// uncapped drag would keep painting a bigger and bigger square past
    /// ~230 km while releasing the same box every time. The preview reads
    /// `half_width_km` straight off this struct, so the cap has to live in
    /// `extend_to` for what is drawn to be what is resampled.
    ///
    /// The corner is ~300 km out — nowhere near the clamp value itself — so a
    /// regression cannot pass by the chosen point coinciding with the cap.
    #[test]
    fn a_drag_past_the_resamplers_maximum_previews_the_box_it_commits() {
        let centre = point(35.0, -97.0);
        let max = rustdar_radar::voxel::MAX_HALF_WIDTH_KM;
        let mut drag = RegionDrag::begin(0, centre).expect("a point on Earth");
        drag.extend_to(point(centre.lat + 300.0 / KM_PER_DEGREE_LAT, centre.lon));
        assert_eq!(
            drag.half_width_km(),
            max,
            "a ~300 km drag must preview the {max} km box it will commit",
        );
        // Still at the stop further out: the control is wound to its end.
        drag.extend_to(point(centre.lat + 400.0 / KM_PER_DEGREE_LAT, centre.lon));
        assert_eq!(drag.half_width_km(), max, "the stop must hold further out");
        // And what was previewed is exactly what commits.
        assert_eq!(
            drag.commit()
                .expect("a maximal drag commits")
                .half_width_km(),
            max,
            "the previewed box and the committed box must be the same box",
        );
        // A stop, not a ratchet: a pointer that comes back inside shrinks the
        // box again.
        let mut back = RegionDrag::begin(0, centre).expect("a point on Earth");
        back.extend_to(point(centre.lat + 300.0 / KM_PER_DEGREE_LAT, centre.lon));
        back.extend_to(point(centre.lat + 100.0 / KM_PER_DEGREE_LAT, centre.lon));
        assert!(
            (back.half_width_km() - 100.0).abs() < 1.0,
            "the cap must not hold a drag that came back inside: {}",
            back.half_width_km(),
        );
    }

    /// A press that never moves commits nothing — the mis-click case.
    #[test]
    fn a_press_with_no_drag_commits_nothing() {
        let drag = RegionDrag::begin(0, point(35.3, -97.3)).expect("a point on Earth");
        assert_eq!(drag.half_width_km(), 0.0);
        assert!(drag.commit().is_none());
    }

    /// A press the projector could not place is refused rather than laundered.
    #[test]
    fn a_press_off_the_earth_starts_no_drag() {
        for bad in [f64::NAN, f64::INFINITY, 1e9, -95.0] {
            assert!(
                RegionDrag::begin(0, point(bad, -97.3)).is_none(),
                "latitude {bad} must not start a drag",
            );
        }
        assert!(RegionDrag::begin(0, point(35.3, 1e9)).is_none());
    }

    /// A pointer that leaves the Earth mid-drag leaves the box where it was.
    ///
    /// Without the guard the NaN would reach `half_width_km` and stick for the
    /// rest of the drag — and then `VolumeRegion::new` would refuse the commit, so
    /// the symptom is a drag that draws normally and silently does nothing on
    /// release.
    #[test]
    fn a_non_finite_corner_leaves_the_drag_alone() {
        let centre = point(35.3, -97.3);
        let mut drag = RegionDrag::begin(0, centre).expect("a point on Earth");
        drag.extend_to(point(35.8, -97.3));
        let good = drag.half_width_km();
        assert!(good > 0.0, "precondition: the drag has a size");
        drag.extend_to(point(f64::NAN, -97.3));
        assert_eq!(
            drag.half_width_km(),
            good,
            "a NaN corner must change nothing"
        );

        // Finite and nonsense, which an `is_finite` check alone would let
        // straight through: `lat: 1e9` walks a perfectly well-defined great
        // circle over nowhere and would set a half-width of millions of
        // kilometres — capped by `extend_to` to the 230 km maximum. The user
        // would drag two centimetres, watch the whole surveillance range light
        // up, and release a box nobody asked for.
        drag.extend_to(point(1e9, -97.3));
        assert_eq!(
            drag.half_width_km(),
            good,
            "a finite-but-absurd corner must change nothing either",
        );
        drag.extend_to(point(35.8, 1e9));
        assert_eq!(drag.half_width_km(), good);
    }

    /// The box is square in kilometres, not in degrees.
    ///
    /// At 35°N a degree of longitude is 82 km against latitude's 111, so a box
    /// built with the same delta on both axes would be 26% narrow. The mutation
    /// this closes is dropping the `cos(lat)` divisor, which produces a box that
    /// looks plausible on screen and resamples ground the user did not select.
    #[test]
    fn the_box_is_square_in_kilometres_rather_than_in_degrees() {
        let centre = point(35.0, -97.0);
        let (nw, se) = corners_for(centre, 80.0).expect("a temperate latitude has corners");
        let lat_span = nw.lat - se.lat;
        let lon_span = se.lon - nw.lon;
        assert!(
            lon_span > lat_span * 1.15,
            "a square in km must span more longitude than latitude at 35°N: \
             {lon_span} vs {lat_span}",
        );
        // And it really is square on the ground: both spans, converted, agree.
        let (_, north_km) =
            rustdar_radar::beam::site_bearing_range_km(centre.lat, centre.lon, nw.lat, centre.lon);
        let (_, east_km) =
            rustdar_radar::beam::site_bearing_range_km(centre.lat, centre.lon, centre.lat, se.lon);
        assert!(
            (north_km - east_km).abs() < 2.0,
            "the box must be square on the ground: {north_km} km north vs {east_km} km east",
        );
    }

    /// The poles have no square, and answering `None` is what keeps an infinity
    /// out of the painter.
    #[test]
    fn a_polar_centre_has_no_drawable_square() {
        assert!(corners_for(point(90.0, 0.0), 80.0).is_none());
        assert!(corners_for(point(-90.0, 0.0), 80.0).is_none());
    }

    // --- The destination rule ----------------------------------------------

    fn map_pane() -> PaneState {
        PaneState::with_site("KTLX".to_owned())
    }

    fn volume_pane(source: Option<usize>) -> PaneState {
        volume_pane_on("KTLX", source)
    }

    /// A 3D pane sitting on `site` — not necessarily the map's own.
    fn volume_pane_on(site: &str, source: Option<usize>) -> PaneState {
        let mut pane = PaneState::with_site(site.to_owned());
        pane.set_kind(PaneKind::Volume);
        if let Some(volume) = pane.volume_mut() {
            volume.source_pane = source;
        }
        pane
    }

    /// A second drag on the same map re-aims the pane already sourced from it.
    ///
    /// The common case by a distance: a user drags a box, sees it was slightly
    /// off, and drags again. Opening a second 3D pane for that would be a layout
    /// change nobody asked for, and it would happen on every correction.
    #[test]
    fn a_second_drag_on_the_same_map_re_aims_the_pane_it_already_feeds() {
        let panes = [map_pane(), volume_pane(Some(0)), volume_pane(Some(9))];
        assert_eq!(
            destination_for(&panes, 0, 6),
            Some(RegionDestination::Existing(1)),
            "the pane sourced from this map wins, even with room to grow",
        );
    }

    /// The first drag on a layout with room opens a 3D pane beside the map,
    /// rather than replacing it.
    ///
    /// Growing has to beat re-aiming some *other* pane, or a single-map layout's
    /// first drag would convert the map being drawn on and the user would lose
    /// the thing they were aiming with.
    #[test]
    fn the_first_drag_grows_the_layout_when_there_is_room() {
        let panes = [map_pane()];
        assert_eq!(
            destination_for(&panes, 0, 4),
            Some(RegionDestination::Grow(2)),
        );
    }

    /// A sourceless 3D pane is re-aimed rather than a sibling grown.
    ///
    /// A pane converted from the menu, reset, or restored with a dangling
    /// source index carries `source_pane: None` — no map feeds it — so the
    /// sourced-from-this-map arm skips it, and without this arm the first drag
    /// would *grow*: a user with exactly one such 3D pane who dragged a box
    /// would get a surprise second one. Re-aiming the pane nobody owns is what
    /// they meant.
    ///
    /// `max_panes` leaves room to grow on purpose: with the layout full this
    /// case is indistinguishable from the any-3D-pane fallback, and the arm
    /// being pinned is the one that runs *while growing is still possible*.
    #[test]
    fn a_sourceless_3d_pane_is_re_aimed_rather_than_a_sibling_grown() {
        let panes = [map_pane(), volume_pane(None)];
        assert_eq!(
            destination_for(&panes, 0, 6),
            Some(RegionDestination::Existing(1)),
            "a restored 3D pane must be re-aimed, not given a sibling",
        );

        // The first *sourceless* pane, not the first 3D pane: a pane another
        // map feeds sits at a lower index and must be passed over.
        let panes = [map_pane(), volume_pane(Some(9)), volume_pane(None)];
        assert_eq!(
            destination_for(&panes, 0, 6),
            Some(RegionDestination::Existing(2)),
            "a pane sourced from another map is that map's to re-aim",
        );

        // And the sourced-from-this-map arm still wins over it: adjusting the
        // box you already dragged must keep re-aiming the pane it feeds.
        let panes = [map_pane(), volume_pane(None), volume_pane(Some(0))];
        assert_eq!(
            destination_for(&panes, 0, 6),
            Some(RegionDestination::Existing(2)),
            "the pane this map already feeds outranks a sourceless one",
        );
    }

    /// A sourceless 3D pane on *another site* is still the one re-aimed, with
    /// room to grow — the rule is site-blind on purpose, because the applier
    /// re-sites whatever pane it answers with.
    ///
    /// This is the layout this family was missing: a KTLX map beside a
    /// sourceless KICT pane. With the rule alone in view, "re-aim" here read as
    /// leaving the pane resampling **KICT's** volume over a box centred on
    /// KTLX's ground ~220 km away — an empty or sliver grid, captioned KICT.
    /// The contract is split: this arm keeps answering `Existing` so the
    /// re-aim stays useful across sites instead of quietly growing a sibling,
    /// and `Gui::apply_pending_region` writes the source map's site and moment
    /// onto the pane, exactly as the section applier does — pinned by
    /// `a_retargeted_3d_pane_takes_the_maps_site_and_moment` in `ui`.
    #[test]
    fn a_sourceless_pane_on_another_site_is_still_the_one_re_aimed() {
        let panes = [map_pane(), volume_pane_on("KICT", None)];
        assert_eq!(
            destination_for(&panes, 0, 6),
            Some(RegionDestination::Existing(1)),
            "a cross-site sourceless pane is re-aimed — the applier moves it \
             to this map's site",
        );
    }

    /// A 3D pane sourced from *another* map does not block growing.
    ///
    /// The sourceless arm matches `source_pane: None` and nothing else. Widened
    /// to "any 3D pane before growing" it would steal a view another map is
    /// feeding — and this is the layout that tells those two rules apart while
    /// there is still room to grow.
    #[test]
    fn a_pane_sourced_from_another_map_does_not_block_growing() {
        let panes = [map_pane(), map_pane(), volume_pane(Some(1))];
        assert_eq!(
            destination_for(&panes, 0, 6),
            Some(RegionDestination::Grow(4)),
            "another map's 3D pane must be left alone while there is room",
        );
    }

    /// At the layout's ceiling, an existing 3D pane is re-aimed rather than
    /// another pane converted.
    ///
    /// Converting destroys a pane the user set up; re-aiming costs a rebuild.
    /// The mutation this closes is ordering these two the other way round, which
    /// still produces a working 3D view and quietly eats a map.
    #[test]
    fn at_the_ceiling_an_existing_3d_pane_beats_converting_one() {
        let panes = [map_pane(), map_pane(), volume_pane(Some(9))];
        assert_eq!(
            destination_for(&panes, 0, 3),
            Some(RegionDestination::Existing(2)),
        );
    }

    /// With no 3D pane and no room, the furthest pane is converted — and never
    /// the map the region was drawn on.
    ///
    /// Taking the map out from under the drag that just happened would leave the
    /// user with no idea where the region they drew went, and no map to draw the
    /// next one on.
    #[test]
    fn the_last_resort_converts_the_furthest_pane_and_never_the_source() {
        let panes = [map_pane(), map_pane(), map_pane()];
        assert_eq!(
            destination_for(&panes, 0, 3),
            Some(RegionDestination::Convert(2)),
        );
        // Drawn on the furthest pane: the next one down is spent instead.
        assert_eq!(
            destination_for(&panes, 2, 3),
            Some(RegionDestination::Convert(1)),
        );
    }

    /// The rule is total: there is no layout on which a completed drag silently
    /// does nothing — and every answer it gives is one the applier can act on.
    ///
    /// A gesture that finishes and produces no visible change is
    /// indistinguishable from one the app failed to receive, which is the worst
    /// outcome available — the user repeats it, and nothing happens again.
    ///
    /// Four kinds per pane rather than two, so every arm of the rule is
    /// reached: a map, a sourceless 3D pane (the arm that re-aims a restored
    /// pane instead of growing), one sourced from pane 0 (the
    /// sourced-from-this-map arm whenever 0 is the source), and one sourced
    /// from a pane index no layout here reaches (the any-3D fallback).
    ///
    /// The well-formedness half is what stops totality being satisfied
    /// vacuously: `Existing` must name a 3D pane in the layout, `Grow` must ask
    /// for exactly one more pane and only when the ceiling allows it, and
    /// `Convert` must name a pane that exists.
    ///
    /// Every layout is enumerated **twice**: once with every pane on one site,
    /// and once with sites alternating by index — and the two answers must be
    /// the same. The rule is site-blind by contract, because siting the chosen
    /// pane is `Gui::apply_pending_region`'s job; a rule that consulted sites
    /// would silently change which arm fires for exactly the layouts a
    /// single-site sweep never generates, which is how the cross-site
    /// sourceless case went unwatched.
    #[test]
    fn every_layout_has_somewhere_to_put_a_region() {
        for max_panes in 1..=6usize {
            for count in 1..=max_panes {
                for kinds in 0..(4u32.pow(count as u32)) {
                    let build = |site_diverse: bool| -> Vec<PaneState> {
                        (0..count)
                            .map(|i| {
                                let site = if site_diverse && i % 2 == 1 {
                                    "KICT"
                                } else {
                                    "KTLX"
                                };
                                match (kinds >> (2 * i)) & 0b11 {
                                    0 => PaneState::with_site(site.to_owned()),
                                    1 => volume_pane_on(site, None),
                                    2 => volume_pane_on(site, Some(0)),
                                    _ => volume_pane_on(site, Some(9)),
                                }
                            })
                            .collect()
                    };
                    let panes = build(false);
                    let diverse = build(true);
                    for source in 0..count {
                        let destination = destination_for(&panes, source, max_panes)
                            .unwrap_or_else(|| {
                                panic!(
                                    "no destination for {count} panes (kinds {kinds:b}), \
                                     source {source}, ceiling {max_panes}",
                                )
                            });
                        let context = format!(
                            "{count} panes (kinds {kinds:b}), source {source}, \
                             ceiling {max_panes}",
                        );
                        assert_eq!(
                            destination_for(&diverse, source, max_panes),
                            Some(destination),
                            "the rule must be site-blind — siting the pane is \
                             the applier's job: {context}",
                        );
                        match destination {
                            RegionDestination::Existing(idx) => assert!(
                                panes.get(idx).map(PaneState::kind) == Some(PaneKind::Volume),
                                "Existing({idx}) does not name a 3D pane: {context}",
                            ),
                            RegionDestination::Grow(new_count) => assert!(
                                new_count == count + 1 && new_count <= max_panes,
                                "Grow({new_count}) is not one new pane within the \
                                 ceiling: {context}",
                            ),
                            RegionDestination::Convert(idx) => assert!(
                                idx < count,
                                "Convert({idx}) names a pane that does not exist: \
                                 {context}",
                            ),
                        }
                    }
                }
            }
        }
    }

    /// A single-pane layout at its ceiling has to spend the map, because there
    /// is nothing else — the one case where the source pane is converted.
    #[test]
    fn a_one_pane_ceiling_spends_the_only_pane_there_is() {
        let panes = [map_pane()];
        assert_eq!(
            destination_for(&panes, 0, 1),
            Some(RegionDestination::Convert(0)),
        );
    }
}
