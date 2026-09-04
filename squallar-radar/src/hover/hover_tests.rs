//! What a loop frame reads, against what the still render painted.

use super::*;
use crate::render::polar::{GateAt, take_gate_reads};
use crate::types::RadarProduct;
use nexrad_model::data::{
    MomentData, PulseWidth, Radial, RadialStatus, Scan, Sweep, VolumeCoveragePattern,
};

const LAT: f64 = 35.3333;
const LON: f64 = -97.2778;
const ELEVATION: f32 = 0.5;
const GATES: usize = 400;
const GATE_KM: f64 = 0.25;
const SCALE: f32 = 2.0;
const OFFSET: f32 = 66.0;

fn volume(n_radials: usize, velocity: bool) -> Scan {
    let radials = (0..n_radials)
        .map(|i| {
            let bytes = (0..GATES)
                .map(|g| ((i * 7 + g * 3) % 254) as u8)
                .collect::<Vec<u8>>();
            let moment = MomentData::from_fixed_point(
                GATES as u16,
                0,
                (GATE_KM * 1000.0) as u16,
                8,
                SCALE,
                OFFSET,
                bytes,
            );
            let (refl, vel) = if velocity {
                (None, Some(moment))
            } else {
                (Some(moment), None)
            };
            Radial::new(
                0,
                i as u16,
                i as f32,
                1.0,
                RadialStatus::IntermediateRadialData,
                1,
                ELEVATION,
                refl,
                vel,
                None,
                None,
                None,
                None,
                None,
            )
        })
        .collect();
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
            Vec::new(),
        ),
        vec![Sweep::new(1, radials)],
    )
}

/// The render of `product` over `scan`, as the still path makes it.
fn render(scan: &Scan, product: RadarProduct) -> crate::render::SweepRender {
    crate::render::render_radar_to_image(scan, ELEVATION, product, LAT, LON)
        .expect("the fixture carries this product")
}

#[test]
fn a_loop_frame_reads_the_same_number_the_render_painted() {
    let scan = std::sync::Arc::new(volume(360, false));
    let out = render(&scan, RadarProduct::Reflectivity);
    let gates = SweepGates::new(
        std::sync::Arc::clone(&scan),
        RadarProduct::Reflectivity,
        ELEVATION,
        crate::scan_size::scan_bytes(&scan),
    )
    .expect("reflectivity is a wire moment and the fixture carries it");

    let geom = out.polar.geometry();
    let mut painted = 0u32;
    let mut blank = 0u32;
    for radial in 0..geom.radials() {
        for gate in 0..geom.gates() {
            let at = GateAt { radial, gate };
            let from_render = out.polar.at(at);
            let from_volume = gates.at(at);
            assert_eq!(
                from_render.map(f32::to_bits),
                from_volume.map(f32::to_bits),
                "gate ({radial}, {gate})"
            );
            if from_render.is_some() {
                painted += 1;
            } else {
                blank += 1;
            }
        }
    }
    assert!(painted > 100_000, "only {painted} gates carried a value");
    assert!(
        blank > 0,
        "the fixture never exercised below-threshold or range-folded"
    );
}

#[test]
fn a_looping_pane_and_a_still_pane_read_one_point_alike() {
    let scan = std::sync::Arc::new(volume(360, false));
    let out = render(&scan, RadarProduct::Reflectivity);
    let gates = SweepGates::new(
        std::sync::Arc::clone(&scan),
        RadarProduct::Reflectivity,
        ELEVATION,
        crate::scan_size::scan_bytes(&scan),
    )
    .unwrap();

    let still = HoverSource::resident(out.polar.clone());
    let mut looping_field = out.polar;
    looping_field.strip_values();
    let looping = HoverSource::from_volume(looping_field, Some(gates));

    let mut checked = 0u32;
    let mut values = 0u32;
    let mut az = 0.4f64;
    while az < 360.0 {
        let mut km = 0.3f64;
        while km < 110.0 {
            let a = still.read(az, km);
            let b = looping.read(az, km);
            assert_eq!(a, b, "({az}°, {km} km)");
            assert_ne!(
                b,
                Reading::NotResident,
                "({az}°, {km} km): the volume is right there"
            );
            if matches!(a, Reading::Value(_)) {
                values += 1;
            }
            checked += 1;
            km *= 1.05;
        }
        az += 1.7;
    }
    assert!(checked > 10_000, "only {checked} points");
    assert!(values > 5_000, "only {values} of them carried a number");
}

#[test]
fn a_loop_frame_of_a_derived_product_says_its_numbers_are_not_resident() {
    let scan = std::sync::Arc::new(volume(360, true));
    assert!(
        SweepGates::new(
            std::sync::Arc::clone(&scan),
            RadarProduct::NormalizedRotation,
            ELEVATION,
            crate::scan_size::scan_bytes(&scan),
        )
        .is_none(),
        "shear is computed, not measured"
    );

    let out = render(&scan, RadarProduct::Velocity);
    let mut field = out.polar;
    field.strip_values();
    let looping = HoverSource::from_volume(field, None);

    let mut said_not_resident = 0u32;
    let mut az = 0.5f64;
    while az < 360.0 {
        let mut km = 1.0f64;
        while km < 90.0 {
            if looping.read(az, km) == Reading::NotResident {
                said_not_resident += 1;
            }
            km *= 1.2;
        }
        az += 3.0;
    }
    assert!(
        said_not_resident > 1000,
        "only {said_not_resident} points reported their numbers missing"
    );
}

#[test]
fn every_product_is_its_own_moment_or_is_derived() {
    use RadarProduct::*;
    let wire = [
        Reflectivity,
        Velocity,
        SpectrumWidth,
        DifferentialReflectivity,
        CorrelationCoefficient,
        DifferentialPhase,
    ];
    for &p in RadarProduct::all() {
        let is_wire = wire.contains(&p);
        assert_eq!(
            p.is_wire_moment(),
            is_wire,
            "{p:?} is classified wrong for a readout"
        );
        if is_wire {
            assert_eq!(
                p.moment_slot().map(|s| s.product()),
                Some(p),
                "{p:?} must round-trip through its own slot"
            );
        } else {
            assert_ne!(
                p.moment_slot().map(|s| s.product()),
                Some(p),
                "{p:?} is derived and must not round-trip"
            );
        }
    }
}

#[test]
fn the_indexed_gate_decode_is_the_models_own_element_for_element() {
    for word_size in [8u8, 16] {
        for scale in [SCALE, 0.0, 0.5, -1.5] {
            let step = usize::from(word_size / 8);
            let bytes: Vec<u8> = (0..=255u8).chain(0..=255u8).collect();
            let gates = bytes.len() / step;
            let moment = MomentData::from_fixed_point(
                (gates * 2) as u16,
                0,
                (GATE_KM * 1000.0) as u16,
                word_size,
                scale,
                OFFSET,
                bytes,
            );
            for gate in 0..gates + 2 {
                assert_eq!(
                    crate::render::moment_value_at(&moment, gate),
                    moment.iter().nth(gate),
                    "word size {word_size}, scale {scale}, gate {gate}",
                );
            }
        }
    }
}

#[test]
fn the_hover_lookup_does_not_walk_the_gates() {
    const NARROW: usize = 200;
    const WIDE: usize = 1832;

    let scan = std::sync::Arc::new(volume(720, false));
    let sources = |gates: usize| {
        let out = render(&scan, RadarProduct::Reflectivity);
        let geom = out.polar.geometry();
        let wedges = geom.wedges().to_vec();
        let g = crate::render::polar::PolarGeometry::from_parts(
            wedges,
            geom.first_gate_slant_km(),
            geom.gate_interval_slant_km(),
            geom.elevation_deg(),
            gates,
        );
        let values = (0..720 * gates).map(|i| (i % 97) as f32).collect();
        HoverSource::resident(crate::render::polar::PolarField::from_parts(g, values))
    };
    let narrow = sources(NARROW);
    let wide = sources(WIDE);
    let volume_backed = {
        let out = render(&scan, RadarProduct::Reflectivity);
        let mut field = out.polar;
        field.strip_values();
        HoverSource::from_volume(
            field,
            SweepGates::new(
                std::sync::Arc::clone(&scan),
                RadarProduct::Reflectivity,
                ELEVATION,
                crate::scan_size::scan_bytes(&scan),
            ),
        )
    };

    let probes: Vec<(f64, f64)> = (0..64)
        .map(|i| (i as f64 * 5.6 + 0.3, 3.0 + i as f64 * 0.7))
        .collect();

    let picked: Vec<GateAt> = probes
        .iter()
        .filter_map(|&(az, km)| narrow.geometry().pick(az, km))
        .collect();
    assert_eq!(picked.len(), probes.len(), "a probe landed off the picture");
    let deepest = picked.iter().map(|at| at.gate).max().expect("64 probes");
    assert!(
        deepest > NARROW / 2,
        "the probes reach only gate {deepest} of {NARROW}, which a walk to the \
         gate would answer nearly as cheaply as an index",
    );

    // The gates one pass of the probes reads out of `src`, with the tally taken
    // fresh first so that nothing the fixture did is counted in it, and the
    // values summed so that nothing here is a call the optimiser can drop.
    let hover_over = |src: &HoverSource| {
        let _ = take_gate_reads();
        let mut sink = 0u32;
        for &(az, km) in &probes {
            if let Reading::Value(v) = src.read(az, km) {
                sink = sink.wrapping_add(v.to_bits());
            }
        }
        (take_gate_reads(), sink)
    };

    for (src, name) in [
        (&narrow, "resident, 200 gates"),
        (&wide, "resident, 1832 gates"),
        (&volume_backed, "from the volume"),
    ] {
        let (read, sink) = hover_over(src);
        assert!(sink != 0, "{name}: no probe read a value at all");
        let hovers = probes.len() as u64;
        assert!(
            read <= hovers,
            "{name}: {read} gates read for {hovers} hovers — the lookup is \
             walking the radial rather than indexing into it (the deepest probe \
             is at gate {deepest}, and a walk to it reads that many)",
        );
        assert_eq!(
            read, hovers,
            "{name}: {read} gates read for {hovers} hovers — the readout \
             reached its gate without passing through a counted accessor, so \
             this test is no longer measuring anything. Check what \
             `note_gate_reads` is still called from.",
        );
    }
}

/// A volume of `n_sweeps` cuts, each `n_radials` radials of `n_gates` gates
/// carrying reflectivity **and** velocity — the shape a real VCP volume has,
/// so the pinned-volume figure below is a real number of megabytes rather
/// than a token one. Every cut declares `ELEVATION` so
/// [`crate::render::sweep_index_for`] resolves against it.
fn sized_volume(n_sweeps: usize, n_radials: usize, n_gates: usize) -> Scan {
    let sweeps = (0..n_sweeps)
        .map(|s| {
            let radials = (0..n_radials)
                .map(|i| {
                    let bytes = |k: usize| {
                        (0..n_gates)
                            .map(|g| ((i * 7 + g * 3 + s * 11 + k) % 254) as u8)
                            .collect::<Vec<u8>>()
                    };
                    let moment = |k: usize| {
                        MomentData::from_fixed_point(
                            n_gates as u16,
                            0,
                            (GATE_KM * 1000.0) as u16,
                            8,
                            SCALE,
                            OFFSET,
                            bytes(k),
                        )
                    };
                    Radial::new(
                        0,
                        i as u16,
                        i as f32,
                        1.0,
                        RadialStatus::IntermediateRadialData,
                        1,
                        ELEVATION,
                        Some(moment(0)),
                        Some(moment(1)),
                        None,
                        None,
                        None,
                        None,
                        None,
                    )
                })
                .collect();
            Sweep::new(s as u8 + 1, radials)
        })
        .collect();
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
            Vec::new(),
        ),
        sweeps,
    )
}

/// **A loop frame prices the decoded volume it pins, not just its geometry.**
///
/// `HoverSource::from_volume` keeps the `Arc<Scan>` the frame was drawn from
/// alive so the readout can decode a gate on demand, and `resident_bytes`
/// once reported the polar field alone — so a frame holding megabytes of
/// decoded radar priced at the few KB of its wedge table, invisible to the
/// census and to every budget that evicts on bytes.
///
/// **This asserts against the volume's own measured size, not `> 0`.** A
/// `> 0` assertion passed the broken code, because the geometry was never
/// zero; what makes this a gate is the multiple. The figures are compared to
/// `scan_size::scan_bytes` — the same function the loop cache prices with —
/// rather than to a recorded constant, so a change to what a `Scan` holds
/// moves both sides together and this test stays about the accounting.
#[test]
fn a_loop_frame_prices_the_volume_it_pins() {
    // 4 cuts × 720 radials × 1000 gates × 2 moments ≈ 5.8 MB of gate bytes.
    let scan = std::sync::Arc::new(sized_volume(4, 720, 1000));
    let volume_bytes = crate::scan_size::scan_bytes(&scan);
    assert!(
        volume_bytes > 5_000_000,
        "the fixture must be volume-shaped to be worth pricing; it is {volume_bytes} B"
    );

    let out = render(&scan, RadarProduct::Reflectivity);
    let mut field = out.polar;
    // A loop frame's field carries no values: the numbers come back out of
    // the volume. This is the exact shape the undercount hid in.
    field.strip_values();
    let field_bytes = crate::render::polar::PolarField::resident_bytes(&field);

    let looping = HoverSource::from_volume(
        field,
        SweepGates::new(
            std::sync::Arc::clone(&scan),
            RadarProduct::Reflectivity,
            ELEVATION,
            volume_bytes,
        ),
    );

    assert_eq!(
        looping.pinned_volume_bytes(),
        volume_bytes,
        "the pinned volume must price at what the volume holds"
    );
    assert_eq!(
        looping.resident_bytes(),
        field_bytes + volume_bytes,
        "the reported cost must be the field AND the volume it pins"
    );
    // The defect, stated as a ratio so it cannot be satisfied by a token
    // addend: the volume is three orders of magnitude past the geometry.
    assert!(
        looping.resident_bytes() > 100 * field_bytes,
        "a frame pinning {volume_bytes} B priced at {} B, barely past its \
         {field_bytes} B of geometry — the volume is not in the figure",
        looping.resident_bytes()
    );
    assert!(
        looping.resident_bytes() >= 5_000_000,
        "a frame pinning a multi-megabyte volume priced at {} B",
        looping.resident_bytes()
    );
}

/// **The other direction: a source that legitimately pins nothing stays
/// small.** A still pane's source keeps its own values and holds no volume,
/// so its cost is its field and exactly its field. A fix that priced every
/// hover source as if it held a scan would over-report the render cache's
/// budget and evict pictures that fit — the worse failure of the two, and
/// the one a gate proven only against the defect would not catch.
#[test]
fn a_source_holding_no_volume_prices_only_its_field() {
    let scan = std::sync::Arc::new(sized_volume(4, 720, 1000));
    let out = render(&scan, RadarProduct::Reflectivity);
    let field_bytes = crate::render::polar::PolarField::resident_bytes(&out.polar);

    let still = HoverSource::resident(out.polar);
    assert_eq!(
        still.pinned_volume_bytes(),
        0,
        "a still pane pins no volume"
    );
    assert_eq!(
        still.resident_bytes(),
        field_bytes,
        "a source over a resident field costs its field and nothing else"
    );
    // And the volume it was rendered from is NOT in the figure: the render
    // read the scan, it did not retain it.
    assert!(
        still.resident_bytes() < crate::scan_size::scan_bytes(&scan),
        "a still source priced as if it held the volume it was drawn from"
    );

    // The empty source is the floor: no field, no volume, no bytes.
    assert_eq!(HoverSource::empty().pinned_volume_bytes(), 0);

    // A loop frame whose product the volume cannot answer for holds no
    // `SweepGates` at all — `from_volume` with `None` — and must price as
    // small as the still source, not as a volume.
    let out = render(&scan, RadarProduct::Reflectivity);
    let mut stripped_field = out.polar;
    stripped_field.strip_values();
    let stripped_bytes = crate::render::polar::PolarField::resident_bytes(&stripped_field);
    let volume_less = HoverSource::from_volume(stripped_field, None);
    assert_eq!(
        volume_less.pinned_volume_bytes(),
        0,
        "a volume-less loop source must not be charged for a volume"
    );
    assert_eq!(volume_less.resident_bytes(), stripped_bytes);
}
