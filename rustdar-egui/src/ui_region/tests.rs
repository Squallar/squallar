//! The region is the viewport, and these are the properties that makes true.
//!
//! Every test here is about *containment* — the box the 3D pane resamples must
//! fit inside the ground its own map is showing, on all four sides — plus the
//! stability the derivation needs to not rebuild an 8 MiB grid every frame.
//!
//! The 18 tests this file used to hold were all about the drag that picked the
//! region and the rule that decided which pane a dragged region landed on
//! (`RegionDrag`, `corners_for`, `destination_for`). None of those things
//! exists: there is no gesture, no anchor, no commit, no minimum drag, no
//! source pane and no destination rule, because a 3D view's box is now the
//! viewport of the pane drawing it. They are named in the change's test
//! accounting rather than reconstructed here — a test for a concept that has
//! been deleted cannot fail, and a suite full of tests that cannot fail is the
//! defect this codebase keeps having to fix.

use super::{HALF_WIDTH_STEP_KM, region_for_viewport};
use crate::pane::VolumeRegion;

/// A map memory parked at `zoom` with no detached centre, which is the state a
/// pane following its site is in.
fn memory_at(zoom: f64) -> walkers::MapMemory {
    let mut memory = walkers::MapMemory::default();
    memory
        .set_zoom(zoom)
        .expect("the test zooms are inside walkers' range");
    memory
}

/// Oklahoma City, near KTLX — a middle latitude where Mercator's scale varies
/// enough across a pane to matter and no test is accidentally at the equator.
fn centre() -> walkers::Position {
    walkers::lat_lon(35.33, -97.28)
}

/// The ground from `region`'s centre to a screen position, kilometres, through
/// the same projector and the same geodesy the derivation used.
fn ground_km_to(
    region: VolumeRegion,
    rect: egui::Rect,
    memory: &walkers::MapMemory,
    center: walkers::Position,
    pos: egui::Pos2,
) -> f64 {
    let projector = walkers::Projector::new(rect, memory, center);
    let edge = projector.unproject(pos.to_vec2());
    let (_, range_km) = rustdar_radar::beam::site_bearing_range_km(
        region.centre().lat,
        region.centre().lon,
        edge.y(),
        edge.x(),
    );
    range_km
}

/// **The change itself**: a wide pane gets a wide box, in the proportion its
/// *ground* is — and that is not the proportion its pixels are.
///
/// The box was the largest square inscribed in the viewport, so converting a
/// 16:9 pane to 3D stopped resampling the left and right flanks of the ground
/// the pane went on showing. Without this test the flip has no pin at all:
/// every other test here is a containment or stability property that the old
/// inscribed square satisfied too.
///
/// The proportion is checked against **ground measured through the projector**,
/// not against `rect.aspect_ratio()`, and the last assertion is what gives that
/// teeth. Mercator is conformal, so a pane's ground aspect and its pixel aspect
/// agree *locally* — they differ only by how much `cos(latitude)` varies over
/// the pane's own vertical span, since the east lane is measured at the
/// centre's latitude and the north lane runs to the poleward edge. That is a
/// small effect and it has to beat the quantisation to be a test rather than a
/// coincidence. Measured at zoom 8: both panes' pixel aspect sits 0.69% below
/// their ground aspect, where the band's lower edge is only 0.34% and 0.25%
/// below it — so an implementation that scaled a square by the pixel aspect
/// lands outside. (At zoom 9 it would not: the box halves, the quantum does
/// not, and the band opens to 0.67% while the Mercator gap closes to 0.34%.
/// The zoom is chosen for that reason and the last assertion is what says so if
/// it stops being true.)
///
/// The band itself is derived rather than picked. Each axis is floored to its
/// own whole [`HALF_WIDTH_STEP_KM`], so the ratio of the two can be as low as
/// `(east - step) / north` and as high as `east / (north - step)`.
///
/// Zoom 8 is also below the resampler's ceiling for both panes, so what is
/// being compared is the measurement rather than `HalfExtentKm::clamped`'s
/// corner scaling — which preserves the aspect and would let this pass while
/// saying nothing about the measurement.
#[test]
fn a_wide_viewport_gets_a_wide_box_in_the_proportion_its_ground_is() {
    let memory = memory_at(8.0);
    let center = centre();
    for size in [egui::vec2(1200.0, 500.0), egui::vec2(1600.0, 500.0)] {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), size);
        let region = region_for_viewport(rect, &memory, center).expect("a measurable box");

        assert!(
            region.half_east_km() > region.half_north_km(),
            "a {:.1}:1 pane must get a box wider than it is tall, or the 3D view \
             is still cutting the viewport down to a square: {:?}",
            size.x / size.y,
            region.half_extent_km(),
        );

        // The ground the pane is showing, re-derived through the same projector
        // and the same geodesy — each axis the nearer of its own two edges,
        // which is what containment requires.
        let ground = |a, b| {
            f64::min(
                ground_km_to(region, rect, &memory, center, a),
                ground_km_to(region, rect, &memory, center, b),
            )
        };
        let east = ground(
            egui::pos2(rect.left(), rect.center().y),
            egui::pos2(rect.right(), rect.center().y),
        );
        let north = ground(
            egui::pos2(rect.center().x, rect.top()),
            egui::pos2(rect.center().x, rect.bottom()),
        );
        assert!(
            region.half_extent_km().corner_km() < rustdar_radar::voxel::MAX_HALF_DIAGONAL_KM,
            "precondition: this fixture must be under the resampler's ceiling, or \
             the aspect below is `clamped`'s and not the measurement's",
        );

        let got = region.half_east_km() / region.half_north_km();
        let (lo, hi) = (
            (east - HALF_WIDTH_STEP_KM) / north,
            east / (north - HALF_WIDTH_STEP_KM),
        );
        assert!(
            (lo..=hi).contains(&got),
            "the box is {got:.4}:1 where the ground it is measured off is \
             {:.4}:1 ({east:.3} km by {north:.3} km); the {HALF_WIDTH_STEP_KM} km \
             quantum allows {lo:.4}..={hi:.4} and nothing else does",
            east / north,
        );

        // And the ground's proportion is not the rect's, so the assertion above
        // cannot be satisfied by dividing the pane's width by its height.
        let pixels = f64::from(size.x / size.y);
        assert!(
            !(lo..=hi).contains(&pixels),
            "at this latitude and zoom the pane's {pixels:.4}:1 pixel aspect is \
             inside the {lo:.4}..={hi:.4} band its {:.4}:1 ground aspect allows, \
             so this fixture no longer distinguishes a measured box from a \
             scaled rect",
            east / north,
        );
    }
}

/// **The property the whole module exists for**: the box is inside the ground
/// the pane's map is showing, on every side.
///
/// The floor a 3D pane stands on is that map, so a box wider than it is a
/// volume standing on transparency past the floor's edge — the bug the stored,
/// separately-dragged region produced whenever the map it was dragged on was
/// showing less ground than the box it described.
///
/// Checked at four edge midpoints rather than at one, because Mercator's scale
/// varies with latitude: the four distances are genuinely different numbers,
/// and a derivation that took the wrong one would still pass a single-edge
/// test. On a non-square viewport as well, since that is the shape a real pane
/// in a split layout has.
#[test]
fn the_box_fits_inside_the_viewport_on_every_side() {
    let memory = memory_at(9.0);
    let center = centre();
    for rect in [
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 800.0)),
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1200.0, 500.0)),
        egui::Rect::from_min_size(egui::pos2(120.0, 60.0), egui::vec2(400.0, 900.0)),
    ] {
        let region = region_for_viewport(rect, &memory, center)
            .expect("a pane with area at zoom 9 has a measurable box");
        for (name, axis_km, pos) in [
            (
                "north",
                region.half_north_km(),
                egui::pos2(rect.center().x, rect.top()),
            ),
            (
                "south",
                region.half_north_km(),
                egui::pos2(rect.center().x, rect.bottom()),
            ),
            (
                "west",
                region.half_east_km(),
                egui::pos2(rect.left(), rect.center().y),
            ),
            (
                "east",
                region.half_east_km(),
                egui::pos2(rect.right(), rect.center().y),
            ),
        ] {
            let edge_km = ground_km_to(region, rect, &memory, center, pos);
            assert!(
                axis_km <= edge_km,
                "a {:.0}x{:.0} viewport put its box {:.3} km out past its {name} edge at \
                 {:.3} km: the floor stops there and the volume would stand on nothing",
                rect.width(),
                rect.height(),
                axis_km - edge_km,
                edge_km,
            );
        }
    }
}

/// The four edges really are different distances, so the minimum above is doing
/// work rather than agreeing with everything.
///
/// Without this the containment test would still pass against a derivation that
/// used the centre's scale for all four sides — the exact shortcut that leaves a
/// thin transparent strip along the poleward edge. In the northern hemisphere
/// Mercator stretches with latitude, so the ground under the *north* half of a
/// pane is the narrower of the two, and it is what the box must be sized by.
#[test]
fn the_poleward_edge_is_the_near_one_and_the_box_is_sized_by_it() {
    // A tall pane low in the zoom range, so the box spans enough latitude for
    // the asymmetry to exceed the quantisation step.
    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(900.0, 900.0));
    let memory = memory_at(8.0);
    let center = centre();
    let region = region_for_viewport(rect, &memory, center).expect("a measurable box");

    let north = ground_km_to(
        region,
        rect,
        &memory,
        center,
        egui::pos2(rect.center().x, rect.top()),
    );
    let south = ground_km_to(
        region,
        rect,
        &memory,
        center,
        egui::pos2(rect.center().x, rect.bottom()),
    );
    assert!(
        north < south,
        "in the northern hemisphere the ground above the centre must be the narrower \
         half — got north {north:.3} km, south {south:.3} km — or there is nothing for \
         taking the minimum to protect",
    );
    assert!(
        south - north > HALF_WIDTH_STEP_KM,
        "the two halves differ by only {:.3} km, under the {HALF_WIDTH_STEP_KM} km \
         quantum: this fixture no longer distinguishes the minimum from the mean",
        south - north,
    );
    assert!(
        region.half_north_km() <= north,
        "the box was sized past the near edge: {:.3} km against {north:.3} km",
        region.half_north_km(),
    );
}

/// Zooming the pane in tightens the box, which is the only region control there
/// is — and the whole reason the drag it replaced existed.
///
/// The grid has a fixed cell count, so a tighter box is finer sampling rather
/// than a smaller allocation. This asserts the resolution moves the right way,
/// through the pane's own readout, so a sign error would be caught by the number
/// the caption prints rather than only by the picture.
#[test]
fn zooming_in_buys_resolution() {
    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 800.0));
    let center = centre();
    let cells = rustdar_radar::voxel::default_shape().nx;

    let wide = region_for_viewport(rect, &memory_at(9.0), center).expect("a measurable box");
    let tight = region_for_viewport(rect, &memory_at(11.0), center).expect("a measurable box");

    for (axis, tight_km, wide_km) in [
        ("east", tight.half_east_km(), wide.half_east_km()),
        ("north", tight.half_north_km(), wide.half_north_km()),
    ] {
        assert!(
            tight_km < wide_km,
            "zooming from 9 to 11 must shrink the box on its {axis} axis: \
             {tight_km:.1} km against {wide_km:.1} km",
        );
    }
    let (wide_km, tight_km) = (
        wide.resolution_km(cells).expect("a non-zero cell count"),
        tight.resolution_km(cells).expect("a non-zero cell count"),
    );
    assert!(
        tight_km.0 < wide_km.0 && tight_km.1 < wide_km.1,
        "the tighter box must be the finer sampling on both axes: \
         {tight_km:?} km/cell against {wide_km:?} km/cell",
    );
}

/// A viewport that moves by less than the quantum produces the *same* region.
///
/// This is not cosmetic. The region is part of `VolumeTarget`, so a region that
/// differed between two frames of a still pane would ask for a fresh 8 MiB
/// resample on every one of them — a permanently hot CPU whose only symptom is a
/// fan. Sub-pixel jitter in a pane rect is ordinary; a rebuild caused by it is
/// not.
#[test]
fn a_sub_quantum_change_in_the_viewport_is_the_same_box() {
    let memory = memory_at(9.0);
    let center = centre();
    let base = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 800.0));
    let nudged = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.05, 800.05));

    let first = region_for_viewport(base, &memory, center).expect("a measurable box");
    let second = region_for_viewport(nudged, &memory, center).expect("a measurable box");
    assert_eq!(
        first.half_extent_km(),
        second.half_extent_km(),
        "a 0.05-point change in the pane rect changed the box, so a still pane would \
         rebuild its grid for ever",
    );
}

/// The quantisation rounds **down**, never up.
///
/// Rounding up would push the box past the ground the floor covers by as much as
/// a kilometre — reintroducing, through a rounding mode, exactly the overhang
/// the derivation exists to remove.
#[test]
fn the_half_width_is_a_whole_number_of_steps_and_never_rounded_up() {
    let memory = memory_at(9.0);
    let center = centre();
    // Sizes chosen to land the raw measurement all over the step, so at least
    // one of them would be rounded up by a `round()` and caught here.
    for width in [640.0_f32, 683.0, 701.0, 745.0, 799.0] {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(width, width));
        let region = region_for_viewport(rect, &memory, center).expect("a measurable box");
        for (axis, km, edge) in [
            (
                "east",
                region.half_east_km(),
                egui::pos2(rect.right(), rect.center().y),
            ),
            (
                "north",
                region.half_north_km(),
                egui::pos2(rect.center().x, rect.top()),
            ),
        ] {
            let steps = km / HALF_WIDTH_STEP_KM;
            assert_eq!(
                steps,
                steps.floor(),
                "a {width}-point pane produced {km:.6} km on its {axis} axis, which is \
                 not a whole number of {HALF_WIDTH_STEP_KM} km steps",
            );
            let raw = ground_km_to(region, rect, &memory, center, edge);
            assert!(
                km <= raw,
                "a {width}-point pane rounded {raw:.6} km *up* to {km:.6} km on its \
                 {axis} axis, putting the box outside the floor",
            );
        }
    }
}

/// A pane with no area has no measurable viewport, and says so.
///
/// A divider dragged to the edge produces this, and it is a real state rather
/// than an error. The caller falls back to the whole-scan box, which crops
/// nothing — the honest answer for a pane that has not been given any room to
/// say what it wants.
#[test]
fn a_collapsed_pane_has_no_measurable_box() {
    let memory = memory_at(7.0);
    let center = centre();
    for rect in [
        egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(0.0, 400.0)),
        egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(400.0, 0.0)),
        egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(0.0, 0.0)),
        egui::Rect::NOTHING,
    ] {
        assert_eq!(
            region_for_viewport(rect, &memory, center),
            None,
            "a {:?} rect is not a viewport that can be measured",
            rect.size(),
        );
    }
}

/// A viewport showing less ground than the resampler's minimum box is refused
/// rather than clamped up.
///
/// `build_voxels` clamps a too-small half-width instead of refusing it, so a
/// clamped region would resample a box *larger* than the viewport that measured
/// it — the pane's caption would describe ground the floor does not cover, and
/// the overhang would be back. Refusing hands the caller the whole-scan
/// fallback, which is wrong in the visible direction rather than the invisible
/// one.
#[test]
fn a_viewport_below_the_resamplers_minimum_is_refused() {
    // Deep zoom over a small pane: a few hundred metres of ground.
    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(64.0, 64.0));
    let region = region_for_viewport(rect, &memory_at(18.0), centre());
    assert_eq!(
        region,
        None,
        "a viewport under {} km across must be refused, not clamped up to a box the \
         floor does not cover",
        2.0 * rustdar_radar::voxel::MIN_HALF_WIDTH_KM,
    );
}

/// The whole-scan box a wide-open pane asks for is honoured rather than clamped
/// down to something smaller.
///
/// A pane zoomed right out lands exactly on the resampler's ceiling, and the
/// caption, the camera's box and the resample all describe that one box.
///
/// # The ceiling and the stand-in are no longer one constant
///
/// They were, and it read as a tidy coincidence: both were 230 km, the nominal
/// unambiguous range. They are now two different facts and the pin says which
/// is which.
///
/// The **ceiling** is what a plan view's furthest frame earns —
/// `box_half_width_km(types::MAX_EXTENT_KM)`, the largest square inscribed in
/// the widest circle a raster will ever be projected at. That is the number
/// [`super::zoom_viewport`]'s outward bound means by "the ground the radar
/// itself covers", and it had to move: a plan view follows its own sweep out to
/// 460 km now, so holding the box at 230 left the 3D pane showing less than the
/// 2D pane beside it — the original complaint with the two panes swapped.
///
/// The **stand-in** ([`crate::pane::BASE_HALF_WIDTH_KM`]) is still the raster's
/// floor, because it answers a different question: not "how far may a box be
/// opened" but "what box does a pane pose its camera against before anything
/// has measured one". Pinning them equal would tie a fallback for an unmeasured
/// viewport to a ceiling that follows the data.
#[test]
fn a_pane_zoomed_out_past_the_ceiling_gets_the_whole_scan_box() {
    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(900.0, 900.0));
    let region = region_for_viewport(rect, &memory_at(5.0), centre())
        .expect("a wide-open pane still has a measurable box");
    let corner = region.half_extent_km().corner_km();
    // The bound is on the **corner**, and it is landed on by scaling both axes
    // by one factor rather than by clamping each to a constant — so it is hit
    // to within the last few bits rather than exactly. Measured across 300k
    // random extents at pane-shaped aspects: 1.14e-13 km, a tenth of a
    // nanometre. The tolerance is four orders above that and eleven below the
    // 1 km quantisation this is derived through, so it can only pass for the
    // right reason.
    assert!(
        (corner - rustdar_radar::voxel::MAX_HALF_DIAGONAL_KM).abs() < 1e-9,
        "a pane showing more ground than the resampler will honour must stop at its \
         ceiling rather than be refused a box: a {corner} km corner against {}",
        rustdar_radar::voxel::MAX_HALF_DIAGONAL_KM,
    );
    assert_eq!(
        rustdar_radar::voxel::MAX_HALF_WIDTH_KM,
        rustdar_radar::voxel::box_half_width_km(rustdar_radar::types::MAX_EXTENT_KM),
        "the ceiling must be the box the widest frame a plan view will project \
         earns, or a 3D pane stops short of the picture beside it",
    );
    assert!(
        region.half_east_km() > crate::pane::BASE_HALF_WIDTH_KM
            && region.half_north_km() > crate::pane::BASE_HALF_WIDTH_KM,
        "the ceiling is no longer the stand-in: a pane zoomed out must be able to \
         pass the box an unmeasured one poses its camera against",
    );
}

/// The box is centred on what the pane is centred on.
///
/// A box measured off the right amount of ground but aimed somewhere else is the
/// same failure as one that is too big, and it is quieter: the picture is
/// plausible and simply not of the storm the user is looking at.
#[test]
fn the_box_is_centred_on_the_pane() {
    let rect = egui::Rect::from_min_size(egui::pos2(37.0, 91.0), egui::vec2(640.0, 480.0));
    let memory = memory_at(9.0);
    let center = centre();
    let region = region_for_viewport(rect, &memory, center).expect("a measurable box");
    let (_, offset_km) = rustdar_radar::beam::site_bearing_range_km(
        center.y(),
        center.x(),
        region.centre().lat,
        region.centre().lon,
    );
    assert!(
        offset_km < 1e-6,
        "the box is centred {offset_km:.6} km from the pane's own centre",
    );
}

/// A pane panned away from its site takes its box with it.
///
/// `MapMemory::detached` is how walkers reports a map the user has dragged off
/// its follow target, and the derivation has to read the same centre the map is
/// drawn at — otherwise a panned 3D pane would resample the ground it *used* to
/// be over, with its own floor showing somewhere else entirely.
#[test]
fn a_detached_map_measures_its_box_where_it_is_looking() {
    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 800.0));
    let mut memory = memory_at(9.0);
    let elsewhere = walkers::lat_lon(41.6, -88.08); // near KLOT, several hundred km away
    memory.center_at(elsewhere);

    let region = region_for_viewport(rect, &memory, centre()).expect("a measurable box");
    let (_, from_detached) = rustdar_radar::beam::site_bearing_range_km(
        elsewhere.y(),
        elsewhere.x(),
        region.centre().lat,
        region.centre().lon,
    );
    assert!(
        from_detached < 1e-6,
        "a panned pane's box stayed {from_detached:.3} km from where the map is \
         actually centred",
    );
}
