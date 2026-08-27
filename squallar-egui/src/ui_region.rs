//! A 3D pane's region: the one gesture that picks one, and the two things a
//! picked or defaulted region then implies — the viewport its floor is drawn
//! through, and the zoom gesture that must not touch either.

/// The armed region pick's yellow: the box in flight, the resolution hint over
/// its top edge, and the active pane's armed hint chip all paint in this one
/// colour — so the chip advertises exactly the box the drag will draw.
pub(crate) const REGION_ARM_COLOR: egui::Color32 = egui::Color32::from_rgb(255, 220, 120);

/// A region drag in flight.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RegionDrag {
    /// Which map pane the press landed on. A drag belongs to one pane for its
    /// whole life; the pointer leaving that pane's rect does not end it, because
    /// dragging past the edge of a pane to make a big box is ordinary.
    pane_idx: crate::pane::PaneId,
    /// The box's centre, fixed on the press frame and never revised.
    centre: squallar_geo::GeoPoint,
    /// Half-width in kilometres as the pointer currently stands. Capped at the
    /// resampler's maximum on the way in — see [`Self::extend_to`] — but *not*
    /// held up to its minimum: a too-small drag is refused whole at commit
    /// rather than resized. Zero until the pointer moves.
    half_width_km: f64,
}

impl RegionDrag {
    /// Start a drag centred on `centre`.
    pub(crate) fn begin(
        pane_idx: crate::pane::PaneId,
        centre: squallar_geo::GeoPoint,
    ) -> Option<Self> {
        centre.is_on_earth().then_some(Self {
            pane_idx,
            centre,
            half_width_km: 0.0,
        })
    }

    /// Which pane this drag belongs to.
    pub(crate) fn pane_idx(self) -> crate::pane::PaneId {
        self.pane_idx
    }

    /// The centre the press fixed.
    pub(crate) fn centre(self) -> squallar_geo::GeoPoint {
        self.centre
    }

    /// Half-width as it currently stands, kilometres.
    pub(crate) fn half_width_km(self) -> f64 {
        self.half_width_km
    }

    /// Re-measure the half-width against a pointer now over `corner`.
    pub(crate) fn extend_to(&mut self, corner: squallar_geo::GeoPoint) {
        if !corner.is_on_earth() {
            return;
        }
        let (bearing_deg, range_km) = squallar_geo::site_bearing_range_km(
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
            self.half_width_km = half.min(squallar_radar::voxel::MAX_HALF_WIDTH_KM);
        }
    }

    /// The region this drag would commit, or `None` if it is too small to be one.
    pub(crate) fn commit(self) -> Option<crate::pane::VolumeRegion> {
        (self.half_width_km >= squallar_radar::voxel::MIN_HALF_WIDTH_KM)
            .then(|| {
                crate::pane::VolumeRegion::new(
                    self.centre,
                    squallar_radar::voxel::HalfExtentKm::square(self.half_width_km),
                )
            })
            .flatten()
    }
}

/// A box's corners as geographic points, `(north-west, south-east)`.
pub(crate) fn corners_for(
    centre: squallar_geo::GeoPoint,
    half: squallar_radar::voxel::HalfExtentKm,
) -> Option<(squallar_geo::GeoPoint, squallar_geo::GeoPoint)> {
    let d_lat = half.north_km / squallar_geo::KM_PER_DEGREE_LAT;
    let cos_lat = centre.lat.to_radians().cos();
    if !(cos_lat.is_finite() && cos_lat.abs() > 1e-6) {
        return None;
    }
    let d_lon = half.east_km / (squallar_geo::KM_PER_DEGREE_LAT * cos_lat);
    let nw = squallar_geo::GeoPoint {
        lat: centre.lat + d_lat,
        lon: centre.lon - d_lon,
    };
    let se = squallar_geo::GeoPoint {
        lat: centre.lat - d_lat,
        lon: centre.lon + d_lon,
    };
    (d_lat.is_finite() && d_lon.is_finite()).then_some((nw, se))
}

/// `walkers::Map::zoom_speed`'s default, which the plan view leaves alone.
const ZOOM_SPEED: f64 = 2.0;

/// Points of wheel travel worth one Web Mercator zoom level.
const POINTS_PER_ZOOM_LEVEL: f64 = 120.0;

/// This frame's geography zoom, in zoom **levels**, from whatever device
/// produced one.
fn zoom_step(input: &egui::InputState) -> f64 {
    let mut delta = f64::from(input.zoom_delta());
    if delta == 1.0 {
        delta = 1.0 + f64::from(input.smooth_scroll_delta.y) / (POINTS_PER_ZOOM_LEVEL * ZOOM_SPEED);
    }
    (delta - 1.0) * ZOOM_SPEED
}

/// This frame's scroll or pinch as a multiplicative dolly for
/// [`OrbitDelta::zoom_factor`](crate::pane::OrbitDelta::zoom_factor), or `1.0`
/// — "the eye did not move" — for a frame with no gesture for this pane.
pub(crate) fn zoom_camera(ctx: &egui::Context, response: &egui::Response) -> f32 {
    if !(response.hovered() || response.dragged()) || !pointer_on_map_layer(ctx) {
        return 1.0;
    }
    dolly_for_step(ctx.input(zoom_step))
}

/// [`zoom_camera`]'s arithmetic, without the gate: zoom levels in, a standoff
/// divisor out.
fn dolly_for_step(step: f64) -> f32 {
    if !step.is_finite() || step == 0.0 {
        return 1.0;
    }
    let factor = step.exp2() as f32;
    if factor.is_finite() && factor > 0.0 {
        factor
    } else {
        1.0
    }
}

/// How many passes [`viewport_for_region`] gets to settle, and the number the
/// loop really can stop short of.
const MAX_FRAMING_PASSES: usize = 8;

/// How much more than the box the framing deliberately covers, as a fraction of
/// it.
const COVERAGE_MARGIN: f64 = 0.001;

/// What the solve **aims** at, as a fraction of the box — the ceiling of the
/// band whose floor is [`COVERAGE_MARGIN`].
const COVERAGE_TARGET: f64 = 2.0 * COVERAGE_MARGIN;

/// The loop's settle test, in the units it has to hand.
const SETTLE_SHORTFALL: f64 = (1.0 + COVERAGE_TARGET) / (1.0 + COVERAGE_MARGIN);

/// The widest zoom `walkers::MapMemory::set_zoom` will accept: the whole world
/// in one 256-point tile.
///
/// Restated here rather than imported because walkers does not export it.
/// `Zoom::try_from` is
///
/// ```text
/// if !(0. ..=26.).contains(&value) { Err(InvalidZoom) } else { Ok(Self(value)) }
/// ```
///
/// — `walkers-0.56.0/src/zoom.rs:14` — and `Zoom` is `pub(crate)` to walkers, so
/// `set_zoom`'s `Err(InvalidZoom)` is the only way a caller can learn where the
/// bounds are. [`a_zoom_walkers_refuses_is_one_this_module_clamps_away`] drives
/// `set_zoom` across both ends and fails if walkers ever moves them, which is
/// what keeps this pair honest across a version bump.
///
/// **`RangeInclusive::contains` is false for a `NaN`**, so a `NaN` zoom is
/// refused too — and a `NaN` survives `f64::clamp` rather than being bounded
/// away by it. That is why [`viewport_for_region`]'s finiteness test comes
/// *before* the clamp and not after.
///
/// [`a_zoom_walkers_refuses_is_one_this_module_clamps_away`]:
///     tests::a_zoom_walkers_refuses_is_one_this_module_clamps_away
const MIN_ZOOM_LEVEL: f64 = 0.0;

/// The tightest zoom `walkers::MapMemory::set_zoom` will accept. See
/// [`MIN_ZOOM_LEVEL`] for where both numbers come from; walkers' own comment
/// calls this end "artificial".
const MAX_ZOOM_LEVEL: f64 = 26.0;

/// A map viewport that frames `rect` on the box of `half` about `centre`.
pub(crate) fn viewport_for_region(
    rect: egui::Rect,
    centre: walkers::Position,
    half: squallar_radar::voxel::HalfExtentKm,
) -> Option<walkers::MapMemory> {
    Some(solve_viewport(rect, centre, half)?.0)
}

/// [`viewport_for_region`]'s whole body, plus **how many passes it took**.
fn solve_viewport(
    rect: egui::Rect,
    centre: walkers::Position,
    half: squallar_radar::voxel::HalfExtentKm,
) -> Option<(walkers::MapMemory, usize)> {
    if !(rect.width() > 0.0 && rect.height() > 0.0) {
        return None;
    }
    if !(half.is_finite() && half.east_km > 0.0 && half.north_km > 0.0) {
        return None;
    }

    let want = squallar_radar::voxel::HalfExtentKm {
        east_km: half.east_km * (1.0 + COVERAGE_TARGET),
        north_km: half.north_km * (1.0 + COVERAGE_TARGET),
    };

    let mut memory = walkers::MapMemory::default();
    for pass in 1..=MAX_FRAMING_PASSES {
        let covered = ground_half_extent(rect, &memory, centre)?;
        let shortfall = (want.east_km / covered.east_km).max(want.north_km / covered.north_km);
        if !shortfall.is_finite() || shortfall <= 0.0 {
            return None;
        }
        if shortfall <= SETTLE_SHORTFALL {
            return Some((memory, pass));
        }
        let target = (memory.zoom() - shortfall.log2()).clamp(MIN_ZOOM_LEVEL, MAX_ZOOM_LEVEL);
        if target == memory.zoom() {
            return Some((memory, pass));
        }
        if memory.set_zoom(target).is_err() {
            return Some((memory, pass));
        }
    }
    Some((memory, MAX_FRAMING_PASSES))
}

/// The ground `rect` covers either side of `centre`, kilometres on each
/// horizontal axis, through the projection the strip is drawn in.
pub(crate) fn ground_half_extent(
    rect: egui::Rect,
    map_memory: &walkers::MapMemory,
    centre: walkers::Position,
) -> Option<squallar_radar::voxel::HalfExtentKm> {
    /// Half a turn of longitude: the most ground there is either side of any
    /// meridian, and so the most an offset can honestly measure.
    const HALF_TURN_DEG: f64 = 180.0;

    let projector = walkers::Projector::new(rect, map_memory, centre);
    let ground_km = |pos: egui::Pos2| {
        let point = projector.unproject(pos.to_vec2());
        let lon = centre.x() + (point.x() - centre.x()).clamp(-HALF_TURN_DEG, HALF_TURN_DEG);
        let (_, range_km) =
            squallar_geo::site_bearing_range_km(centre.y(), centre.x(), point.y(), lon);
        (range_km.is_finite() && range_km > 0.0).then_some(range_km)
    };
    Some(squallar_radar::voxel::HalfExtentKm {
        east_km: ground_km(egui::pos2(rect.left(), rect.center().y))?
            .min(ground_km(egui::pos2(rect.right(), rect.center().y))?),
        north_km: ground_km(egui::pos2(rect.center().x, rect.top()))?
            .min(ground_km(egui::pos2(rect.center().x, rect.bottom()))?),
    })
}

/// Whether the pointer is over the map rather than over floating chrome.
pub(crate) fn pointer_on_map_layer(ctx: &egui::Context) -> bool {
    ctx.pointer_latest_pos().is_some_and(|pos| {
        !ctx.layer_id_at(pos)
            .is_some_and(|l| l.order > egui::Order::Background)
    })
}

#[cfg(test)]
mod tests;
