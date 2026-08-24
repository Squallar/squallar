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
            ELEVATION
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
