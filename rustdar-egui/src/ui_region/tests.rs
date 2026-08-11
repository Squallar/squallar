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
        for (name, pos) in [
            ("north", egui::pos2(rect.center().x, rect.top())),
            ("south", egui::pos2(rect.center().x, rect.bottom())),
            ("west", egui::pos2(rect.left(), rect.center().y)),
            ("east", egui::pos2(rect.right(), rect.center().y)),
        ] {
            let edge_km = ground_km_to(region, rect, &memory, center, pos);
            assert!(
                region.half_width_km() <= edge_km,
                "a {:.0}x{:.0} viewport put its box {:.3} km out past its {name} edge at \
                 {:.3} km: the floor stops there and the volume would stand on nothing",
                rect.width(),
                rect.height(),
                region.half_width_km() - edge_km,
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
        region.half_width_km() <= north,
        "the box was sized past the near edge: {:.3} km against {north:.3} km",
        region.half_width_km(),
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

    assert!(
        tight.half_width_km() < wide.half_width_km(),
        "zooming from 9 to 11 must shrink the box: {:.1} km against {:.1} km",
        tight.half_width_km(),
        wide.half_width_km(),
    );
    let (wide_km, tight_km) = (
        wide.resolution_km(cells).expect("a non-zero cell count"),
        tight.resolution_km(cells).expect("a non-zero cell count"),
    );
    assert!(
        tight_km < wide_km,
        "the tighter box must be the finer sampling: {tight_km:.3} km/cell against \
         {wide_km:.3} km/cell",
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
        first.half_width_km(),
        second.half_width_km(),
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
        let steps = region.half_width_km() / HALF_WIDTH_STEP_KM;
        assert_eq!(
            steps,
            steps.floor(),
            "a {width}-point pane produced {:.6} km, which is not a whole number of \
             {HALF_WIDTH_STEP_KM} km steps",
            region.half_width_km(),
        );
        let raw = ground_km_to(
            region,
            rect,
            &memory,
            center,
            egui::pos2(rect.center().x, rect.top()),
        );
        assert!(
            region.half_width_km() <= raw,
            "a {width}-point pane rounded {raw:.6} km *up* to {:.6} km, putting the box \
             outside the floor",
            region.half_width_km(),
        );
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
/// The resampler's ceiling and the fallback are the same constant, so a pane
/// zoomed right out lands exactly on it — and the caption, the camera's box and
/// the resample all describe one box.
#[test]
fn a_pane_zoomed_out_past_the_ceiling_gets_the_whole_scan_box() {
    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(900.0, 900.0));
    let region = region_for_viewport(rect, &memory_at(5.0), centre())
        .expect("a wide-open pane still has a measurable box");
    assert_eq!(
        region.half_width_km(),
        rustdar_radar::voxel::MAX_HALF_WIDTH_KM,
        "a pane showing more ground than the resampler will honour must stop at its \
         ceiling, which is also `DEFAULT_HALF_WIDTH_KM`",
    );
    assert_eq!(
        region.half_width_km(),
        crate::pane::DEFAULT_HALF_WIDTH_KM,
        "the ceiling and the degenerate-viewport fallback are one constant",
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
