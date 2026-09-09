//! **The one place a code plane becomes a payload the UI can draw.**
//!
//! What is at stake here is agreement, not arithmetic. The plane and the
//! geometry arrive as two objects describing one sweep, and a payload built
//! out of a mismatched pair paints a picture nobody measured: one sweep's
//! codes at another sweep's angles, plausible and wrong. So the refusals are
//! asserted before anything else, each against a pair that is healthy in every
//! other respect.
//!
//! The rest is that nothing is *reinterpreted* on the way across. The levels
//! come out in the order and at the lengths the producer wrote them, the edge
//! table is the render's own wedge predicate and not a second opinion about
//! where a radial starts, and the colour table is the product's own palette
//! baked once.

use super::fan_sweep;
use squallar_radar::render::codes::{CodePlane, Lut, LutKey};
use squallar_radar::render::polar::{PolarGeometry, Wedge};
use squallar_radar::types::RadarProduct;

const RADIALS: usize = 8;
const GATES: usize = 6;
const FIRST_GATE_SLANT_KM: f64 = 2.125;
const GATE_INTERVAL_SLANT_KM: f64 = 0.25;
const ELEVATION_DEG: f64 = 0.5;
const SITE_LAT: f64 = 35.33;
const SITE_LON: f64 = -97.28;

/// Reflectivity: an eight-bit wire moment, which is what the plane admits
/// without argument. `(2.0, 66.0)` is the scale/offset a real Level II
/// reflectivity moment carries.
fn key() -> LutKey {
    LutKey {
        product: RadarProduct::Reflectivity,
        scale: 2.0,
        offset: 66.0,
    }
}

/// A plane whose codes are all distinct, so a level that came out reordered or
/// offset by one is visible rather than hidden by repetition.
fn plane() -> CodePlane {
    CodePlane::build(
        RADIALS,
        GATES,
        (0..(RADIALS * GATES) as u32)
            .map(|i| (i + 2) as u8)
            .collect(),
        key(),
        8,
    )
    .expect("reflectivity at eight bits is admitted")
}

fn wedges() -> Vec<Wedge> {
    (0..RADIALS)
        .map(|i| Wedge {
            azimuth_deg: i as f32 * 45.0,
            half_width_deg: 22.5,
        })
        .collect()
}

fn geometry() -> PolarGeometry {
    PolarGeometry::from_parts(
        wedges(),
        FIRST_GATE_SLANT_KM,
        GATE_INTERVAL_SLANT_KM,
        Some(ELEVATION_DEG),
        GATES,
    )
}

#[test]
fn a_matched_plane_and_geometry_make_a_well_formed_payload() {
    let (plane, geometry) = (plane(), geometry());
    let sweep =
        fan_sweep(&plane, &geometry, SITE_LAT, SITE_LON).expect("the pair describes one sweep");

    assert!(
        sweep.is_well_formed(),
        "the producer built a payload the consumer's own door refuses"
    );
    assert_eq!(sweep.radials as usize, RADIALS);
    assert_eq!(sweep.gates as usize, GATES);
    assert_eq!(sweep.field, squallar_radar::fields::known::REFLECTIVITY);
    assert_eq!(sweep.geometry.reach_gates as usize, geometry.reach_gates());
    assert_eq!(sweep.geometry.site_lat, SITE_LAT);
    assert_eq!(sweep.geometry.site_lon, SITE_LON);
    assert_eq!(sweep.geometry.elevation_deg, Some(ELEVATION_DEG));
    assert_eq!(sweep.geometry.first_gate_slant_km, FIRST_GATE_SLANT_KM);
    assert_eq!(
        sweep.geometry.gate_interval_slant_km,
        GATE_INTERVAL_SLANT_KM
    );
    // Carried as data so no shader spells a radius of its own, and they are
    // the tree's own two — the sphere a ground range is taken on, and the
    // bent radius a slant range converts through. They are not equal, and a
    // payload that made them equal would be a straight beam.
    assert_eq!(
        sweep.geometry.earth_radius_km,
        squallar_geo::EARTH_RADIUS_KM
    );
    assert_eq!(
        sweep.geometry.effective_radius_km,
        squallar_radar::beam::RE_EFF_KM
    );
    assert_ne!(
        sweep.geometry.earth_radius_km,
        sweep.geometry.effective_radius_km
    );
}

/// **Every level, byte for byte, in the order the producer wrote it.** Read
/// back through the consumer's own offsets, so an offset that was one level
/// out would surface here as different bytes rather than as a length that
/// still happened to fit.
#[test]
fn every_level_arrives_unreordered_and_at_its_own_length() {
    let (plane, geometry) = (plane(), geometry());
    let sweep = fan_sweep(&plane, &geometry, SITE_LAT, SITE_LON).expect("matched pair");

    assert_eq!(sweep.levels(), plane.levels());
    assert!(
        plane.levels() > 1,
        "fixture: a one-level plane cannot show that the chain survives the trip"
    );
    for level in 0..plane.levels() {
        let (want, r, g) = plane.level(level).expect("inside the chain");
        assert_eq!(
            sweep.level_shape(level),
            Some((r as u32, g as u32)),
            "level {level} shape"
        );
        assert_eq!(sweep.level(level), Some(want), "level {level} bytes");
    }
    assert_eq!(sweep.codes.len(), plane.resident_bytes());
}

/// The table is the product's own palette, baked once — the same bytes
/// `Lut::of` produces for the plane's own decode, not a table rebuilt from a
/// decode assembled here.
#[test]
fn the_table_is_the_planes_own_key_baked() {
    let (plane, geometry) = (plane(), geometry());
    let sweep = fan_sweep(&plane, &geometry, SITE_LAT, SITE_LON).expect("matched pair");
    assert_eq!(sweep.lut_rgba, Lut::of(plane.decode()).to_rgba_bytes());
    // And it is a real table rather than a run of one colour, or the equality
    // above would hold over anything.
    let distinct: std::collections::BTreeSet<&[u8]> = sweep.lut_rgba.chunks(4).collect();
    assert!(distinct.len() > 8, "{} distinct entries", distinct.len());
}

/// **The edges are the render's own wedge predicate**, whose bounds are
/// `azimuth ± half_width`, and an unpainted radial collapses instead of
/// drawing somewhere arbitrary.
#[test]
fn the_edges_are_the_drawn_wedges_and_an_unpainted_radial_collapses() {
    let mut ws = wedges();
    ws[3] = Wedge::UNPAINTED;
    let geometry = PolarGeometry::from_parts(
        ws.clone(),
        FIRST_GATE_SLANT_KM,
        GATE_INTERVAL_SLANT_KM,
        Some(ELEVATION_DEG),
        GATES,
    );
    let sweep = fan_sweep(&plane(), &geometry, SITE_LAT, SITE_LON).expect("matched pair");

    assert_eq!(sweep.edges.len(), RADIALS);
    for (i, w) in ws.iter().enumerate() {
        if i == 3 {
            assert_eq!(
                sweep.edges[i],
                [0.0, 0.0],
                "the unpainted radial did not collapse"
            );
            continue;
        }
        assert_eq!(
            sweep.edges[i],
            [
                w.azimuth_deg - w.half_width_deg,
                w.azimuth_deg + w.half_width_deg
            ]
        );
        // The sector is the width the render painted, not the width the sweep
        // declared: those are the same only when nothing was trimmed, and this
        // says which one travelled.
        assert!((sweep.edges[i][1] - sweep.edges[i][0] - 45.0).abs() < 1e-4);
    }
}

/// **`reach_km` is the far edge of the last gate, not its centre.** Stated as
/// two inequalities against the geometry's own `gate_ground_km`, so it is a
/// property of where the disc ends rather than a second spelling of the
/// expression that computed it.
#[test]
fn the_disc_ends_past_the_last_gates_centre_and_short_of_the_next() {
    let (plane, geometry) = (plane(), geometry());
    let sweep = fan_sweep(&plane, &geometry, SITE_LAT, SITE_LON).expect("matched pair");
    let last = geometry.reach_gates() - 1;
    assert!(
        sweep.geometry.reach_km > geometry.gate_ground_km(last),
        "{} is not past gate {last}'s centre at {}",
        sweep.geometry.reach_km,
        geometry.gate_ground_km(last),
    );
    assert!(
        sweep.geometry.reach_km < geometry.gate_ground_km(geometry.reach_gates()),
        "{} reaches into the gate after the last one drawn",
        sweep.geometry.reach_km,
    );
}

/// A geometry whose ranges are **already ground ranges** is not converted a
/// second time — the distinction `PolarGeometry::elevation_deg` draws with
/// `None`, carried across rather than flattened to zero degrees.
#[test]
fn a_ground_range_geometry_is_not_bent_again() {
    let flat = PolarGeometry::from_parts(
        wedges(),
        FIRST_GATE_SLANT_KM,
        GATE_INTERVAL_SLANT_KM,
        None,
        GATES,
    );
    let sweep = fan_sweep(&plane(), &flat, SITE_LAT, SITE_LON).expect("matched pair");
    assert_eq!(sweep.geometry.elevation_deg, None);
    assert_eq!(
        sweep.geometry.reach_km,
        FIRST_GATE_SLANT_KM + (GATES as f64 - 0.5) * GATE_INTERVAL_SLANT_KM
    );
    // The tilted arm of the same fixture reaches a *shorter* ground range,
    // which is what says the conversion is live and not a no-op both ways.
    let tilted = fan_sweep(&plane(), &geometry(), SITE_LAT, SITE_LON).expect("matched pair");
    assert!(tilted.geometry.reach_km < sweep.geometry.reach_km);
}

/// Each mismatch on its own, against a pair that agrees in every other
/// respect.
#[test]
fn a_plane_and_a_geometry_that_are_not_one_sweep_are_refused() {
    let wide = PolarGeometry::from_parts(
        (0..RADIALS + 1)
            .map(|i| Wedge {
                azimuth_deg: i as f32 * 40.0,
                half_width_deg: 20.0,
            })
            .collect(),
        FIRST_GATE_SLANT_KM,
        GATE_INTERVAL_SLANT_KM,
        Some(ELEVATION_DEG),
        GATES,
    );
    assert!(
        fan_sweep(&plane(), &wide, SITE_LAT, SITE_LON).is_none(),
        "accepted a geometry with one more radial than the plane"
    );

    // **Narrower, not wider**, and the direction is the whole point. A
    // geometry one gate *wider* also has a reach one gate wider, so the reach
    // guard refuses it and the shape check is never reached — a fixture that
    // trips two guards says nothing about either. One gate narrower carries a
    // reach that is comfortably inside the plane, so the stride disagreement
    // is the only thing left to refuse it.
    let shallow = PolarGeometry::from_parts(
        wedges(),
        FIRST_GATE_SLANT_KM,
        GATE_INTERVAL_SLANT_KM,
        Some(ELEVATION_DEG),
        GATES - 1,
    );
    assert!(
        shallow.reach_gates() <= GATES,
        "fixture: the reach guard would fire"
    );
    assert!(
        fan_sweep(&plane(), &shallow, SITE_LAT, SITE_LON).is_none(),
        "accepted a geometry one gate narrower than the plane"
    );

    // The healthy control, so the two refusals above are the mismatch's and
    // not the fixture's.
    assert!(fan_sweep(&plane(), &geometry(), SITE_LAT, SITE_LON).is_some());
}

/// A geometry that reached no gates has no disc to draw, and one whose reach
/// runs past its own stride is describing a row it does not have.
///
/// **Both arrive over the wire, and that is where this reaches them.**
/// `from_parts` ties `reach_gates` to `gates`, so nothing built through it can
/// state either defect — but `PolarField::from_bytes` reads the reach off the
/// buffer as a bare `u32` and validates it against nothing, and
/// `PolarRenderTarget::into_field` derives it from what was actually painted,
/// which is `0` for a sweep that painted nothing. A worker reply is exactly
/// that buffer. Doctoring the four bytes is the shortest statement of the
/// hazard the guard exists for.
#[test]
fn a_reach_of_nothing_and_a_reach_past_the_stride_are_both_refused() {
    // The reach is the third `u32` of the header — `PolarField::to_bytes`.
    const REACH_AT: usize = 8;

    let doctored = |reach: u32| {
        let field = squallar_radar::render::polar::PolarField::from_parts(geometry(), Vec::new());
        let mut bytes = field.to_bytes();
        bytes[REACH_AT..REACH_AT + 4].copy_from_slice(&reach.to_le_bytes());
        squallar_radar::render::polar::PolarField::from_bytes(&bytes)
            .expect("only the reach was changed, and the length did not move")
    };

    // The control first: the same round trip, untouched, still builds — so the
    // two refusals below are the reach's and not the wire's.
    let intact = doctored(GATES as u32);
    assert_eq!(intact.geometry().reach_gates(), GATES);
    assert!(fan_sweep(&plane(), intact.geometry(), SITE_LAT, SITE_LON).is_some());

    let reached_nothing = doctored(0);
    assert_eq!(reached_nothing.geometry().reach_gates(), 0);
    assert!(
        fan_sweep(&plane(), reached_nothing.geometry(), SITE_LAT, SITE_LON).is_none(),
        "built a disc out of a sweep that painted no gate"
    );

    let past_the_stride = doctored(GATES as u32 + 1);
    assert_eq!(past_the_stride.geometry().reach_gates(), GATES + 1);
    assert_eq!(
        past_the_stride.geometry().gates(),
        GATES,
        "fixture: the stride must still match the plane, or the shape check fires first"
    );
    assert!(
        fan_sweep(&plane(), past_the_stride.geometry(), SITE_LAT, SITE_LON).is_none(),
        "accepted a reach past the row the plane actually has"
    );
}

/// **The fidelity refusal is consumed, not re-litigated.** Six products the
/// design scheduled onto R8 planes resolve more colours than a plane has
/// codes, and `CodePlane::build` is where that stops. This asserts the
/// consequence for the seam — such a product can never reach a fan surface,
/// because there is no plane to build one from — without forming a second
/// opinion about which products those are.
#[test]
fn a_product_the_plane_refuses_can_never_reach_a_fan() {
    for product in [
        RadarProduct::NormalizedRotation,
        RadarProduct::StormRelativeVelocity,
        RadarProduct::SpecificDifferentialPhase,
        RadarProduct::VerticallyIntegratedLiquid,
    ] {
        let refused = CodePlane::build(
            RADIALS,
            GATES,
            vec![7; RADIALS * GATES],
            LutKey::identity(product),
            8,
        );
        assert!(
            refused.is_err(),
            "{product:?} built a plane, so this seam's claim that it cannot \
             reach a fan now rests on nothing"
        );
    }
    // The control: the same call shape on a product the encoder admits.
    assert!(CodePlane::build(RADIALS, GATES, vec![7; RADIALS * GATES], key(), 8,).is_ok());
}

// ── The join: what the renderer produces is what this accepts ───────────────

/// [`a_scan`] with every radial carrying its moment.
fn a_scan() -> nexrad_model::data::Scan {
    a_scan_missing(&[])
}

/// A one-tilt volume of 36 radials, each carrying an eight-bit reflectivity
/// moment whose codes vary along the radial and die out past gate 80 — except
/// the radials named in `missing`, which carry none at all.
fn a_scan_missing(missing: &[u16]) -> nexrad_model::data::Scan {
    use nexrad_model::data::{
        ChannelConfiguration, ElevationCut, MomentData, PulseWidth, Radial, RadialStatus, Scan,
        Sweep, VolumeCoveragePattern, WaveformType,
    };
    let radials = (0..36u16)
        .map(|i| {
            let bytes: Vec<u8> = (0..120usize)
                .map(|g| if g < 80 { ((g % 200) + 2) as u8 } else { 0 })
                .collect();
            Radial::new(
                0,
                i,
                f32::from(i) * 10.0,
                10.0,
                RadialStatus::IntermediateRadialData,
                1,
                0.5,
                (!missing.contains(&i))
                    .then(|| MomentData::from_fixed_point(120, 2125, 250, 8, 2.0, 66.0, bytes)),
                None,
                None,
                None,
                None,
                None,
                None,
            )
        })
        .collect();
    let cut = ElevationCut::new(
        0.5,
        ChannelConfiguration::ConstantPhase,
        WaveformType::CS,
        20.0,
        true,
        true,
        false,
        false,
        1,
        20,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        false,
        0,
        false,
        0,
        false,
        false,
    );
    Scan::new(
        VolumeCoveragePattern::new(
            212,
            0,
            0.5,
            PulseWidth::Short,
            false,
            0,
            false,
            0,
            false,
            false,
            0,
            false,
            false,
            vec![cut],
        ),
        vec![Sweep::new(1, radials)],
    )
}

/// **What the renderer emits is what this function accepts** — the join
/// between the two crates, and the one seam whose failure is a pane with
/// nothing on it.
///
/// `fan_sweep` refuses a plane and a geometry that are not one sweep, and
/// every one of its refusals is silent at the call site: the delivery gets
/// `None`, the reply carries no raster either, and the frame retires as
/// failed. Nothing above this asserts that the producer's own two halves
/// satisfy those guards, and they arrive as separate objects built by separate
/// code, so they are free to drift.
///
/// The reach is the conjunct that would go first. `render_sweep_plane`
/// measures it off the painted codes; `is_well_formed` requires
/// `1 <= reach_gates <= gates`; the fixture dies out at gate 80 of 120, so a
/// producer that reported the stride instead would still pass the bound and a
/// producer that reported zero would not. The exact figure is asserted, not
/// just the bound.
#[test]
fn a_rendered_plane_is_a_payload_this_function_accepts() {
    let render = squallar_radar::render::render_sweep_plane(
        &a_scan(),
        0.5,
        RadarProduct::Reflectivity,
        &squallar_radar::nyquist::DeclaredNyquist::empty(),
        false,
    )
    .expect("an eight-bit reflectivity sweep renders as a plane");
    assert!(
        render.image.is_empty(),
        "the polar path built a raster as well, which is the allocation it exists to skip",
    );
    let plane = render.codes.as_ref().expect("the render carries a plane");
    assert_eq!(
        render.polar.geometry().reach_gates(),
        80,
        "the reach is the furthest painted gate, not the stride and not zero",
    );

    let sweep = fan_sweep(plane, render.polar.geometry(), SITE_LAT, SITE_LON)
        .expect("the renderer's own two halves describe one sweep");
    assert!(
        sweep.is_well_formed(),
        "the payload the renderer produced does not describe itself consistently",
    );
    assert_eq!((sweep.radials, sweep.gates), (36, 120));
    // Every level of the chain reached the payload, so a zoomed-out fragment
    // has a level to read rather than clamping to the closest.
    assert_eq!(sweep.levels(), plane.levels());
    assert!(sweep.levels() > 1, "a max-reducing product has a chain");
}

/// **A radial the sweep carried no moment for claims no sky.**
///
/// The raster's own semantics: `PolarBuffers` leaves a radial it never painted
/// with a NaN wedge, and `fan_sweep` turns that into a degenerate sector. The
/// polar path writes its wedges out rather than recording them as it paints,
/// so this is the branch where the two could have come to disagree — and the
/// disagreement would not be a missing picture, since an unpainted code is
/// transparent either way. It would be a *readout*: `draw_edges` gives the
/// azimuth to whichever radial claims it, and a radial with no data claiming a
/// sector takes hovers from the neighbours that do.
///
/// The populated radials are the control: their edges must be their own
/// wedge's bounds, so "every edge collapsed" cannot pass this.
#[test]
fn a_radial_with_no_moment_claims_no_sector() {
    let missing = [3u16, 17];
    let render = squallar_radar::render::render_sweep_plane(
        &a_scan_missing(&missing),
        0.5,
        RadarProduct::Reflectivity,
        &squallar_radar::nyquist::DeclaredNyquist::empty(),
        false,
    )
    .expect("the remaining radials still make a plane");
    let plane = render.codes.as_ref().expect("a plane");
    let sweep = fan_sweep(plane, render.polar.geometry(), SITE_LAT, SITE_LON)
        .expect("the pair describes one sweep");

    assert_eq!(sweep.edges.len(), 36);
    for (i, edge) in sweep.edges.iter().enumerate() {
        if missing.contains(&(i as u16)) {
            assert_eq!(
                *edge,
                [0.0, 0.0],
                "radial {i} carried no moment and still claimed a sector",
            );
        } else {
            assert!(
                edge[1] > edge[0],
                "radial {i} carried a moment and claimed nothing",
            );
        }
    }
}
