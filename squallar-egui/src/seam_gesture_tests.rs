//! **Every map gesture starts past the antimeridian, and the same ground gives
//! the same answer however the user got there.**
//!
//! The map works in a *continuous* longitude: the centre runs past ±180 as it
//! is panned across the seam and `walkers::Projector::unproject` folds nothing,
//! so a press out there hands the gesture 190° or 550° rather than −170°. The
//! things a gesture *stores* — a region's centre, a section's two ends, an
//! offline area's centre — are written in the ±180 frame every datum is written
//! in, so each carries its screen-derived position across with
//! [`squallar_geo::GeoPoint::on_earth`] before it stores it.
//!
//! The gate is the equivalence, not the acceptance. Accepting the press and
//! keeping the continuous frame would pass an "it starts" test and then give a
//! different download, a different resample box and a different cut for the
//! same patch of ground depending on which way the user panned to reach it. So
//! every test here compares a gesture past the seam against the identical
//! gesture a whole number of turns back, and every comparison is exact: no
//! tolerance to loosen, and a fold applied twice or not at all moves a whole
//! world rather than a last bit.

use crate::pane::{SectionLine, VolumeRegion};
use crate::ui_download_area::PickedBox;
use crate::ui_region::{DragBoundsKm, RegionDrag, corners_for};
use squallar_geo::GeoPoint;

/// A latitude a press can land on, well clear of the poles so a box about it
/// has corners.
const LAT: f64 = 35.3331;

/// Pairs of `(as the pane hands it over, the ±180 spelling of the same
/// meridian)`.
///
/// **Every longitude here is exact in `f64` and so is its turn shift** —
/// `the_fixtures_name_one_meridian_twice` checks that rather than assuming it,
/// because a value like `−97.2778` comes back a turn out one ulp light and
/// would turn every exact comparison below into a tolerance nobody derived.
/// One and several turns each way, and a pair straddling the seam itself.
const SEAM_PAIRS: [(f64, f64); 8] = [
    (190.0, -170.0),
    (-190.0, 170.0),
    (180.5, -179.5),
    (-180.5, 179.5),
    (550.0, -170.0),
    (-550.0, 170.0),
    (910.0, -170.0),
    (-909.5, 170.5),
];

/// Positions no fold can rescue, in either coordinate. Latitude does not wrap,
/// so 95° N is a bug rather than a place a turn away.
const NOT_A_PLACE: [GeoPoint; 5] = [
    GeoPoint {
        lat: f64::NAN,
        lon: 179.0,
    },
    GeoPoint {
        lat: 95.0,
        lon: 179.0,
    },
    GeoPoint {
        lat: -90.001,
        lon: 179.0,
    },
    GeoPoint {
        lat: LAT,
        lon: f64::NAN,
    },
    GeoPoint {
        lat: LAT,
        lon: f64::INFINITY,
    },
];

fn at(lon: f64) -> GeoPoint {
    GeoPoint { lat: LAT, lon }
}

fn voxel_bounds() -> DragBoundsKm {
    DragBoundsKm {
        min_half_width_km: squallar_radar::voxel::MIN_HALF_WIDTH_KM,
        max_half_width_km: squallar_radar::voxel::MAX_HALF_WIDTH_KM,
    }
}

/// One drag, pressed at `press` and released over `corner`, as the two arms
/// that use [`RegionDrag`] would run it.
fn drag(press: GeoPoint, corner: GeoPoint, bounds: DragBoundsKm) -> Option<(GeoPoint, f64)> {
    let mut drag = RegionDrag::begin(0, press, bounds)?;
    drag.extend_to(corner);
    drag.commit()
}

/// **The fixtures are what they claim to be.** A pair whose fold is not exact
/// would make every `assert_eq!` below a statement about `f64` addition instead
/// of about the gestures.
#[test]
fn the_fixtures_name_one_meridian_twice() {
    for (far, near) in SEAM_PAIRS {
        assert_eq!(
            squallar_geo::normalize_lon(far),
            near,
            "{far} folds to {}, not to the {near} this table pairs it with",
            squallar_geo::normalize_lon(far),
        );
        assert!(
            !at(far).is_on_earth(),
            "{far} is inside the ±180 frame already, so it gates nothing",
        );
        assert!(
            at(near).is_on_earth(),
            "{near} is not a spelling of a place"
        );
    }
}

// ── The press: every gesture starts out there ────────────────────────────

/// **A region press past the seam starts the drag it starts a turn back**, and
/// commits the same box: same centre, same half-width, same corners.
#[test]
fn a_region_drag_past_the_seam_commits_the_box_it_commits_a_turn_back() {
    for (far, near) in SEAM_PAIRS {
        // One patch of ground for the corner, given the same way to both drags:
        // what is under test is the frame the *press* arrived in.
        for (corner_lat, corner_lon) in [(LAT + 0.9, near + 0.9), (LAT - 1.5, near - 1.1)] {
            let corner = GeoPoint {
                lat: corner_lat,
                lon: corner_lon,
            };
            let out_there = drag(at(far), corner, voxel_bounds())
                .unwrap_or_else(|| panic!("a press at {far}° started no drag at all"));
            let back_home = drag(at(near), corner, voxel_bounds())
                .unwrap_or_else(|| panic!("a press at {near}° started no drag"));

            assert_eq!(
                out_there, back_home,
                "a drag pressed at {far}° committed {out_there:?} where the same \
                 drag pressed at {near}° committed {back_home:?} — one patch of \
                 ground, two boxes",
            );
        }
    }
}

/// **What a gesture stores is in the frame every datum is written in.** The
/// press is folded, not merely tolerated: a stored 190° would draw, download and
/// resample somewhere no other part of the app can find.
#[test]
fn a_stored_gesture_position_satisfies_the_strict_predicate() {
    for (far, near) in SEAM_PAIRS {
        let corner = GeoPoint {
            lat: LAT + 0.9,
            lon: near + 0.9,
        };
        let (centre, _) = drag(at(far), corner, voxel_bounds()).expect("a drag past the seam");
        assert_eq!(
            centre.lon, near,
            "a press at {far}° stored {}, not the {near} the ±180 frame spells \
             that meridian",
            centre.lon,
        );
        assert!(
            centre.is_on_earth(),
            "a press at {far}° stored a longitude the data frame refuses",
        );

        let region = VolumeRegion::new(centre, squallar_radar::voxel::HalfExtentKm::square(120.0))
            .expect("a resample box about a folded centre");
        assert!(region.centre().is_on_earth());
        let picked = PickedBox::new(centre, 40.0).expect("an offline box about a folded centre");
        assert!(picked.centre.is_on_earth());
    }
}

/// **A pointer that crosses the seam mid-drag keeps measuring.** The press was
/// legal, and refusing the corner froze the box at whatever width it had when
/// the pointer reached 180° — a gesture that stops halfway with no way to say so.
#[test]
fn a_corner_carried_past_the_seam_measures_the_box_it_measures_folded() {
    // 179.5° and −179.5° are one degree apart across the seam, and both they and
    // their turn shifts are exact in `f64`.
    let press = at(179.5);
    for turns in [-2.0, -1.0, 1.0, 2.0] {
        let folded = GeoPoint {
            lat: LAT + 0.6,
            lon: -179.5,
        };
        let out_there = GeoPoint {
            lat: folded.lat,
            lon: folded.lon + 360.0 * turns,
        };
        let carried = drag(press, out_there, voxel_bounds())
            .unwrap_or_else(|| panic!("a corner at {}° ended the drag", out_there.lon));
        let home = drag(press, folded, voxel_bounds()).expect("a corner just over the seam");
        assert_eq!(
            carried, home,
            "a pointer reported at {}° measured a different box from the same \
             pointer reported at {}°",
            out_there.lon, folded.lon,
        );
    }
}

/// **A cross-section drawn past the seam is the line drawn a turn back**, and
/// each end is folded on its own: a line drawn *across* the seam is the same
/// line as the one whose far end is spelled the other way.
#[test]
fn a_section_line_past_the_seam_is_the_line_drawn_a_turn_back() {
    for (far, near) in SEAM_PAIRS {
        let out_there = SectionLine::new(
            at(far),
            GeoPoint {
                lat: LAT + 1.2,
                lon: far + 0.75,
            },
        )
        .unwrap_or_else(|| panic!("a section drawn at {far}° was refused"));
        let back_home = SectionLine::new(
            at(near),
            GeoPoint {
                lat: LAT + 1.2,
                lon: near + 0.75,
            },
        )
        .expect("the same section a turn back");
        assert_eq!(
            out_there.a(),
            back_home.a(),
            "the A ends of one line disagree: {far}° gave {:?}, {near}° gave {:?}",
            out_there.a(),
            back_home.a(),
        );
        assert_eq!(
            out_there.b(),
            back_home.b(),
            "the B ends of one line disagree"
        );
        assert!(out_there.a().is_on_earth() && out_there.b().is_on_earth());
    }

    // Across the seam itself, where the two ends fold opposite ways. Folding the
    // pair as one shift — the rule for a *rect* being placed on a pane — would
    // leave the far end out of the frame and refuse the line.
    let across = SectionLine::new(
        at(179.5),
        GeoPoint {
            lat: LAT,
            lon: 180.5,
        },
    )
    .expect("a section drawn across the antimeridian");
    let spelled = SectionLine::new(
        at(179.5),
        GeoPoint {
            lat: LAT,
            lon: -179.5,
        },
    )
    .expect("the same two places, spelled inside the frame");
    assert_eq!(
        (across.a(), across.b()),
        (spelled.a(), spelled.b()),
        "179.5° → 180.5° and 179.5° → −179.5° are the same two places and were \
         stored as different lines",
    );
}

/// **An offline area picked past the seam is the same download.** The area's id
/// is a quantised centre, and two spellings of one meridian would resume as two
/// downloads of one town.
#[test]
fn an_offline_area_past_the_seam_is_the_same_download() {
    for (far, near) in SEAM_PAIRS {
        let out_there =
            PickedBox::new(at(far), 40.0).unwrap_or_else(|| panic!("a box at {far}° was refused"));
        let back_home = PickedBox::new(at(near), 40.0).expect("the same box a turn back");
        assert_eq!(out_there, back_home, "one town picked twice gave two boxes");

        for level in crate::basemap_areas::DETAIL_LEVELS {
            let a = out_there.area_spec(14, level).expect("a box has a bbox");
            let b = back_home.area_spec(14, level).expect("a box has a bbox");
            assert_eq!(
                a, b,
                "the download cut at {far}° asks for {a:?} where the one cut at \
                 {near}° asks for {b:?}",
            );
        }
    }
}

/// **The box the gesture commits covers the ground the gesture described**, at
/// the seam as anywhere else — stated against the corner the pointer was over,
/// never against a remembered number.
#[test]
fn the_committed_box_reaches_the_corner_the_pointer_was_over() {
    for (far, near) in SEAM_PAIRS {
        let corner = GeoPoint {
            lat: LAT + 1.0,
            lon: near + 1.4,
        };
        let (centre, half_km) =
            drag(at(far), corner, voxel_bounds()).expect("a drag past the seam");

        // What the drag measures is the larger of the corner's two ground
        // components, so the committed half-width is that and the box's own
        // corners reach it. Measured from the corner **as the drag stores it**:
        // `site_bearing_range_km` is periodic in the longitude difference, so a
        // corner a turn out is the same ground, but only to the last bit — and
        // this claim is exact.
        let corner = corner.on_earth().expect("a corner over the ground");
        let (bearing_deg, range_km) =
            squallar_geo::site_bearing_range_km(centre.lat, centre.lon, corner.lat, corner.lon);
        let bearing = bearing_deg.to_radians();
        let want = (range_km * bearing.sin())
            .abs()
            .max((range_km * bearing.cos()).abs());
        assert_eq!(
            half_km, want,
            "a drag pressed at {far}° over a corner {range_km:.1} km away \
             committed {half_km} km of half-width",
        );

        let half = squallar_radar::voxel::HalfExtentKm::square(half_km);
        let (nw, se) = corners_for(centre, half).expect("a box away from the poles");
        assert!(
            se.lon - nw.lon > 0.0 && nw.lat - se.lat > 0.0,
            "the box committed at {far}° came back inside out: {nw:?} .. {se:?}",
        );
    }
}

// ── The other arm: what a fold must NOT start accepting ──────────────────

/// **A fold is not a licence.** A position no turn can rescue still starts
/// nothing, in every gesture — over-acceptance here is the worse direction,
/// because a `NaN` centre commits a box that draws nowhere and downloads
/// nothing while every check upstream reads green.
#[test]
fn a_position_no_turn_can_rescue_still_starts_nothing() {
    for bad in NOT_A_PLACE {
        assert_eq!(
            RegionDrag::begin(0, bad, voxel_bounds()),
            None,
            "a press at {bad:?} began a region drag",
        );
        assert_eq!(
            PickedBox::new(bad, 40.0),
            None,
            "a press at {bad:?} picked an offline area",
        );
        assert!(
            SectionLine::new(bad, at(179.0)).is_none(),
            "a press at {bad:?} began a section line",
        );
        assert!(
            SectionLine::new(at(179.0), bad).is_none(),
            "a release at {bad:?} finished a section line",
        );
        assert_eq!(
            VolumeRegion::new(bad, squallar_radar::voxel::HalfExtentKm::square(120.0)),
            None,
            "a resample box was built about {bad:?}",
        );

        // And a corner leaves a drag that had already measured exactly as it was:
        // one bad frame must not stick for the rest of the gesture.
        let mut live = RegionDrag::begin(0, at(179.5), voxel_bounds()).expect("a press at 179.5");
        live.extend_to(GeoPoint {
            lat: LAT + 0.6,
            lon: -179.5,
        });
        let settled = live;
        live.extend_to(bad);
        assert_eq!(live, settled, "a corner at {bad:?} moved the drag");
    }
}

/// **The data path is where it was.** The gesture fold stands in front of the
/// predicate and never inside it, so the loud rejection
/// `squallar_source::volume::VolumeGrid::footprint` depends on still fires —
/// and a gesture is not permitted to reach the frame that rejection is about.
#[test]
fn the_strict_predicate_the_data_path_reads_is_unchanged() {
    for (far, _) in SEAM_PAIRS {
        assert!(
            !at(far).is_on_earth(),
            "is_on_earth accepted {far}; a footprint straddling the antimeridian \
             would stop being rejected and a bbox spanning the world would pass",
        );
    }
    for lon in [180.001, -180.001] {
        assert!(!at(lon).is_on_earth(), "is_on_earth accepted {lon}");
    }
}
