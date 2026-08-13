//! What a loop frame reads, against what the still render painted.
//!
//! The one claim that matters here is that the two agree gate for gate. A
//! looping pane and a still pane show the same sweep through two different
//! value sources — the render's own numbers, and the volume it was rendered
//! from — and a reader must not be able to tell which one answered.

use super::*;
use crate::render::polar::GateAt;
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

/// A one-sweep volume of `n_radials` at 1° spacing whose gate bytes vary in
/// both axes, so that a wrong radial and a wrong gate are both visible in the
/// number rather than aliasing onto the right one.
///
/// Byte 0 decodes as below-threshold and byte 1 as range-folded, and both are
/// deliberately in range: they are the two states the readout has to answer
/// `None` for, and a fixture of nothing but ordinary values would never reach
/// either arm of [`crate::render::painted_moment_value`].
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

/// **A loop frame's readout is the still pane's readout, gate for gate.**
///
/// This is what makes the two value sources one answer. A still pane reads the
/// numbers the render kept; a loop frame reads them back out of the volume it
/// was rendered from, because keeping 5.03 MiB per frame is not affordable at
/// 14 frames in a browser. If those ever disagreed, the same pane would print
/// one number stopped and another looping, over the same pixel of the same
/// sweep.
///
/// Every gate of every radial, not a sample: the disagreements this is guarding
/// against are the ones that show up on particular decoded values — the
/// out-of-scale codes, the range-folded sentinel, below threshold — and a
/// sampled walk can step over all three.
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

/// The two sources answer a *point* alike too, which is the question the pane
/// actually asks.
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

/// **A loop frame of a derived product says so rather than going blank.**
///
/// The failing half of the loop fix, and the reason [`Reading::NotResident`]
/// exists. Normalized rotation is computed from velocity, so the volume behind
/// the picture cannot answer for it — but the picture is *there*, and a reader
/// hovering it is owed the difference between "the radar found nothing here"
/// and "this frame's numbers were not kept". Before this, both were a blank
/// readout.
#[test]
fn a_loop_frame_of_a_derived_product_says_its_numbers_are_not_resident() {
    let scan = std::sync::Arc::new(volume(360, true));
    // A volume of velocity cannot build a `SweepGates` for shear, whatever
    // else is true of it.
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

    // Somewhere the render definitely painted.
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

/// Every product is either its own moment or is computed from one, and the
/// eleven computed ones must not be read straight out of a volume.
///
/// The classification [`SweepGates::new`] gates on. Walked over the whole enum
/// so a product added later is classified deliberately rather than by whichever
/// arm of `moment_slot` it happened to land in — the failure mode being a
/// readout that prints metres per second under a shear colour scale.
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

/// **The readout does not walk the gates, which is what would make it too
/// expensive to run on pointer motion.**
///
/// It runs on the frame thread, every frame the pointer is over a pane. The
/// regression that matters is a lookup that became `O(gates)` — a linear search
/// along the radial instead of the one division
/// [`crate::render::polar::PolarGeometry::pick`] does, or a decode of every
/// gate up to the one asked for — because that is a real temptation on the
/// volume-backed path, where the gate has to come out of a `MomentData`.
///
/// So this measures the **shape** rather than the clock: two fields with the
/// same 720 radials and gate counts nine times apart, timed against each other.
/// A lookup that is flat in the gate count answers both alike; one that walks
/// them is nine times slower on the wider field. The arms are interleaved and
/// the *minimum* per-call time is taken, which is what makes the comparison
/// survive a 32-core box that is compiling this workspace while it runs — the
/// same method `POOLED_CELLS` uses for the same reason.
///
/// The absolute figures, measured on this box with nothing else running:
/// **832 ns** per hover at 200 gates, **832 ns** at 1832 and **851 ns** reading
/// out of the volume, `--release`; 3.03 / 3.03 / 5.33 µs unoptimized. They are
/// printed rather than asserted, because a bound tight enough to mean anything
/// is a bound that fails for reasons that have nothing to do with the code —
/// this one did, at 25 µs, on a box compiling the workspace beside it.
#[test]
fn the_hover_lookup_does_not_walk_the_gates() {
    const NARROW: usize = 200;
    const WIDE: usize = 1832;
    const ROUNDS: u32 = 60;

    let scan = std::sync::Arc::new(volume(720, false));
    let sources = |gates: usize| {
        // A field of `gates` gates over the fixture's own wedges, so the two
        // arms differ in exactly one dimension.
        let out = render(&scan, RadarProduct::Reflectivity);
        let geom = out.polar.geometry();
        let wedges = geom.wedges().to_vec();
        let g = crate::render::polar::PolarGeometry::from_parts(
            wedges,
            geom.first_gate_km(),
            geom.gate_interval_km(),
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
    let time = |src: &HoverSource| {
        let mut sink = 0u32;
        let start = std::time::Instant::now();
        for &(az, km) in &probes {
            if let Reading::Value(v) = src.read(az, km) {
                sink = sink.wrapping_add(v.to_bits());
            }
        }
        (start.elapsed(), sink)
    };

    let mut best = [std::time::Duration::MAX; 3];
    let mut sink = 0u32;
    for round in 0..ROUNDS {
        // Alternating order, so neither arm always runs into a cold cache.
        let order: [usize; 3] = if round % 2 == 0 { [0, 1, 2] } else { [2, 1, 0] };
        for i in order {
            let (d, s) = time([&narrow, &wide, &volume_backed][i]);
            best[i] = best[i].min(d);
            sink = sink.wrapping_add(s);
        }
    }
    assert!(sink != 0, "the loop was optimised away");

    let each = |d: std::time::Duration| d / probes.len() as u32;
    eprintln!(
        "per hover over 720 wedges: {NARROW} gates {:?}, {WIDE} gates {:?}, \
         from the volume {:?}",
        each(best[0]),
        each(best[1]),
        each(best[2]),
    );

    // Nine times the gates must not cost anything like nine times the time.
    // Two is generous against a flat lookup and far under a walking one.
    for (i, name) in [(1usize, "resident"), (2, "from the volume")] {
        assert!(
            best[i] < best[0] * 2,
            "{name}: {:?} against {:?} for {}x fewer gates — the lookup is \
             walking the radial rather than indexing into it",
            each(best[i]),
            each(best[0]),
            WIDE / NARROW,
        );
    }
}
