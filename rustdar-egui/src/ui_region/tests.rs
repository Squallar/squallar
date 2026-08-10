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
                    let destination =
                        destination_for(&panes, source, max_panes).unwrap_or_else(|| {
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
