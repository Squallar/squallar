use super::*;
use crate::sampler::{Sample, SampleStatus, samplable};
use nexrad_model::data::{
    ChannelConfiguration, ElevationCut, MomentData, PulseWidth, Radial, RadialStatus, Scan, Sweep,
    VolumeCoveragePattern, WaveformType,
};

// ── Fixtures ────────────────────────────────────────────────────────────
//
// Built to *fail* a wrong implementation, per the sampler's own
// experience: its first mutation pass left 13 survivors and every one was
// a fixture too tidy to discriminate. So nothing here is symmetric or
// tidy —
//
//  * sweeps arrive high cut first, so a builder trusting collection order
//    gets the ladder upside down;
//  * azimuths start away from 0 and wrap through it in collection order;
//  * the two tilts carry different radial counts (720 super-res below,
//    360 above), so a test that only ever reads the low tilt proves
//    nothing about the high one;
//  * the upper tilt's gates stop short, so range truncation is reachable;
//  * fields vary along **both** azimuth and range, because a field
//    constant along range cannot tell a half-cell offset from a correct
//    one;
//  * and the boundary fixtures plant a **sharp echo edge** — below
//    threshold on one side, 65 dBZ on the other — because a fixture where
//    every voxel has data cannot test the behaviour that motivates the
//    whole encoding decision. The tests that need one assert that it is
//    there before relying on it.

const REFL_SCALE: f32 = 2.0;
const REFL_OFFSET: f32 = 66.0;
/// Operational super-resolution first-gate *centre*. Nonzero on purpose: a
/// builder that forgot it is 2 km inward everywhere and still passes any
/// fixture whose gates start at the origin.
const FIRST_GATE_M: u16 = 2125;
const GATE_M: u16 = 250;

/// KTLX, whose elevation `eet`'s own test pins at 1213 ft.
const SITE: (f64, f64) = (35.33306, -97.2775);
const SITE_ELEV_FT: f64 = 1213.0;

fn encode_refl(dbz: f64) -> u8 {
    ((dbz * f64::from(REFL_SCALE) + f64::from(REFL_OFFSET)).round() as i64).clamp(2, 255) as u8
}

/// What `encode_refl` round-trips to, so a 0.5 dB quantisation step is not
/// mistaken for a builder error.
fn round_trip_refl(dbz: f64) -> f32 {
    (f32::from(encode_refl(dbz)) - REFL_OFFSET) / REFL_SCALE
}

fn gate_slant_km(j: usize) -> f64 {
    f64::from(FIRST_GATE_M) / 1000.0 + j as f64 * f64::from(GATE_M) / 1000.0
}

/// dBZ at an azimuth and slant range, or `None` for below threshold — the
/// no-data half of every edge in this module.
type Field<'f> = &'f dyn Fn(f64, f64) -> Option<f64>;

/// One reflectivity sweep, azimuths given explicitly in **collection**
/// order.
fn refl_sweep(
    elevation_number: u8,
    elevation_deg: f32,
    azimuths: &[f32],
    n_gates: usize,
    field: Field<'_>,
) -> Sweep {
    let spacing = 360.0 / azimuths.len() as f32;
    let radials = azimuths
        .iter()
        .enumerate()
        .map(|(i, &az)| {
            let bytes: Vec<u8> = (0..n_gates)
                .map(|j| match field(f64::from(az), gate_slant_km(j)) {
                    None => 0,
                    Some(v) => encode_refl(v),
                })
                .collect();
            Radial::new(
                0,
                i as u16,
                az,
                spacing,
                RadialStatus::IntermediateRadialData,
                elevation_number,
                elevation_deg,
                Some(MomentData::from_fixed_point(
                    bytes.len() as u16,
                    FIRST_GATE_M,
                    GATE_M,
                    8,
                    REFL_SCALE,
                    REFL_OFFSET,
                    bytes,
                )),
                None,
                None,
                None,
                None,
                None,
                None,
            )
        })
        .collect();
    Sweep::new(elevation_number, radials)
}

/// One velocity sweep over `field`, m/s through the (2, 129) codec —
/// the fixture for the fold-guard test, shaped like [`refl_sweep`].
fn vel_sweep(
    elevation_number: u8,
    elevation_deg: f32,
    azimuths: &[f32],
    n_gates: usize,
    field: Field<'_>,
) -> Sweep {
    const VEL_SCALE: f32 = 2.0;
    const VEL_OFFSET: f32 = 129.0;
    let spacing = 360.0 / azimuths.len() as f32;
    let radials = azimuths
        .iter()
        .enumerate()
        .map(|(i, &az)| {
            let bytes: Vec<u8> = (0..n_gates)
                .map(|j| match field(f64::from(az), gate_slant_km(j)) {
                    None => 0,
                    Some(v) => ((v * f64::from(VEL_SCALE) + f64::from(VEL_OFFSET)).round() as i64)
                        .clamp(2, 255) as u8,
                })
                .collect();
            Radial::new(
                0,
                i as u16,
                az,
                spacing,
                RadialStatus::IntermediateRadialData,
                elevation_number,
                elevation_deg,
                None,
                Some(MomentData::from_fixed_point(
                    bytes.len() as u16,
                    FIRST_GATE_M,
                    GATE_M,
                    8,
                    VEL_SCALE,
                    VEL_OFFSET,
                    bytes,
                )),
                None,
                None,
                None,
                None,
                None,
            )
        })
        .collect();
    Sweep::new(elevation_number, radials)
}

/// Azimuths in collection order: `n` of them, starting at `start` and
/// wrapping through 0.
fn wrapped_azimuths(n: usize, start: f64) -> Vec<f32> {
    let step = 360.0 / n as f64;
    (0..n)
        .map(|i| (start + i as f64 * step).rem_euclid(360.0) as f32)
        .collect()
}

fn cut(angle_deg: f64) -> ElevationCut {
    ElevationCut::new(
        angle_deg,
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
    )
}

fn vcp(cut_angles: &[f64]) -> VolumeCoveragePattern {
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
        cut_angles.iter().copied().map(cut).collect(),
    )
}

/// The two elevations every fixture below flies, and the numbers the tests
/// compute expected heights from.
const LOW_DEG: f32 = 0.53;
const HIGH_DEG: f32 = 4.47;
const LOW_GATES: usize = 600; // to 151.9 km slant
const HIGH_GATES: usize = 200; // stops at 51.9 km — range truncation

/// A two-tilt volume over `field`.
fn scan_of(field: Field<'_>) -> Scan {
    Scan::new(
        vcp(&[0.5, 4.5]),
        vec![
            refl_sweep(
                2,
                HIGH_DEG,
                &wrapped_azimuths(360, 211.0),
                HIGH_GATES,
                field,
            ),
            refl_sweep(1, LOW_DEG, &wrapped_azimuths(720, 293.5), LOW_GATES, field),
        ],
    )
}

/// A two-tilt volume carrying **all six** moments, so the per-product
/// tests build real populated grids rather than tables in isolation.
///
/// Every moment gets its own raw byte per gate through its **own** scale
/// and offset, which is how a real radial is laid out and is why a builder
/// that reached for the wrong moment slot reads a plausible number in the
/// wrong units rather than nothing at all.
fn six_moment_scan() -> Scan {
    // (scale, offset) per moment, from the ICD; the same pairs
    // `no_measurement_encodes_as_the_no_data_index` restates.
    const CODECS: [(f32, f32); 6] = [
        (2.0, 66.0),    // reflectivity
        (2.0, 129.0),   // velocity
        (2.0, 129.0),   // spectrum width
        (16.0, 128.0),  // ZDR
        (2.8361, 2.0),  // PhiDP
        (300.0, -60.5), // rho HV
    ];
    let sweep = |elevation_number: u8, elevation_deg: f32, start: f64, n_gates: usize| {
        let azimuths = wrapped_azimuths(360, start);
        let spacing = 360.0 / azimuths.len() as f32;
        let radials = azimuths
            .iter()
            .enumerate()
            .map(|(i, &az)| {
                // Not constant along range, and a different code in every
                // moment: a wrong slot is then a wrong number rather than
                // a lucky match.
                let moment = |slot: usize| {
                    let (scale, offset) = CODECS[slot];
                    // The lowest raw code each moment's encoding actually
                    // carries. Spectrum width shares velocity's (2, 129)
                    // codec but is non-negative, so the RDA never emits a
                    // code under 129 for it; a fixture that did would be
                    // testing the out-of-span clamp rather than the
                    // builder, and the clamp has its own test.
                    let floor = usize::from(if slot == 2 { 129u8 } else { 2 });
                    let bytes: Vec<u8> = (0..n_gates)
                        .map(|j| (floor + ((j * 7 + slot * 31 + i) % (256 - floor))) as u8)
                        .collect();
                    Some(MomentData::from_fixed_point(
                        bytes.len() as u16,
                        FIRST_GATE_M,
                        GATE_M,
                        8,
                        scale,
                        offset,
                        bytes,
                    ))
                };
                Radial::new(
                    0,
                    i as u16,
                    az,
                    spacing,
                    RadialStatus::IntermediateRadialData,
                    elevation_number,
                    elevation_deg,
                    moment(0),
                    moment(1),
                    moment(2),
                    moment(3),
                    moment(4),
                    moment(5),
                    None,
                )
            })
            .collect();
        Sweep::new(elevation_number, radials)
    };
    Scan::new(
        vcp(&[0.5, 4.5]),
        vec![
            sweep(1, LOW_DEG, 117.5, LOW_GATES),
            sweep(2, HIGH_DEG, 41.0, HIGH_GATES),
        ],
    )
}

/// Three rungs that all reach 100 km, with reflectivity above threshold on
/// **exactly one** of them (or none).
///
/// The other two carry the moment and report below threshold, so the
/// ladder still has three rungs — which is the whole point: the question
/// is what a *measured* layer on one rung does to its neighbours, not what
/// a one-rung ladder does.
fn one_rung_carries_data(carrier: Option<usize>) -> Scan {
    let full: Field<'_> = &|_, _| Some(45.0);
    let empty: Field<'_> = &|_, _| None;
    // Medians deliberately off their nominal cuts, as real ones are.
    let medians = [0.53f32, 2.47, 4.51];
    let sweeps = (0..3)
        .map(|i| {
            refl_sweep(
                (i + 1) as u8,
                medians[i],
                &wrapped_azimuths(360, 137.0 + i as f64),
                LOW_GATES,
                if carrier == Some(i) { full } else { empty },
            )
        })
        .collect();
    Scan::new(vcp(&[0.5, 2.5, 4.5]), sweeps)
}

/// A scan whose coverage pattern has no cuts — what a scan reconstructed
/// from a `RenderInput` looks like.
fn placeholder_scan() -> Scan {
    Scan::new(
        vcp(&[]),
        vec![refl_sweep(
            1,
            LOW_DEG,
            &wrapped_azimuths(360, 0.0),
            LOW_GATES,
            &|_, _| Some(30.0),
        )],
    )
}

fn request(shape: VoxelShape) -> VoxelRequest {
    VoxelRequest {
        centre: SITE,
        half_width_km: 60.0,
        base_km_msl: 0.0,
        top_km_msl: 12.0,
        product: RadarProduct::Reflectivity,
        shape,
        values_wanted: true,
    }
}

/// A shape with three **different** axes, so a transposed index cannot
/// pass by accident.
const ODD: VoxelShape = VoxelShape {
    nx: 11,
    ny: 13,
    nz: 7,
};

/// Every moment a grid can be built for.
const SLOTS: [MomentSlot; 6] = [
    MomentSlot::Reflectivity,
    MomentSlot::Velocity,
    MomentSlot::SpectrumWidth,
    MomentSlot::DifferentialReflectivity,
    MomentSlot::DifferentialPhase,
    MomentSlot::CorrelationCoefficient,
];

/// The products those moments belong to, in the same order.
///
/// Use this **only** where the loop is about a Level II *moment* — an
/// encoding, a slot, a wire codec. Anything that is about a *product* —
/// a table, a profile, an isosurface, a round trip — must loop over
/// [`VOLUME_PRODUCTS`], which is three entries longer.
const SAMPLABLE: [RadarProduct; 6] = [
    RadarProduct::Reflectivity,
    RadarProduct::Velocity,
    RadarProduct::SpectrumWidth,
    RadarProduct::DifferentialReflectivity,
    RadarProduct::DifferentialPhase,
    RadarProduct::CorrelationCoefficient,
];

/// The three products the vertical views **derive** rather than sample.
const DERIVED: [RadarProduct; 3] = [
    RadarProduct::StormRelativeVelocity,
    RadarProduct::NormalizedRotation,
    RadarProduct::SpecificDifferentialPhase,
];

/// Every product a voxel grid can be built for: the six native moments
/// then the three derivations.
///
/// This constant exists because of the exact defect it now closes. The
/// commit that admitted SRV, NROT and KDP to every 3D surface — LUT
/// profile, isosurface shape, isosurface default, cache keys, UI gates —
/// left every product loop in this module at the six natives, so three
/// products shipped with no table coverage, no profile coverage and no
/// isosurface coverage whatsoever. NROT then shipped rendering 8 033 of
/// 8 039 painted voxels at alpha 2–4 of 180, and nothing went red.
/// [`the_product_loops_cover_every_product_the_vertical_views_admit`]
/// makes the next such admission fail here before it can ship.
const VOLUME_PRODUCTS: [RadarProduct; 9] = [
    RadarProduct::Reflectivity,
    RadarProduct::Velocity,
    RadarProduct::SpectrumWidth,
    RadarProduct::DifferentialReflectivity,
    RadarProduct::DifferentialPhase,
    RadarProduct::CorrelationCoefficient,
    RadarProduct::StormRelativeVelocity,
    RadarProduct::NormalizedRotation,
    RadarProduct::SpecificDifferentialPhase,
];

/// The ramp a product's colour table is built over, keyed by **product**
/// — which is what [`build_voxels`] itself does.
///
/// The old spelling, `value_range_for(samplable(product).unwrap())`, could
/// not answer for a derivation at all: it panics on all three, and two of
/// them carry their own spans ([`data_levels_for`]) rather than their
/// source slot's. NROT borrows the velocity slot and spans ±4 unitless
/// where velocity spans ±63.5 m/s; a test that reached for the slot's
/// range would be measuring a table nothing builds.
fn ramp_of(product: RadarProduct) -> (f32, f32) {
    value_range_for_product(product, crate::derive::volume_slot(product).unwrap())
}

/// The product loops in this module cover **everything** the vertical
/// views admit — the guard that makes widening the product set impossible
/// to do quietly.
///
/// `derive::volume_slot` is the single predicate every vertical view gates
/// on (the 3D pane, the section pane, the volume-alpha editor, the grid
/// builder itself). If a product joins that set without joining
/// [`VOLUME_PRODUCTS`], it renders in the app and is exercised by nothing
/// here, which is precisely the state SRV, NROT and KDP shipped in.
#[test]
fn the_product_loops_cover_every_product_the_vertical_views_admit() {
    let admitted: Vec<RadarProduct> = RadarProduct::all()
        .iter()
        .copied()
        .filter(|p| crate::derive::volume_slot(*p).is_some())
        .collect();
    let mut covered = VOLUME_PRODUCTS.to_vec();
    covered.sort_by_key(|p| p.code());
    let mut want = admitted.clone();
    want.sort_by_key(|p| p.code());
    assert_eq!(
        covered, want,
        "VOLUME_PRODUCTS is not the set `derive::volume_slot` admits; a \
             product that renders in a vertical view is covered by no product \
             loop in this module",
    );
    // And the two halves really are a partition, so `SAMPLABLE` cannot be
    // quietly re-pointed at the whole set and the distinction lost.
    for product in SAMPLABLE {
        assert!(samplable(product).is_some(), "{}", product.name());
    }
    for product in DERIVED {
        assert!(
            samplable(product).is_none() && crate::derive::volume_slot(product).is_some(),
            "{} is not a derivation",
            product.name(),
        );
    }
    assert_eq!(SAMPLABLE.len() + DERIVED.len(), VOLUME_PRODUCTS.len());
}

// ── Shapes, budget and the target default ───────────────────────────────

#[test]
fn every_named_shape_fits_the_texture_budget() {
    for (name, shape) in [
        ("wasm", WASM_SHAPE),
        ("mobile", MOBILE_SHAPE),
        ("desktop", DESKTOP_SHAPE),
    ] {
        assert!(
            shape.is_supported(),
            "{name} has an axis outside 1..={MAX_AXIS}",
        );
        assert!(
            shape.cells() <= VOXEL_TEXTURE_BUDGET_BYTES,
            "{name} needs {} bytes of index plane against a \
                 {VOXEL_TEXTURE_BUDGET_BYTES} byte budget",
            shape.cells(),
        );
    }
}

/// The module doc's memory table, as arithmetic rather than as prose.
#[test]
fn the_named_shapes_cost_what_the_module_doc_says() {
    const MIB: usize = 1024 * 1024;
    assert_eq!(WASM_SHAPE.cells(), MIB, "wasm: 1 MiB of indices");
    assert_eq!(MOBILE_SHAPE.cells(), 3_538_944, "mobile: 3.375 MiB");
    assert_eq!(DESKTOP_SHAPE.cells(), 8 * MIB, "desktop: 8 MiB");
    // The value plane is four times the index plane, which is what makes
    // the desktop grid 40 MiB rather than 8.
    assert_eq!(DESKTOP_SHAPE.cells() * 4, 32 * MIB);
}

/// wasm gets the small shape, everything else the large one, and the
/// **mobile** shape is deliberately unreachable from here — see the module
/// doc.
#[test]
fn default_shape_is_the_targets() {
    #[cfg(target_arch = "wasm32")]
    assert_eq!(default_shape(), WASM_SHAPE);
    #[cfg(not(target_arch = "wasm32"))]
    assert_eq!(default_shape(), DESKTOP_SHAPE);
    assert_ne!(
        default_shape(),
        MOBILE_SHAPE,
        "this crate has no build script, so it cannot see the `mobile` \
             cfg; the frontend selects MOBILE_SHAPE explicitly",
    );
}

/// Both arms of the `cfg` cascade, from a host that compiles only one of
/// them.
///
/// The test above can only ever check the arm it was built for, so the
/// wasm arm's *content* would otherwise be checked by nothing that runs —
/// mutation testing found precisely that hole. See
/// [`default_shape_for`]'s doc.
#[test]
fn both_target_classes_get_their_own_default_shape() {
    assert_eq!(default_shape_for(true), WASM_SHAPE);
    assert_eq!(default_shape_for(false), DESKTOP_SHAPE);
    assert_ne!(default_shape_for(true), default_shape_for(false));
}

#[test]
fn an_axis_outside_the_guarantee_is_refused() {
    let scan = scan_of(&|_, _| Some(40.0));
    // Each axis independently, in both directions, so a guard that checks
    // only one of the three survives none of these.
    for bad in [
        VoxelShape { nx: 0, ..ODD },
        VoxelShape { ny: 0, ..ODD },
        VoxelShape { nz: 0, ..ODD },
        VoxelShape { nx: 257, ..ODD },
        VoxelShape { ny: 257, ..ODD },
        VoxelShape { nz: 257, ..ODD },
    ] {
        assert_eq!(
            build_voxels(&scan, &request(bad), SITE.0, SITE.1),
            None,
            "{bad:?} should be refused",
        );
    }
    assert!(
        build_voxels(
            &scan,
            &request(VoxelShape {
                nx: MAX_AXIS,
                ny: 1,
                nz: 1
            }),
            SITE.0,
            SITE.1,
        )
        .is_some(),
        "256 is the guarantee, so it is allowed",
    );
}

// ── Refusals ────────────────────────────────────────────────────────────

/// Two refusal kinds, distinguished on purpose. The integrals and the
/// classification have **no per-tilt field at all** — `derive::volume_slot`
/// refuses them on any volume. The derived products (SRV, NROT, KDP) have
/// one, but this fixture is reflectivity-only, so their derivation finds
/// no source moment and refuses **this volume** — the same products build
/// real grids on a velocity/ΦDP-carrying volume, which
/// `derive::tests::a_derived_voxel_grid_resamples_the_derived_field` pins.
#[test]
fn a_product_with_no_native_moment_is_refused() {
    let scan = scan_of(&|_, _| Some(40.0));
    for product in [
        RadarProduct::VerticallyIntegratedLiquid,
        RadarProduct::EchoTops,
        RadarProduct::HydrometeorClassification,
        RadarProduct::VilDensity,
    ] {
        let req = VoxelRequest {
            product,
            ..request(ODD)
        };
        assert_eq!(
            build_voxels(&scan, &req, SITE.0, SITE.1),
            None,
            "{} has no per-tilt field to resample, on any volume",
            product.name(),
        );
    }
    for product in [
        RadarProduct::NormalizedRotation,
        RadarProduct::StormRelativeVelocity,
        RadarProduct::SpecificDifferentialPhase,
    ] {
        let req = VoxelRequest {
            product,
            ..request(ODD)
        };
        assert_eq!(
            build_voxels(&scan, &req, SITE.0, SITE.1),
            None,
            "{} derives from a moment this volume does not carry",
            product.name(),
        );
    }
}

/// The refusal that keeps a render worker from silently building a
/// different ladder from the main thread's. Until WP-D carries the cut
/// angles on the wire, this is the only thing standing between the two.
#[test]
fn a_placeholder_coverage_pattern_is_refused() {
    let scan = placeholder_scan();
    assert_eq!(build_voxels(&scan, &request(ODD), SITE.0, SITE.1), None);
}

#[test]
fn a_non_finite_number_anywhere_is_refused() {
    let scan = scan_of(&|_, _| Some(40.0));
    let base = request(ODD);
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        // Every scalar independently: a guard covering six of the seven
        // survives whichever one it missed.
        let cases = [
            VoxelRequest {
                half_width_km: bad,
                ..base.clone()
            },
            VoxelRequest {
                base_km_msl: bad,
                ..base.clone()
            },
            VoxelRequest {
                top_km_msl: bad,
                ..base.clone()
            },
            VoxelRequest {
                centre: (bad, SITE.1),
                ..base.clone()
            },
            VoxelRequest {
                centre: (SITE.0, bad),
                ..base.clone()
            },
        ];
        for req in cases {
            assert_eq!(
                build_voxels(&scan, &req, SITE.0, SITE.1),
                None,
                "{req:?} carries {bad} and should be refused",
            );
        }
        assert_eq!(build_voxels(&scan, &base, bad, SITE.1), None, "site lat");
        assert_eq!(build_voxels(&scan, &base, SITE.0, bad), None, "site lon");
    }
}

#[test]
fn a_top_at_or_below_the_base_is_refused() {
    let scan = scan_of(&|_, _| Some(40.0));
    for (base_km_msl, top_km_msl) in [(5.0, 5.0), (5.0, 4.0)] {
        let req = VoxelRequest {
            base_km_msl,
            top_km_msl,
            ..request(ODD)
        };
        assert_eq!(build_voxels(&scan, &req, SITE.0, SITE.1), None);
    }
    let req = VoxelRequest {
        base_km_msl: 5.0,
        top_km_msl: 5.001,
        ..request(ODD)
    };
    assert!(build_voxels(&scan, &req, SITE.0, SITE.1).is_some());
}

/// A zoom control that runs out of travel should stop, not fail — so the
/// half-width clamps where everything else refuses.
#[test]
fn the_half_width_is_clamped_rather_than_refused() {
    let scan = scan_of(&|_, _| Some(40.0));
    for (asked, want) in [
        (0.0, MIN_HALF_WIDTH_KM),
        (1.0, MIN_HALF_WIDTH_KM),
        (-500.0, MIN_HALF_WIDTH_KM),
        (60.0, 60.0),
        (10_000.0, MAX_HALF_WIDTH_KM),
    ] {
        let req = VoxelRequest {
            half_width_km: asked,
            ..request(ODD)
        };
        let grid = build_voxels(&scan, &req, SITE.0, SITE.1)
            .unwrap_or_else(|| panic!("{asked} km should clamp, not refuse"));
        let (lo, hi) = grid.x_range_km();
        assert!(
            (hi - lo - 2.0 * want).abs() < 1e-9,
            "asked {asked} km, wanted a {want} km half-width, got {:?}",
            grid.x_range_km(),
        );
    }
}

// ── Orientation and cell centres ────────────────────────────────────────

/// x east, y north, z up — pinned with a quadrant field, on a shape whose
/// three axes are all different so a transposed index cannot pass.
#[test]
fn the_grid_is_indexed_x_east_y_north_z_up() {
    // 60 dBZ strictly inside the north-east quadrant, 15 elsewhere.
    let scan = scan_of(&|az, _| {
        Some(if (0.0..90.0).contains(&az) {
            60.0
        } else {
            15.0
        })
    });
    let shape = VoxelShape {
        nx: 21,
        ny: 23,
        nz: 5,
    };
    // The corner columns below sit at 43.7 km ground range, where the two
    // rungs bracket 0.517 … 3.529 km above the antenna. Rows are 1 km
    // apart from 1.0 km MSL, so row 1 (1.63 km over the antenna) is inside
    // the bracket and row 4 (4.63 km) is over the top of it.
    let req = VoxelRequest {
        half_width_km: 40.0,
        base_km_msl: 0.5,
        top_km_msl: 5.5,
        ..request(shape)
    };
    let grid = build_voxels(&scan, &req, SITE.0, SITE.1).unwrap();

    // Corner columns, well away from the quadrant boundaries: their
    // azimuths are 44.2°, 135.8°, 224.2° and 315.8°.
    let iz = 1;
    let (west, east) = (2, shape.nx - 3);
    let (south, north) = (2, shape.ny - 3);
    let strong = grid.value_at(east, north, iz).unwrap();
    assert!(
        (strong - round_trip_refl(60.0)).abs() < 0.05,
        "north-east should read the 60 dBZ quadrant, read {strong}",
    );
    for (x, y, corner) in [
        (west, north, "north-west"),
        (east, south, "south-east"),
        (west, south, "south-west"),
    ] {
        let weak = grid.value_at(x, y, iz).unwrap();
        assert!(
            (weak - round_trip_refl(15.0)).abs() < 0.05,
            "{corner} should read the 15 dBZ background, read {weak}",
        );
    }

    // And z is up: the top row of the box is above the 4.47° beam at these
    // ranges, so nothing may be extrapolated into it.
    assert_eq!(
        grid.index_at(east, north, shape.nz - 1),
        Some(NO_DATA_INDEX),
    );
}

/// Cell centres at the half-step, proved by a field that **varies along
/// range**: a builder sampling the cell's edge reads a different dBZ, and
/// a builder sampling a constant field could not tell.
#[test]
fn cell_centres_sit_at_the_half_step() {
    // dBZ that names the ground range it was read at.
    let scan = scan_of(&|_, slant| Some(20.0 + beam::ground_range_km(slant, f64::from(LOW_DEG))));
    let shape = VoxelShape {
        nx: 2,
        ny: 1,
        nz: 3,
    };
    // At 20 km ground range the two rungs bracket 0.209 … 1.587 km over
    // the antenna, so rows at 0.7 / 1.1 / 1.5 km MSL — 0.33 / 0.73 /
    // 1.13 km over it — all sit inside.
    let req = VoxelRequest {
        half_width_km: 40.0,
        base_km_msl: 0.5,
        top_km_msl: 1.7,
        ..request(shape)
    };
    let grid = build_voxels(&scan, &req, SITE.0, SITE.1).unwrap();

    // Two columns over an 80 km span: centres at −20 and +20 km east, both
    // on the y = 0 line. Ground range 20 km, not 0 and not 40.
    assert_eq!(
        grid.cell_centre_km(0, 0, 0).map(|c| (c.0, c.1)),
        Some((-20.0, 0.0)),
    );
    assert_eq!(
        grid.cell_centre_km(1, 0, 0).map(|c| (c.0, c.1)),
        Some((20.0, 0.0)),
    );

    for ix in 0..2 {
        for iz in 0..shape.nz {
            let read = grid.value_at(ix, 0, iz).unwrap();
            assert!(
                (read - round_trip_refl(40.0)).abs() < 0.3,
                "column {ix} row {iz} sits at 20 km ground range, so the \
                     field reads 40 dBZ; got {read}. An edge-sampled column \
                     would read 20 or 60.",
            );
        }
    }
}

/// The vertical axis is MSL and the site's own elevation is subtracted
/// exactly once.
///
/// KTLX stands at 1213 ft — 0.3697 km — which is 7 rows of this grid. A
/// builder that skipped the subtraction, or applied it with the wrong
/// sign, moves the lowest row with data by 7 or 14 rows.
#[test]
fn the_height_axis_is_msl_above_the_sites_own_elevation() {
    let scan = scan_of(&|_, _| Some(35.0));
    let nz = 240;
    let (base_km_msl, top_km_msl) = (0.0, 12.0);
    let dz = (top_km_msl - base_km_msl) / nz as f64;
    let shape = VoxelShape { nx: 2, ny: 1, nz };
    let req = VoxelRequest {
        half_width_km: 40.0,
        base_km_msl,
        top_km_msl,
        ..request(shape)
    };
    let grid = build_voxels(&scan, &req, SITE.0, SITE.1).unwrap();

    let lowest_beam_km = beam::height_at_ground_km(20.0, f64::from(LOW_DEG));
    let site_km_msl = SITE_ELEV_FT * 0.0003048;
    assert!(
        site_km_msl / dz > 5.0,
        "precondition: the site's elevation must be several rows deep or \
             this test cannot see the subtraction ({site_km_msl} km over a \
             {dz} km row)",
    );

    let first_with_data = (0..nz)
        .find(|&iz| grid.index_at(0, 0, iz) != Some(NO_DATA_INDEX))
        .expect("the column crosses the beam somewhere");
    let got_msl = base_km_msl + (first_with_data as f64 + 0.5) * dz;
    let want_msl = lowest_beam_km + site_km_msl;
    assert!(
        (got_msl - want_msl).abs() <= dz,
        "lowest row with data is at {got_msl} km MSL; the 0.53° beam is \
             {lowest_beam_km} km over a {site_km_msl} km site, so it should be \
             {want_msl}. Dropping the site elevation would put it at \
             {lowest_beam_km}.",
    );
}

/// The box may be centred away from the radar, and the ranges it reports
/// stay measured **from the site** — which is what lets a renderer place
/// the box knowing only `site`.
#[test]
fn the_centre_may_sit_away_from_the_site() {
    let scan = scan_of(&|_, _| Some(30.0));
    // ~50 km due east of KTLX.
    let east_lon = SITE.1 + 50.0 / (111.320 * SITE.0.to_radians().cos());
    let req = VoxelRequest {
        centre: (SITE.0, east_lon),
        half_width_km: 20.0,
        ..request(ODD)
    };
    let grid = build_voxels(&scan, &req, SITE.0, SITE.1).unwrap();
    assert!(
        (grid.x_range_km().0 - 30.0).abs() < 0.5 && (grid.x_range_km().1 - 70.0).abs() < 0.5,
        "a box 50 km east with a 20 km half-width spans 30..70 km east of \
             the site; got {:?}",
        grid.x_range_km(),
    );
    assert!(
        (grid.y_range_km().0 + 20.0).abs() < 0.5 && (grid.y_range_km().1 - 20.0).abs() < 0.5,
        "and stays on the site's own latitude; got {:?}",
        grid.y_range_km(),
    );
    assert_eq!(grid.site(), SITE);

    // A box centred on the site itself lands exactly on zero, with no
    // rounding drift out of the polar round trip.
    let centred = build_voxels(&scan, &request(ODD), SITE.0, SITE.1).unwrap();
    assert_eq!(centred.x_range_km(), (-60.0, 60.0));
    assert_eq!(centred.y_range_km(), (-60.0, 60.0));
}

/// Every number a renderer builds its model matrix from, asserted
/// together.
///
/// The six range bounds plus the site are the whole contract of the
/// output: a renderer reading them is not allowed to look anything else
/// up, so an accessor quietly returning the wrong pair would put the
/// volume somewhere else on screen with nothing else disagreeing.
/// Mutation testing found `z_range_km_msl` and the height half of
/// `cell_centre_km` unasserted for exactly that reason — the horizontal
/// axes were covered and the vertical one was not.
#[test]
fn the_output_carries_everything_a_model_matrix_needs() {
    let scan = scan_of(&|_, _| Some(35.0));
    let req = VoxelRequest {
        half_width_km: 37.5,
        base_km_msl: 0.75,
        top_km_msl: 15.25,
        ..request(ODD)
    };
    let grid = build_voxels(&scan, &req, SITE.0, SITE.1).unwrap();
    assert_eq!(grid.shape(), ODD);
    assert_eq!(grid.x_range_km(), (-37.5, 37.5));
    assert_eq!(grid.y_range_km(), (-37.5, 37.5));
    assert_eq!(grid.z_range_km_msl(), (0.75, 15.25));
    assert_eq!(grid.site(), SITE);
    assert_eq!(grid.product(), RadarProduct::Reflectivity);
    assert_eq!(
        grid.value_range(),
        (-32.5, 95.0),
        "255 data levels of 0.5 dBZ from −32.0, with index 0 half a step \
             under the bottom of them",
    );

    // Cell centres on all three axes at once, at the half-step, at both
    // ends — a fencepost error moves the corner cells and leaves the
    // middle alone.
    let (dx, dy, dz) = (75.0 / 11.0, 75.0 / 13.0, 14.5 / 7.0);
    let close = |got: Option<(f64, f64, f64)>, want: (f64, f64, f64)| {
        let g = got.expect("inside the grid");
        assert!(
            (g.0 - want.0).abs() < 1e-9
                && (g.1 - want.1).abs() < 1e-9
                && (g.2 - want.2).abs() < 1e-9,
            "cell centre {g:?} should be {want:?}",
        );
    };
    close(
        grid.cell_centre_km(0, 0, 0),
        (-37.5 + dx / 2.0, -37.5 + dy / 2.0, 0.75 + dz / 2.0),
    );
    close(
        grid.cell_centre_km(10, 12, 6),
        (37.5 - dx / 2.0, 37.5 - dy / 2.0, 15.25 - dz / 2.0),
    );
    // And each axis's bound independently, so a guard covering two of the
    // three survives neither.
    assert_eq!(grid.cell_centre_km(11, 0, 0), None);
    assert_eq!(grid.cell_centre_km(0, 13, 0), None);
    assert_eq!(grid.cell_centre_km(0, 0, 7), None);
    assert_eq!(grid.index_at(11, 0, 0), None);
    assert_eq!(grid.value_at(0, 0, 7), None);
}

/// The ladder the grid was resampled from travels with it, because the
/// sampler does not cross the worker boundary and the grid does.
#[test]
fn the_grid_reports_the_ladder_it_was_built_from() {
    let scan = scan_of(&|_, _| Some(35.0));
    let grid = build_voxels(&scan, &request(ODD), SITE.0, SITE.1).unwrap();
    assert_eq!(grid.tilt_count(), 2);
    assert!(
        (grid.widest_tilt_gap_deg() - (f64::from(HIGH_DEG) - f64::from(LOW_DEG))).abs() < 1e-6,
        "0.53° and 4.47° are 3.94° apart; reported {}",
        grid.widest_tilt_gap_deg(),
    );
    // Which is a wide enough gap to be worth warning about: at 60 km a
    // 3.94° step is over 4 km of unmeasured height.
    assert!(grid.widest_tilt_gap_deg() > 3.0);

    // And it is the sampler's own answer, not a recount.
    let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();
    assert_eq!(grid.tilt_count(), sampler.tilt_count());
    assert_eq!(grid.widest_tilt_gap_deg(), sampler.widest_tilt_gap_deg());
}

/// A one-rung ladder is the degenerate case, and it fabricates **nothing**.
///
/// A single beam has no vertical extent to interpolate over, so
/// `Column::at_height_km` answers only at exactly that beam's height —
/// which no cell centre lands on — and the grid comes back empty. That is
/// the right answer and the opposite of the plan's risk 2: the danger with
/// a short ladder is a smooth layer that is not there, and one rung cannot
/// draw one. The grid still builds, and says why through
/// [`VoxelGrid::tilt_count`].
#[test]
fn a_single_tilt_volume_fills_nothing_rather_than_smearing_one_beam() {
    let scan = Scan::new(
        vcp(&[0.5]),
        vec![refl_sweep(
            1,
            LOW_DEG,
            &wrapped_azimuths(720, 293.5),
            LOW_GATES,
            &|_, _| Some(50.0),
        )],
    );
    let grid = build_voxels(&scan, &request(ODD), SITE.0, SITE.1).unwrap();
    assert_eq!(grid.tilt_count(), 1);
    assert_eq!(grid.widest_tilt_gap_deg(), 0.0);
    assert!(
        grid.indices().iter().all(|&i| i == NO_DATA_INDEX),
        "one rung has no vertical extent, so nothing may be filled in",
    );
    // The same volume with a second cut *does* fill, so the emptiness
    // above is the ladder's doing and not a broken fixture.
    let two = scan_of(&|_, _| Some(50.0));
    let filled = build_voxels(&two, &request(ODD), SITE.0, SITE.1).unwrap();
    assert!(filled.indices().iter().any(|&i| i != NO_DATA_INDEX));
}

/// **Vertical detail belongs to the tilt ladder, not to `nz`.**
///
/// WP-B measured this on a cross-section; a voxel grid inherits it exactly,
/// because both go through `Column::at_height_km`, whose `blend` returns
/// the **nearest** rung the moment its bracket partner has no value. So a
/// measured layer on one rung is painted out to the half-weight midpoint on
/// each side, and a layer that falls between rungs is painted nowhere.
///
/// Both are pinned here in the grid's own units, on a 200-row box whose
/// rows are 60 m apart — a resolution 58× finer than the band the ladder
/// actually resolves, which is the point.
#[test]
fn a_layer_is_quantised_to_the_ladder_rather_than_to_nz() {
    let nz = 200;
    let (base_km_msl, top_km_msl) = (0.0, 12.0);
    let dz = (top_km_msl - base_km_msl) / nz as f64;
    let shape = VoxelShape { nx: 2, ny: 1, nz };
    // Half-width 200 km with two columns puts their centres at ±100 km
    // east on the y = 0 line — WP-B's own range.
    let req = VoxelRequest {
        half_width_km: 200.0,
        base_km_msl,
        top_km_msl,
        ..request(shape)
    };
    let site_km_msl = SITE_ELEV_FT * 0.0003048;
    let beam = |deg: f64| beam::height_at_ground_km(100.0, deg);
    let (low, middle, high) = (beam(0.53), beam(2.47), beam(4.51));

    // ── a layer measured on exactly one rung ──
    let grid = build_voxels(&one_rung_carries_data(Some(1)), &req, SITE.0, SITE.1).unwrap();
    assert_eq!(grid.tilt_count(), 3, "all three rungs must survive");
    let rows: Vec<usize> = (0..nz)
        .filter(|&iz| grid.index_at(1, 0, iz) != Some(NO_DATA_INDEX))
        .collect();
    assert!(!rows.is_empty(), "the middle rung's layer must paint");
    let height_of = |iz: usize| base_km_msl + (iz as f64 + 0.5) * dz - site_km_msl;
    let (first, last) = (height_of(rows[0]), height_of(rows[rows.len() - 1]));
    assert_eq!(
        rows.len(),
        rows[rows.len() - 1] - rows[0] + 1,
        "and it must paint one contiguous band, not a striped one",
    );

    let lower_mid = (low + middle) / 2.0;
    let upper_mid = (middle + high) / 2.0;
    assert!(
        (first - lower_mid).abs() <= dz,
        "the band's floor is the half-weight midpoint to the rung below \
             ({lower_mid} km), not the beam itself ({middle} km); got {first}",
    );
    assert!(
        (last - upper_mid).abs() <= dz,
        "and its ceiling is the midpoint to the rung above ({upper_mid} \
             km); got {last}",
    );

    // The fabricated thickness, as a number. One tilt, 3.48 km of band.
    assert!(
        ((last - first) - 3.48).abs() < 0.1,
        "one rung paints a {} km band at 100 km on this ladder",
        last - first,
    );
    assert!(
        (last - first) / dz > 50.0,
        "which is {}x the row height, so no amount of nz recovers the \
             layer's true thickness",
        (last - first) / dz,
    );

    // ── a layer that no rung looked at ──
    let missed = build_voxels(&one_rung_carries_data(None), &req, SITE.0, SITE.1).unwrap();
    assert_eq!(missed.tilt_count(), 3, "the ladder is the same one");
    assert!(
        missed.indices().iter().all(|&i| i == NO_DATA_INDEX),
        "a layer between tilts is measured by nothing and painted nowhere, \
             however fine the grid",
    );
}

// ── The builder adds no geometry of its own ─────────────────────────────

/// Every cell is the sampler's own answer at that cell's coordinates.
///
/// The guard against this module quietly growing a second copy of the beam
/// geometry: the coordinates below are written out longhand rather than
/// through `axis_centre`, so the two spellings have to agree.
#[test]
fn every_cell_is_the_samplers_own_answer() {
    let scan = scan_of(&|az, slant| (az < 200.0).then_some(10.0 + (slant % 37.0) + az / 12.0));
    let shape = VoxelShape {
        nx: 9,
        ny: 8,
        nz: 6,
    };
    let req = VoxelRequest {
        half_width_km: 55.0,
        base_km_msl: 0.5,
        top_km_msl: 9.5,
        ..request(shape)
    };
    let grid = build_voxels(&scan, &req, SITE.0, SITE.1).unwrap();
    let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();
    let site_km_msl = SITE_ELEV_FT * 0.0003048;

    let mut with_data = 0usize;
    for iz in 0..shape.nz {
        let z_msl = 0.5 + (iz as f64 + 0.5) * (9.5 - 0.5) / shape.nz as f64;
        for iy in 0..shape.ny {
            let y = -55.0 + (iy as f64 + 0.5) * 110.0 / shape.ny as f64;
            for ix in 0..shape.nx {
                let x = -55.0 + (ix as f64 + 0.5) * 110.0 / shape.nx as f64;
                let want = sampler.sample(
                    x.atan2(y).to_degrees().rem_euclid(360.0),
                    x.hypot(y),
                    z_msl - site_km_msl,
                );
                let got_index = grid.index_at(ix, iy, iz).unwrap();
                let got_value = grid.value_at(ix, iy, iz).unwrap();
                match want.value().filter(|v| v.is_finite()) {
                    Some(v) => {
                        with_data += 1;
                        assert_eq!(got_value, v, "value at {ix},{iy},{iz}");
                        assert_eq!(got_index, grid.value_to_index(v), "index at {ix},{iy},{iz}",);
                    }
                    None => {
                        assert_eq!(got_index, NO_DATA_INDEX, "index at {ix},{iy},{iz}");
                        assert!(got_value.is_nan(), "value at {ix},{iy},{iz}");
                    }
                }
            }
        }
    }
    // Both halves of the comparison have to be exercised, or the loop
    // above proves only that empty grids match empty grids.
    assert!(
        with_data > 0 && with_data < shape.cells(),
        "precondition: the fixture must produce both data and no-data \
             cells; got {with_data} of {}",
        shape.cells(),
    );
}

/// Nothing is filled in above the highest tilt, below the lowest, or past
/// the last gate — the volume's shell is no-data, not extrapolated.
#[test]
fn nothing_is_extrapolated_outside_the_ladder() {
    let scan = scan_of(&|_, _| Some(45.0));
    let shape = VoxelShape {
        nx: 3,
        ny: 3,
        nz: 40,
    };
    // A box reaching well past the low tilt's last gate (151.9 km slant)
    // and well above the high tilt's beam.
    let req = VoxelRequest {
        half_width_km: 220.0,
        base_km_msl: 0.0,
        top_km_msl: 25.0,
        ..request(shape)
    };
    let grid = build_voxels(&scan, &req, SITE.0, SITE.1).unwrap();

    // Over the site every beam centre is at zero height, so the whole
    // column above the ground is above the volume — the cone of silence,
    // reported rather than invented.
    let centre = (shape.nx / 2, shape.ny / 2);
    assert!(
        (0..shape.nz).all(|iz| grid.index_at(centre.0, centre.1, iz) == Some(NO_DATA_INDEX)),
        "the cone of silence must stay empty",
    );

    // The corner column sits at 220·√2 = 311 km, past every gate.
    assert!(
        (0..shape.nz).all(|iz| grid.index_at(0, 0, iz) == Some(NO_DATA_INDEX)),
        "311 km is past the last gate of both tilts",
    );

    // The top of the box is over the 4.47° beam everywhere in it.
    let top = shape.nz - 1;
    assert!(
        (0..shape.nx)
            .all(|ix| (0..shape.ny).all(|iy| grid.index_at(ix, iy, top) == Some(NO_DATA_INDEX))),
        "25 km MSL is above the highest tilt at every range in this box",
    );

    // And the fixture is not simply empty.
    assert!(
        grid.indices().iter().any(|&i| i != NO_DATA_INDEX),
        "precondition: something in this grid must have data, or the \
             assertions above are vacuous",
    );
}

// ── The two planes ──────────────────────────────────────────────────────

#[test]
fn the_value_plane_is_absent_unless_asked_for() {
    let scan = scan_of(&|_, _| Some(40.0));
    let req = VoxelRequest {
        values_wanted: false,
        ..request(ODD)
    };
    let lean = build_voxels(&scan, &req, SITE.0, SITE.1).unwrap();
    assert_eq!(lean.values(), None);
    assert_eq!(lean.value_at(0, 0, 0), None);
    assert_eq!(lean.memory_bytes(), ODD.cells() + LUT_LEN);

    let full = build_voxels(&scan, &request(ODD), SITE.0, SITE.1).unwrap();
    assert_eq!(full.values().map(<[f32]>::len), Some(ODD.cells()));
    assert_eq!(full.memory_bytes(), ODD.cells() * 5 + LUT_LEN);
    // Same indices either way: the value plane is a copy, not a different
    // resample.
    assert_eq!(lean.indices(), full.indices());
}

/// The two planes say the same thing about every cell: `NaN` exactly where
/// the index is [`NO_DATA_INDEX`], and never one without the other.
#[test]
fn the_two_planes_agree_cell_for_cell() {
    let scan = scan_of(&|az, slant| (az < 140.0 && slant < 80.0).then_some(52.0));
    let grid = build_voxels(&scan, &request(ODD), SITE.0, SITE.1).unwrap();
    let values = grid.values().unwrap();
    let (mut empty, mut filled) = (0, 0);
    for (index, value) in grid.indices().iter().zip(values) {
        if *index == NO_DATA_INDEX {
            empty += 1;
            assert!(value.is_nan(), "no-data cell carries {value}");
        } else {
            filled += 1;
            assert!(value.is_finite(), "data cell carries {value}");
            assert_eq!(*index, grid.value_to_index(*value));
        }
    }
    assert!(
        empty > 0 && filled > 0,
        "precondition: this fixture must produce both, or the loop proves \
             nothing ({empty} empty, {filled} filled)",
    );
}

// ── Equality and Debug ──────────────────────────────────────────────────

/// The reason `PartialEq` is hand-written: the value plane is mostly
/// `NaN`, and a derived one would make a grid unequal to a byte-identical
/// copy of itself.
#[test]
fn two_identical_grids_compare_equal_through_the_nan_value_plane() {
    let scan = scan_of(&|az, _| (az < 90.0).then_some(48.0));
    let a = build_voxels(&scan, &request(ODD), SITE.0, SITE.1).unwrap();
    let b = build_voxels(&scan, &request(ODD), SITE.0, SITE.1).unwrap();

    assert!(
        a.values().unwrap().iter().any(|v| v.is_nan()),
        "precondition: without a NaN in the value plane this test would \
             pass under a derived PartialEq too, and prove nothing",
    );
    assert_eq!(a, b);
    assert_eq!(a, a.clone());
    // The derive's behaviour, shown rather than described.
    assert!(
        !a.values()
            .unwrap()
            .iter()
            .zip(b.values().unwrap())
            .all(|(x, y)| x == y),
        "an element-wise `==` over the value planes disagrees, which is \
             exactly what `#[derive(PartialEq)]` would have used",
    );

    // And it still discriminates: a different box is a different grid.
    let moved = VoxelRequest {
        half_width_km: 61.0,
        ..request(ODD)
    };
    assert_ne!(a, build_voxels(&scan, &moved, SITE.0, SITE.1).unwrap());
    let lean = VoxelRequest {
        values_wanted: false,
        ..request(ODD)
    };
    assert_ne!(a, build_voxels(&scan, &lean, SITE.0, SITE.1).unwrap());
}

/// A grid built by hand, so the equality tests can vary **one** field.
///
/// Every field before the value plane in `eq`'s `&&` chain short-circuits,
/// and the index plane is a quantisation of the value plane — so a pair
/// built from two different scans differs on `indices` and never reaches
/// the value comparison at all. Mutation testing found all three of
/// `same_values`' arms unreachable from `build_voxels` alone for exactly
/// that reason.
fn hand_built(values: Option<Vec<f32>>) -> VoxelGrid {
    let value_range = value_range_for(MomentSlot::Reflectivity);
    VoxelGrid {
        indices: vec![0, 7, 200, 255],
        values,
        lut: colormap_lut(RadarProduct::Reflectivity, value_range),
        shape: VoxelShape {
            nx: 2,
            ny: 2,
            nz: 1,
        },
        x_range_km: (-10.0, 10.0),
        y_range_km: (-10.0, 10.0),
        z_range_km_msl: (0.0, 5.0),
        site: SITE,
        value_range,
        product: RadarProduct::Reflectivity,
        tilt_count: 2,
        widest_tilt_gap_deg: 3.94,
    }
}

/// The value plane is compared **bitwise**, its length counts, and having
/// no plane at all is a state of its own.
#[test]
fn the_value_plane_is_compared_bit_for_bit_and_its_absence_is_a_state() {
    let nan = f32::NAN;
    let a = hand_built(Some(vec![nan, -20.0, 45.0, 62.5]));
    assert_eq!(a, hand_built(Some(vec![nan, -20.0, 45.0, 62.5])));

    // Same index plane, different values — the pair `build_voxels` cannot
    // produce.
    let different = hand_built(Some(vec![nan, -20.0, 45.25, 62.5]));
    assert_eq!(
        a.indices(),
        different.indices(),
        "precondition: only the value plane may differ, or this proves \
             nothing about `same_values`",
    );
    assert_ne!(a, different, "a different value plane is a different grid");

    // A shorter plane is a different payload, not a prefix match.
    assert_ne!(a, hand_built(Some(vec![nan, -20.0, 45.0])));

    // Bitwise: two NaNs with different payloads are two different
    // payloads, even though neither equals itself as a float.
    let other_nan = hand_built(Some(vec![
        f32::from_bits(nan.to_bits() ^ 1),
        -20.0,
        45.0,
        62.5,
    ]));
    assert!(other_nan.values().unwrap()[0].is_nan());
    assert_ne!(a, other_nan);

    // No plane at all: equal to another grid with none, unequal to one
    // with a plane, in both directions.
    assert_eq!(hand_built(None), hand_built(None));
    assert_ne!(a, hand_built(None));
    assert_ne!(hand_built(None), a);
}

/// `Debug` is a summary, for the reason the sampler's is: `assert_eq!`
/// reaches for it on failure, and the derive would print 8 MiB.
#[test]
fn debug_is_a_summary_rather_than_the_grid() {
    let scan = scan_of(&|az, _| (az < 90.0).then_some(48.0));
    let grid = build_voxels(&scan, &request(ODD), SITE.0, SITE.1).unwrap();
    let text = format!("{grid:?}");
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(text.len() < 400, "{} chars: {text}", text.len());
    assert!(text.contains("ref"), "{text}");
    assert!(text.contains("11x13x7"), "{text}");

    // The fill count is what two grids most often differ by, so it is the
    // one number in the summary worth checking rather than merely
    // formatting. Counted here rather than trusted.
    let filled = grid
        .indices()
        .iter()
        .filter(|&&i| i != NO_DATA_INDEX)
        .count();
    assert!(
        filled > 0 && filled < ODD.cells(),
        "precondition: a partly filled grid, or the count below cannot \
             discriminate ({filled} of {})",
        ODD.cells(),
    );
    assert_ne!(
        filled,
        ODD.cells() - filled,
        "precondition: filled and empty must differ, or reporting the \
             wrong one of the two would read the same",
    );
    assert!(
        text.contains(&format!("{filled}/{}", ODD.cells())),
        "the summary must report {filled} of {} cells with data: {text}",
        ODD.cells(),
    );
}

// ── The ramp ────────────────────────────────────────────────────────────

/// Affine, and exact both ways for all 255 data indices of all six
/// moments.
#[test]
fn the_ramp_is_affine_and_round_trips_every_data_index() {
    for slot in SLOTS {
        let range = value_range_for(slot);
        let step = f64::from(range.1 - range.0) / 255.0;
        for index in 1..=255u8 {
            let value = ramp_value(range, index);
            assert_eq!(
                ramp_index(range, value),
                index,
                "{slot:?} index {index} -> {value} -> {}",
                ramp_index(range, value),
            );
            // Affine: the gap to the entry below is one step everywhere,
            // including across index 1 — which is what makes filtering
            // within data exactly linear interpolation of the value.
            let below = ramp_value(range, index - 1);
            assert!(
                (f64::from(value - below) - step).abs() < step * 1e-4,
                "{slot:?} step {}->{index} is {} not {step}",
                index - 1,
                value - below,
            );
        }
    }
}

/// Index 0 is **below** the moment's lowest data level, not on it — so a
/// real measurement can never read as an absence.
///
/// Index 1 is compared to within a ten-thousandth of a step rather than
/// exactly, because `value_range` is `f32` and reconstructing
/// `(lo − step) + step` cancels: ΦDP's bottom level comes back as 1.2e−7
/// instead of 0. That is four decimal orders below the 1.4° step and eight
/// below the span, and the round trip through
/// [`ramp_index`] is still exact — which
/// `the_ramp_is_affine_and_round_trips_every_data_index` pins separately,
/// so nothing here is being waved through.
#[test]
fn index_zero_is_one_step_below_the_bottom_data_level() {
    for slot in SLOTS {
        let (lo, hi) = data_levels(slot);
        let range = value_range_for(slot);
        let step = (f64::from(hi) - f64::from(lo)) / 254.0;
        assert!(
            (f64::from(ramp_value(range, 1)) - f64::from(lo)).abs() < step * 1e-4,
            "{slot:?}: index 1 must be the bottom data level {lo}, is {}",
            ramp_value(range, 1),
        );
        assert_eq!(
            ramp_value(range, 255),
            hi,
            "{slot:?}: index 255 must be the top data level exactly",
        );
        assert!(
            range.0 < lo,
            "{slot:?}: index 0 ({}) must sit under the bottom data level \
                 ({lo})",
            range.0,
        );
        // And by a whole step, not by a rounding crumb — that is what
        // keeps a real measurement off the no-data index.
        assert!(
            (f64::from(lo) - f64::from(range.0) - step).abs() < step * 1e-4,
            "{slot:?}: index 0 must sit one full step ({step}) below {lo}, \
                 sits {} below",
            f64::from(lo) - f64::from(range.0),
        );
    }
}

/// Every raw code of every moment's Level II encoding lands on a data
/// index, inside the declared span.
///
/// The encodings are written out here rather than read from a fixture:
/// they are the ICD's, they are what `data_levels` was derived from, and
/// restating them is the only way this test can disagree with the table.
#[test]
fn no_measurement_encodes_as_the_no_data_index() {
    // (slot, scale, offset) for the 8-bit moments; ΦDP is 16-bit and is
    // walked over its own turn instead.
    let encodings = [
        (MomentSlot::Reflectivity, 2.0, 66.0),
        (MomentSlot::Velocity, 2.0, 129.0),
        (MomentSlot::SpectrumWidth, 2.0, 129.0),
        (MomentSlot::DifferentialReflectivity, 16.0, 128.0),
        (MomentSlot::CorrelationCoefficient, 300.0, -60.5),
    ];
    for (slot, scale, offset) in encodings {
        let range = value_range_for(slot);
        let (lo, hi) = data_levels(slot);
        for code in 2..=255u32 {
            let value = ((code as f32) - offset) / scale;
            // Spectrum width shares velocity's encoding but is
            // non-negative; its negative half is not a measurement.
            if slot == MomentSlot::SpectrumWidth && value < 0.0 {
                continue;
            }
            assert!(
                value >= lo && value <= hi,
                "{slot:?} code {code} decodes to {value}, outside the \
                     declared span {lo}..={hi}",
            );
            assert_ne!(
                ramp_index(range, value),
                NO_DATA_INDEX,
                "{slot:?} code {code} ({value}) encodes as no-data",
            );
        }
    }
    // ΦDP over its whole turn, at a resolution finer than its 16-bit
    // encoding's 1/2.8361 of a degree.
    let range = value_range_for(MomentSlot::DifferentialPhase);
    for step in 0..=3600 {
        let value = step as f32 / 10.0;
        assert_ne!(ramp_index(range, value), NO_DATA_INDEX, "PhiDP {value}");
    }
    // And the clamp has teeth: something off either end still lands on a
    // data index rather than being silently reclassified as absent.
    let refl = value_range_for(MomentSlot::Reflectivity);
    assert_eq!(ramp_index(refl, -1000.0), 1);
    assert_eq!(ramp_index(refl, 1000.0), 255);
    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert_eq!(ramp_index(refl, bad), NO_DATA_INDEX, "{bad}");
    }
}

/// A value outside the declared span clamps to the nearest **data** level
/// — never to the no-data index — and the value plane keeps the number the
/// radar actually reported.
///
/// Found by a fixture that wrote spectrum-width raw codes under 129. The
/// RDA does not emit those: spectrum width shares velocity's `(2, 129)`
/// codec, so codes 2…128 decode to a *negative* width, which is not a
/// measurement, and the ICD's defined range for the moment is 0…63 m/s.
/// So the case is unreachable from valid Level II — but it is reachable
/// from a malformed file, and the two planes then say different things
/// about the same cell **on purpose**: the index plane must land on the
/// ramp, and the value plane must not launder a bad number into a
/// plausible one. The one visible consequence is that such a cell paints
/// the palette's 0 m/s grey where the plan view paints it transparent,
/// which is one index out of 256 on data that should not exist.
#[test]
fn a_value_outside_the_declared_span_clamps_to_the_nearest_data_level() {
    let range = value_range_for(MomentSlot::SpectrumWidth);
    let (lo, hi) = data_levels(MomentSlot::SpectrumWidth);
    // A raw code of 5 through spectrum width's codec.
    let impossible = (5.0 - 129.0) / 2.0;
    assert!(impossible < lo, "precondition: {impossible} is under {lo}");
    assert_eq!(
        ramp_index(range, impossible),
        1,
        "an under-range value takes the bottom data level, not no-data",
    );
    assert_eq!(ramp_index(range, hi + 100.0), 255);
    // And the same on the other five moments, so the clamp is not a
    // spectrum-width special case.
    for slot in SLOTS {
        let range = value_range_for(slot);
        let (lo, hi) = data_levels(slot);
        assert_eq!(ramp_index(range, lo - 1e6), 1, "{slot:?}");
        assert_eq!(ramp_index(range, hi + 1e6), 255, "{slot:?}");
    }
}

/// Four of the six steps land exactly on the moment's own quantum, and the
/// two that do not are recorded rather than rounded away.
#[test]
fn the_declared_steps_are_measured() {
    let step = |slot| {
        let (lo, hi) = data_levels(slot);
        (f64::from(hi) - f64::from(lo)) / 254.0
    };
    assert_eq!(step(MomentSlot::Reflectivity), 0.5, "Level II's own 0.5 dB");
    assert_eq!(step(MomentSlot::Velocity), 0.5, "the 0.5 m/s encoding");
    assert_eq!(step(MomentSlot::SpectrumWidth), 0.25);
    assert_eq!(
        step(MomentSlot::DifferentialReflectivity),
        0.0625,
        "1/16 dB"
    );
    // Marginally coarser than their encodings, and far finer than a viewer
    // can distinguish. Pinned so a change to either span is noticed.
    assert!((step(MomentSlot::DifferentialPhase) - 1.417_32).abs() < 1e-5);
    assert!((step(MomentSlot::CorrelationCoefficient) - 0.003_385_8).abs() < 1e-7);
}

// ── The colour table ────────────────────────────────────────────────────

/// All nine products build, all nine carry a full table, and all nine
/// come back with data in them — the end-to-end check the single-moment
/// fixtures cannot make.
///
/// Nine, not six: the three derivations run through `derive::prepare`
/// inside `build_voxels`, so a derivation that stopped producing a field
/// would surface here as an empty grid rather than in the app.
#[test]
fn every_volume_product_builds_a_populated_grid_and_a_full_table() {
    assert_eq!(LUT_LEN, 1024);
    let scan = six_moment_scan();
    for product in VOLUME_PRODUCTS {
        let req = VoxelRequest {
            product,
            half_width_km: 40.0,
            base_km_msl: 0.5,
            top_km_msl: 4.0,
            ..request(ODD)
        };
        let grid = build_voxels(&scan, &req, SITE.0, SITE.1).unwrap();
        assert_eq!(grid.lut().len(), LUT_LEN, "{}", product.name());
        assert_eq!(grid.product(), product);
        let filled = grid
            .indices()
            .iter()
            .filter(|&&i| i != NO_DATA_INDEX)
            .count();
        assert!(
            filled > 0,
            "{} came back empty, so every per-product assertion below it \
                 would be vacuous",
            product.name(),
        );
        // Every value sits inside the declared span, which is what makes
        // the quantisation declared rather than hoped for.
        let (lo, hi) = grid.value_range();
        for value in grid.values().unwrap().iter().filter(|v| v.is_finite()) {
            assert!(
                *value >= lo && *value <= hi,
                "{} read {value} outside {lo}..={hi}",
                product.name(),
            );
        }
    }
}

/// The no-data entry is fully transparent for **every** product — forced,
/// not inherited, because four of the six palettes hand back an opaque
/// colour at the bottom of their ramp and an opaque no-data index paints
/// the entire outside of the volume.
#[test]
fn the_no_data_entry_is_transparent_for_every_product() {
    for product in VOLUME_PRODUCTS {
        let range = ramp_of(product);
        let lut = colormap_lut(product, range);
        assert_eq!(
            &lut[0..4],
            &[0, 0, 0, 0],
            "{} entry 0 must be transparent",
            product.name(),
        );
    }
    // The precondition that makes the forcing necessary rather than
    // decorative: these four palettes are opaque at the ramp's bottom.
    for product in [
        RadarProduct::Velocity,
        RadarProduct::DifferentialReflectivity,
        RadarProduct::DifferentialPhase,
        RadarProduct::CorrelationCoefficient,
    ] {
        let range = value_range_for(samplable(product).unwrap());
        let (_, _, _, alpha) = get_color_for_value(product, ramp_value(range, 0));
        assert_ne!(
            alpha,
            0,
            "{} paints its ramp bottom opaque, which is why entry 0 is \
                 forced",
            product.name(),
        );
    }
}

/// The table comes from `get_color_for_value`, not from
/// `LegendScale::thresholds`. Four things that would break, each shown.
#[test]
fn the_table_is_the_palette_function_not_its_stops() {
    // 1. `extract_scale` filters non-finite stops, so ZDR's NEG_INFINITY
    //    floor — the stop colouring everything under −2 dB — is absent
    //    from `thresholds` entirely.
    let zdr = get_legend_scale(RadarProduct::DifferentialReflectivity);
    assert!(
        zdr.thresholds.iter().all(|(v, _)| *v >= -2.0),
        "precondition: the ZDR stops start at −2 dB, so a table built \
             from them has no colour under it",
    );
    let range = value_range_for(MomentSlot::DifferentialReflectivity);
    let lut = colormap_lut(RadarProduct::DifferentialReflectivity, range);
    // Index 1 is −7.875 dB, well under the lowest surviving stop.
    assert_eq!(&lut[4..8], &[66, 66, 66, 180], "ZDR's floor colour");

    // 2. The per-product transparency floor lives only in the function:
    //    reflectivity under 0 dBZ is transparent, and no stop says so.
    let refl_range = value_range_for(MomentSlot::Reflectivity);
    let refl = colormap_lut(RadarProduct::Reflectivity, refl_range);
    let below_zero = ramp_index(refl_range, -0.5);
    assert_eq!(refl[usize::from(below_zero) * 4 + 3], 0, "−0.5 dBZ");
    assert_ne!(refl[usize::from(ramp_index(refl_range, 0.5)) * 4 + 3], 0);
    assert!(
        get_legend_scale(RadarProduct::Reflectivity)
            .thresholds
            .iter()
            .any(|(v, _)| *v == 0.0),
        "precondition: the stops *do* carry 0 dBZ, with a colour — so a \
             table built from them would paint everything under it opaque",
    );

    // 3. Velocity's stops are in mph in two separate tables; the function
    //    is the only thing that knows the input is m/s.
    let vel_range = value_range_for(MomentSlot::Velocity);
    let vel = colormap_lut(RadarProduct::Velocity, vel_range);
    let inbound = usize::from(ramp_index(vel_range, -30.0)) * 4;
    let outbound = usize::from(ramp_index(vel_range, 30.0)) * 4;
    assert!(
        vel[inbound + 1] > vel[inbound] && vel[outbound] > vel[outbound + 1],
        "inbound must be green and outbound red; got {:?} and {:?}",
        &vel[inbound..inbound + 4],
        &vel[outbound..outbound + 4],
    );

    // 4. Every entry is exactly the function's answer: the colour
    //    verbatim, the alpha scaled by the product's own 3D transparency
    //    profile — and by nothing else, so the profile can only ever make
    //    an entry *more* transparent than its plan-view colour.
    for product in VOLUME_PRODUCTS {
        let range = ramp_of(product);
        let lut = colormap_lut(product, range);
        for index in 1..=255u8 {
            let value = ramp_value(range, index);
            let (r, g, b, a) = get_color_for_value(product, value);
            let scaled = (f32::from(a) * volume_alpha_scale(product, value)).round() as u8;
            let at = usize::from(index) * 4;
            assert_eq!(
                &lut[at..at + 4],
                &[r, g, b, scaled],
                "{} entry {index}",
                product.name(),
            );
            assert!(
                lut[at + 3] <= a,
                "{} entry {index}: the 3D profile must never exceed the \
                     palette's own alpha",
                product.name(),
            );
        }
    }
}

/// A non-gradient scale's table must be consumed `NEAREST`, or a blend
/// names a step the scale does not define.
#[test]
fn the_table_filter_is_nearest_only_for_a_non_gradient_scale() {
    let scan = six_moment_scan();
    for product in VOLUME_PRODUCTS {
        let req = VoxelRequest {
            product,
            ..request(ODD)
        };
        let grid = build_voxels(&scan, &req, SITE.0, SITE.1).unwrap();
        let want = if product == RadarProduct::SpectrumWidth {
            LutFilter::Nearest
        } else {
            LutFilter::Linear
        };
        assert_eq!(grid.lut_filter(), want, "{}", product.name());
        assert_eq!(
            grid.lut_filter() == LutFilter::Linear,
            get_legend_scale(product).is_gradient,
            "the filter is derived from the scale, never stored",
        );
    }
    // The rule exists for the categorical case, which the sampler refuses
    // — stated here so the reason survives if spectrum width's scale ever
    // becomes a gradient.
    assert_eq!(
        samplable(RadarProduct::HydrometeorClassification),
        None,
        "HHC is the scale where a blended step would be a wrong category, \
             and it is not a moment",
    );
    assert!(!get_legend_scale(RadarProduct::HydrometeorClassification).is_gradient);
}

// ── The boundary, which is the whole point of the encoding ──────────────

/// What a `Linear` fetch of an `R8Unorm` texture returns between two
/// texels: the hardware normalises each to [0, 1], interpolates, and hands
/// back a float, which the shader scales by 255 to index the table.
fn fetched_index(a: u8, b: u8, t: f64) -> f64 {
    f64::from(a) * (1.0 - t) + f64::from(b) * t
}

fn ramp_value_at(range: (f32, f32), index: f64) -> f64 {
    f64::from(range.0) + (f64::from(range.1) - f64::from(range.0)) * index / 255.0
}

fn alpha_at(lut: &[u8], index: f64) -> u8 {
    lut[(index.round() as usize).min(255) * 4 + 3]
}

/// **The test the encoding decision exists for.** A sharp echo edge, and
/// the filtered result across it — fading out rather than jumping to an
/// opaque middle.
///
/// The comparison is against the *rejected* encoding, computed here rather
/// than described, because "bottom-of-ramp is better" is only a claim
/// until both are evaluated over the same edge.
#[test]
fn an_echo_edge_fades_instead_of_fabricating_a_mid_value() {
    // A 65 dBZ core with a hard azimuthal and radial edge: outside it the
    // radar looked and saw nothing (raw code 0, below threshold), which is
    // no-data, not a low value.
    let scan = scan_of(&|az, slant| {
        ((40.0..80.0).contains(&az) && (20.0..50.0).contains(&slant)).then_some(65.0)
    });
    let shape = VoxelShape {
        nx: 64,
        ny: 64,
        nz: 24,
    };
    let req = VoxelRequest {
        half_width_km: 60.0,
        base_km_msl: 0.5,
        top_km_msl: 8.0,
        ..request(shape)
    };
    let grid = build_voxels(&scan, &req, SITE.0, SITE.1).unwrap();
    let range = grid.value_range();

    // Find a real edge in the built grid: two x-adjacent cells, one with
    // data and one without. A fixture where every voxel has data cannot
    // test this at all, which is why the field above has an edge in it.
    let mut edge = None;
    for iz in 0..shape.nz {
        for iy in 0..shape.ny {
            for ix in 0..shape.nx - 1 {
                let a = grid.index_at(ix, iy, iz).unwrap();
                let b = grid.index_at(ix + 1, iy, iz).unwrap();
                if a != NO_DATA_INDEX && b == NO_DATA_INDEX && a > 150 {
                    edge = Some((a, b));
                }
            }
        }
    }
    let (data, empty) = edge.expect("the fixture must contain a strong echo edge");
    assert_eq!(empty, NO_DATA_INDEX);
    // Measured, so the numbers below are numbers and not a description:
    // the 65 dBZ core resamples to index 195 exactly, which is
    // −32.5 + 195 × 0.5.
    assert_eq!((data, grid.index_to_value(data)), (195, 65.0));

    // ── ours: bottom of ramp ──
    let mut previous = f64::INFINITY;
    let mut first_transparent = None;
    let data_value = ramp_value_at(range, f64::from(data));
    for step in 0..=64 {
        let t = f64::from(step) / 64.0;
        let index = fetched_index(data, empty, t);
        let value = ramp_value_at(range, index);
        assert!(
            value <= previous,
            "the fetched value must fall monotonically toward the ramp \
                 bottom; at t={t} it rose to {value} from {previous}",
        );
        assert!(
            value <= data_value + 1e-9,
            "nothing on the boundary may be stronger than the echo it \
                 borders: {value} > {data_value} at t={t}",
        );
        previous = value;
        if first_transparent.is_none() && alpha_at(grid.lut(), index) == 0 {
            first_transparent = Some(t);
        }
    }
    let faded_at = first_transparent.expect("the boundary must reach transparency");
    assert!(
        faded_at < 1.0,
        "alpha must reach zero *before* the no-data neighbour, or the \
             fade is a single step at the very end; reached it at t={faded_at}",
    );
    assert!(
        faded_at < 0.75,
        "the fade should be a real fraction of the edge, not a rounding \
             artefact; reached transparency only at t={faded_at}",
    );
    // Measured: index 195 × (1 − t) drops to 64 — the top of the
    // transparent band, −0.5 dBZ — at t = 43/64, so the last third of the
    // way to the empty neighbour is already invisible.
    assert_eq!(faded_at, 43.0 / 64.0);

    // ── the rejected encoding: index 0 out of band ──
    //
    // Data indices 1..=255 span the palette's own 0..95 dBZ and 0 means
    // "no data", off the ramp. Same edge, same filter.
    let (oob_lo, oob_hi) = (0.0f64, 95.0f64);
    let oob_value = |index: f64| oob_lo + (index - 1.0) / 254.0 * (oob_hi - oob_lo);
    let oob_data = (1.0 + (data_value - oob_lo) / (oob_hi - oob_lo) * 254.0).round();
    let oob_half = fetched_index(oob_data as u8, 0, 0.5);
    let fabricated = oob_value(oob_half);
    assert!(
        fabricated > 25.0,
        "the rejected encoding is supposed to fabricate a mid-dBZ shell; \
             halfway across the edge it reads {fabricated} dBZ",
    );
    // Fully opaque, because that index is an ordinary data index.
    assert_ne!(
        get_color_for_value(RadarProduct::Reflectivity, fabricated as f32).3,
        0,
        "and the alpha floor cannot rescue it: the floor applies to the \
             fetched index, and {fabricated} dBZ is a perfectly ordinary echo",
    );

    // Ours, at the same place on the same edge.
    let ours_half = ramp_value_at(range, fetched_index(data, empty, 0.5));
    assert!(
        ours_half < fabricated - 10.0,
        "bottom-of-ramp must read materially weaker halfway across the \
             edge than the out-of-band encoding: {ours_half} dBZ against \
             {fabricated} dBZ",
    );

    // The whole comparison as three numbers, so a change to any of them
    // is a change to the decision rather than to a wording.
    assert_eq!(
        (
            (data_value * 100.0).round(),
            (ours_half * 100.0).round(),
            (fabricated * 100.0).round(),
        ),
        (6500.0, 1625.0, 3235.0),
        "65.00 dBZ core; halfway across its edge bottom-of-ramp reads \
             16.25 dBZ and fades out a third of the way further on, while the \
             rejected out-of-band encoding reads 32.35 dBZ at full opacity and \
             only vanishes on the empty voxel itself",
    );
}

/// **What the shipped encoding actually costs the five non-fading
/// moments, measured rather than argued.**
///
/// The module doc used to claim bottom-of-ramp was "strictly better" than
/// an out-of-band index for every moment, on the reasoning that an opaque
/// *end-of-ramp* colour beats an opaque *mid-ramp* one. **That reasoning
/// is wrong for a bidirectional or centred palette**, and this test is why
/// the claim was corrected: for velocity and ZDR the out-of-band ramp's
/// midpoint *is* the palette's neutral, while our ramp's bottom is its
/// saturated extreme — so the shipped encoding paints the **more**
/// alarming halo, not the less.
///
/// Each row is a half-edge fetch: a plausible echo value adjacent to
/// nothing, filtered at `t = 0.5`, decoded under both encodings.
///
/// The out-of-band ramp is data indices 1..=255 over the **palette's own**
/// range, with 0 reserved off it. Spanning the palette rather than the
/// moment's physical range is what an out-of-band design would actually
/// do, and the distinction is the whole comparison: widening below the
/// palette floor is *our* construction's requirement — index 0 has to be a
/// value — and an encoding that reserves 0 has no such need. Over the same
/// span the two are the identical mapping for every `i >= 1`, so comparing
/// them there would measure nothing.
///
/// Shipping bottom-of-ramp is still right — the out-of-band ramp cannot
/// represent the moment's floor at all, so it clamps real measurements
/// outside the palette range — but the honest summary is "no worse, and a
/// wash or slightly worse per moment", not "strictly better".
#[test]
fn the_half_edge_costs_of_both_encodings_are_measured_per_moment() {
    let rows: Vec<(&str, f64, f64)> = SLOTS
        .iter()
        .zip(SAMPLABLE)
        .map(|(&slot, product)| {
            // A plausible echo for the moment, at a hard edge.
            let echo: f32 = match slot {
                MomentSlot::Reflectivity => 65.0,
                MomentSlot::Velocity => 30.0,
                MomentSlot::SpectrumWidth => 4.0,
                MomentSlot::DifferentialReflectivity => 1.5,
                MomentSlot::DifferentialPhase => 60.0,
                MomentSlot::CorrelationCoefficient => 0.98,
            };
            let range = value_range_for(slot);
            let shipped = ramp_value_at(
                range,
                fetched_index(ramp_index(range, echo), NO_DATA_INDEX, 0.5),
            );

            // The rejected encoding, over the palette's own range.
            let legend = get_legend_scale(product);
            let (lo, hi) = (f64::from(legend.min_value), f64::from(legend.max_value));
            let oob_index = (1.0 + (f64::from(echo) - lo) / (hi - lo) * 254.0).round();
            let oob_half = fetched_index(oob_index as u8, 0, 0.5);
            let out_of_band = lo + (oob_half - 1.0) / 254.0 * (hi - lo);

            let round3 = |v: f64| (v * 1000.0).round() / 1000.0;
            (product.code(), round3(shipped), round3(out_of_band))
        })
        .collect();

    assert_eq!(
        rows,
        vec![
            // Reflectivity: unambiguously better, and the only moment with
            // a transparent band to fade into.
            ("ref", 16.25, 32.352),
            // Velocity: ours is a −17 m/s *inbound* shell around every
            // outbound couplet edge; the out-of-band ramp's midpoint is
            // near zero, which the palette paints dark.
            ("vel", -17.0, -3.119),
            // Spectrum width and ΦDP: a wash, sub-metre and sub-degree.
            ("sw", 1.875, 1.985),
            // ZDR: ours saturates the negative extreme, theirs sits near
            // the neutral 0 dB.
            ("zdr", -3.219, -0.258),
            ("phi", 29.055, 29.203),
            // ρHV: ours paints a 0.588 shell — squarely in the
            // debris/non-meteorological band — around every echo edge and
            // around the whole volume boundary.
            ("rho", 0.588, 0.714),
        ],
        "half-edge fetch, shipped vs out-of-band, per moment",
    );

    // When this table was first measured, all five non-reflectivity
    // moments were fully opaque at every data entry — the half-edge fetch
    // painted at full strength, and the measurement's conclusion was that
    // WP-I had to supply the fade itself. It now has: every moment's table
    // carries either a transparent band (shaped to its own physics by
    // `volume_alpha_scale`) or a flat translucency (ΦDP), so the wrong
    // colours tabulated above land at reduced or zero alpha wherever the
    // profile says the value is background.
    for (slot, product) in SLOTS.iter().zip(SAMPLABLE) {
        if product == RadarProduct::Reflectivity {
            continue;
        }
        let range = value_range_for(*slot);
        let lut = colormap_lut(product, range);
        let see_through = lut
            .chunks_exact(4)
            .skip(1)
            .filter(|entry| entry[3] <= SEE_THROUGH_ALPHA_CEILING)
            .count();
        assert!(
            see_through >= 16,
            "{}: only {see_through} see-through entries — the solid-block \
                 failure the profiles exist to prevent",
            product.name(),
        );
    }
}

/// How wide the **bottom** fade is, per product — the number that anchors
/// the march's skip threshold.
///
/// **Recorded, not asserted to be large**, because since the per-product
/// transparency profiles landed this is no longer the number that decides
/// drawability — [`VoxelGrid::clear_indices`] is. A diverging moment's
/// see-through band sits mid-ramp, so its bottom band is honestly 0: the
/// ramp's bottom is its saturated extreme and must stay opaque. Only the
/// two moments whose *floor* is background — reflectivity (palette's own
/// quarter-ramp fade) and spectrum width (the profile's laminar-flow
/// fade) — have one.
#[test]
fn the_fade_band_is_measured_per_product() {
    let scan = six_moment_scan();
    let mut measured = Vec::new();
    for product in VOLUME_PRODUCTS {
        let req = VoxelRequest {
            product,
            ..request(ODD)
        };
        let grid = build_voxels(&scan, &req, SITE.0, SITE.1).unwrap();
        measured.push((product.code(), grid.fade_band()));
    }
    assert_eq!(
        measured,
        vec![
            // −32.5 … −0.5 dBZ is transparent: a quarter of the ramp.
            ("ref", 64),
            // The ramp's bottom is −64 m/s, the strongest inbound air —
            // velocity's see-through band is mid-ramp, measured by
            // `the_default_transparency_profile_is_measured_per_product`.
            ("vel", 0),
            // 0 … 2 m/s: the profile's laminar-flow floor.
            ("sw", 9),
            // Bottom = −7.9 dB, a saturated hail extreme: opaque.
            ("zdr", 0),
            // Flat translucency, no transparent band at all.
            ("phi", 0),
            // Bottom = lowest ρHV, the most non-meteorological: opaque.
            ("rho", 0),
            // Bottom = −63.5 m/s of storm-relative inbound: opaque, for
            // velocity's reason.
            ("srv", 0),
            // Bottom = −4, an extreme anticyclonic couplet: opaque.
            ("nrot", 0),
            // 0 … 0.25 °/km, under the estimator's own significance —
            // KDP's span starts below zero, so the band covers the
            // negative half of the display clamp as well.
            ("kdp", 50),
        ],
        "the fade band per product",
    );

    // What that means for reflectivity, in the units that matter.
    let grid = build_voxels(&scan, &request(ODD), SITE.0, SITE.1).unwrap();
    assert_eq!(
        grid.index_to_value(grid.fade_band()),
        -0.5,
        "the top of the transparent band is the last level under 0 dBZ",
    );
    assert!(
        f64::from(grid.fade_band()) / 255.0 > 0.24,
        "a quarter of the whole ramp",
    );

    // The two ends of the measurement, which no product's palette reaches
    // and only a hand-built table can: a table opaque from index 1 has no
    // band, and one transparent throughout fades over the whole ramp.
    let mut opaque = hand_built(None);
    opaque.lut = vec![255; LUT_LEN];
    opaque.lut[3] = 0;
    assert_eq!(opaque.fade_band(), 0);
    let mut clear = hand_built(None);
    clear.lut = vec![0; LUT_LEN];
    assert_eq!(clear.fade_band(), u8::MAX);
}

/// The per-product 3D transparency profile, pinned at physical landmarks.
///
/// Each row is a rationale made testable: the value named is *why* the
/// profile has its shape, so a change to any constant in
/// `volume_alpha_profile` fails here with the physics in the message
/// rather than as an index diff. The last block is the drawability
/// measurement the renderer's solid-block gate reads.
#[test]
fn the_default_transparency_profile_is_measured_per_product() {
    let alpha = |product: RadarProduct, value: f32| {
        let range = ramp_of(product);
        let lut = colormap_lut(product, range);
        lut[usize::from(ramp_index(range, value)) * 4 + 3]
    };
    // "Solid" means the palette's own plan-view alpha, verbatim — several
    // 2D palettes are themselves translucent in places, and the profile
    // may only scale them down, never up.
    let palette_alpha = |product: RadarProduct, value: f32| {
        let range = ramp_of(product);
        get_color_for_value(product, ramp_value(range, ramp_index(range, value))).3
    };
    let solid = |product: RadarProduct, value: f32, what: &str| {
        assert_eq!(
            alpha(product, value),
            palette_alpha(product, value),
            "{what}: full plan-view strength",
        );
        assert!(alpha(product, value) > 0, "{what}: visible at all");
    };

    // Velocity: calm air is invisible, cores are solid — in both signs.
    assert_eq!(alpha(RadarProduct::Velocity, 0.0), 0, "calm air");
    assert_eq!(alpha(RadarProduct::Velocity, 3.5), 0, "ambient drift");
    assert_eq!(
        alpha(RadarProduct::Velocity, -3.5),
        0,
        "ambient drift, inbound"
    );
    solid(RadarProduct::Velocity, 30.0, "an outbound core");
    solid(RadarProduct::Velocity, -30.0, "an inbound core");
    let mid = alpha(RadarProduct::Velocity, 10.0);
    assert!(
        mid > 0 && mid < palette_alpha(RadarProduct::Velocity, 10.0),
        "the fade between drift and core is a fade, not a step: {mid}",
    );

    // Spectrum width: laminar flow is invisible, turbulence is solid.
    assert_eq!(alpha(RadarProduct::SpectrumWidth, 1.0), 0, "laminar flow");
    solid(RadarProduct::SpectrumWidth, 10.0, "turbulence");

    // ZDR: the quiet band is the interval the crate's own HCA leaves for
    // ordinary rain, and it does not contain zero. This block is the
    // regression: a profile that put a clear band on 0 dB rendered the
    // canonical tumbling-hail signature as a hole.
    let zdr = RadarProduct::DifferentialReflectivity;
    use volume_alpha_profile as p;
    assert_eq!(
        (p::ZDR_RAIN_LO_DB, p::ZDR_RAIN_HI_DB),
        (crate::hca::MIN_ZDR_BD as f32, crate::hca::MAX_ZDR_GR as f32),
        "the quiet band must stay the HCA's own rain interval",
    );
    for (value, what) in [
        (p::ZDR_RAIN_LO_DB, "the rain band's floor"),
        (1.0, "moderate rain"),
        (p::ZDR_RAIN_HI_DB, "the rain band's ceiling"),
    ] {
        assert_eq!(alpha(zdr, value), 0, "{what} is the volume's filler");
    }
    // The finding, as a number, and it is asserted through the table
    // rather than through the constants so that it is a statement about
    // what renders. Tumbling hail sits at ZDR ~ 0 under high Z —
    // `hca::HSDA_MAX_ZDR` is 2.0 and high ZDR is never large hail — so 0 dB
    // must be plainly visible, not a hole and not a whisper. A bound of
    // zero would be subsumed by this one; a third of the palette's alpha is
    // the claim worth making.
    let hail = alpha(zdr, p::ZDR_TUMBLING_DB);
    assert!(
        hail >= palette_alpha(zdr, p::ZDR_TUMBLING_DB) / 3,
        "tumbling hail at 0 dB renders at {hail} of {}: a hole where the \
             HCA's own bounds (HSDA_MAX_ZDR = {}) put the signature",
        palette_alpha(zdr, p::ZDR_TUMBLING_DB),
        crate::hca::HSDA_MAX_ZDR,
    );
    // …and a plateau, not a ramp to full. Measured over four volumes,
    // ZDR in [−0.5, +0.5] is 68 % of every data voxel in the box: a low
    // side that reached full opacity drew 91 % of the volume at a mean
    // alpha of 110 of 180, which is a wall, and a wall is the other way
    // of telling the user nothing.
    assert_eq!(
        p::ZDR_TUMBLING_ALPHA,
        p::PHI_ALPHA,
        "the plateau is the translucency this module already argues for a \
             moment with no honest background band",
    );
    // Strict, and the strictness is the whole point of the assertion. The
    // rejected ramped profile — `1 - smoothstep(-RAIN_LO, RAIN_LO, value)`
    // on the low side — lands on exactly half the palette's alpha at 0 dB,
    // 90 of 180, so `<=` admits the very shape this bound exists to
    // refuse. Half is the wall, not the last value under it.
    let ceiling = palette_alpha(zdr, p::ZDR_TUMBLING_DB) / 2;
    assert!(
        hail < ceiling,
        "tumbling hail at 0 dB renders at {hail}, at or over the {ceiling} \
             that keeps the 68 % of a volume sharing its band a haze rather \
             than a wall",
    );
    assert!(
        alpha(zdr, -1.5) > hail,
        "the plateau must still climb toward the deep negative tail",
    );
    solid(zdr, p::ZDR_NEGATIVE_DB, "the deep negative tail");
    solid(zdr, -3.5, "a three-body spike");
    solid(zdr, p::ZDR_COLUMN_DB, "a ZDR column");
    solid(zdr, 4.0, "a big-drop core");
    // Monotone away from the rain band in both directions, so "further
    // from rain is more visible" holds and no interior notch hides a
    // value between two landmarks.
    for pair in [
        [0.4f32, 0.2],
        [0.2, 0.0],
        [0.0, -0.25],
        [-1.0, -2.0],
        [2.1, 2.2],
        [2.5, 2.8],
    ] {
        let (nearer, further) = (alpha(zdr, pair[0]), alpha(zdr, pair[1]));
        assert!(
            further >= nearer,
            "ZDR {} is further from the rain band than {} and renders \
                 fainter ({further} against {nearer})",
            pair[1],
            pair[0],
        );
    }

    // ρHV: uniform precipitation is invisible — the profile inverts,
    // because this moment's background is the TOP of its scale. Debris
    // reads solid at the palette's own (translucent) strength.
    assert_eq!(
        alpha(RadarProduct::CorrelationCoefficient, 1.0),
        0,
        "pure rain"
    );
    assert_eq!(alpha(RadarProduct::CorrelationCoefficient, 0.99), 0, "rain");
    let (r, g, b, debris_2d) = get_color_for_value(RadarProduct::CorrelationCoefficient, 0.5);
    let _ = (r, g, b);
    assert_eq!(
        alpha(RadarProduct::CorrelationCoefficient, 0.5),
        debris_2d,
        "a debris signature keeps its full plan-view alpha",
    );
    assert!(
        alpha(RadarProduct::CorrelationCoefficient, 0.85)
            > alpha(RadarProduct::CorrelationCoefficient, 0.95),
        "alpha must rise as ρHV falls away from rain",
    );

    // ΦDP: flat translucency — a cumulative, site-offset moment has no
    // honest background band, so no value is favoured over another.
    let phi_alphas: Vec<u8> = {
        let range = value_range_for(MomentSlot::DifferentialPhase);
        colormap_lut(RadarProduct::DifferentialPhase, range)
            .chunks_exact(4)
            .skip(1)
            .map(|e| e[3])
            .collect()
    };
    let phi_max = *phi_alphas.iter().max().unwrap();
    assert!(
        phi_max <= 128,
        "ΦDP must stay translucent everywhere: max alpha {phi_max}",
    );
    assert!(
        phi_alphas.iter().all(|a| *a > 0),
        "…but visible everywhere it is measured: no value band is favoured",
    );

    // ── The three derived products ──────────────────────────────────
    //
    // Not one of these had a row here when they were admitted to every 3D
    // surface, and all three defects below shipped in that gap.

    // SRV: velocity's numbers under SRV's own names, so a change to
    // velocity's band cannot drag SRV along silently.
    let srv = RadarProduct::StormRelativeVelocity;
    assert_eq!(
        (p::SRV_CLEAR_MS, p::SRV_OPAQUE_MS),
        (p::VELOCITY_CLEAR_MS, p::VELOCITY_OPAQUE_MS),
        "SRV is velocity's profile today; changing that is a decision, \
             not an edit",
    );
    assert_eq!(alpha(srv, 0.0), 0, "air travelling with the storm");
    solid(srv, 30.0, "an outbound storm-relative core");
    solid(srv, -30.0, "an inbound storm-relative core");
    // The premise the entry corrects, as arithmetic. In still air SRV is
    // a cosine of amplitude equal to the storm speed, so the near-zero
    // band is a ridge perpendicular to the motion, not a background. A
    // 40 kt storm puts still air 45° off the motion axis here — kept
    // visible on purpose: subtracting it back out would be base velocity.
    let still_air_45 =
        (40.0 * crate::srv::KT_TO_MS * f64::from(std::f32::consts::FRAC_1_SQRT_2)) as f32;
    assert!(
        (still_air_45 - 14.55).abs() < 0.05,
        "still air 45° off a 40 kt motion reads {still_air_45} m/s",
    );
    let lobe = volume_alpha_scale(srv, still_air_45);
    assert!(
        (lobe - 0.73).abs() < 0.02,
        "the ambient opacity lobe measures {lobe:.3}, not the ~0.73 the \
             profile entry states",
    );

    // NROT: the finding. The clear point is the algorithm's own
    // significance floor, not a number chosen here — NROT's palette is
    // class-structured, so a higher clear point relocates the
    // nothing→weak class boundary instead of softening a gradient.
    let nrot = RadarProduct::NormalizedRotation;
    assert_eq!(
        p::NROT_CLEAR,
        crate::nrot::SIGNIFICANT as f32,
        "the volume must go visible exactly where the algorithm calls a \
             bin painted and the palette gives it its first colour",
    );
    assert_eq!(alpha(nrot, 0.0), 0, "no rotation");
    assert_eq!(alpha(nrot, 0.2), 0, "under the significance floor");
    // The contract, stated exactly: the volume goes visible on precisely
    // the ramp entries the plan view paints, and on no others. This is
    // the assertion the shipped profile failed — 8 033 of the 8 039
    // voxels a real tornado-warned volume painted came back at alpha 2–4
    // of 180, six of them visible — and it is stronger than the constant
    // it pins, because a smoothstep starting at 0 rounds the first
    // several entries of the weak class back to invisible even with the
    // clear point correct.
    {
        let range = ramp_of(nrot);
        let lut = colormap_lut(nrot, range);
        let mut painted_and_drawn = 0usize;
        for index in 1..=255u8 {
            let value = ramp_value(range, index);
            let plan = get_color_for_value(nrot, value).3;
            let volume = lut[usize::from(index) * 4 + 3];
            assert_eq!(
                volume > 0,
                plan > 0,
                "NROT index {index} ({value:.4}): the plan view paints it \
                     at {plan} and the volume draws it at {volume}",
            );
            if plan > 0 {
                painted_and_drawn += 1;
                assert!(
                    f32::from(volume) >= f32::from(plan) * p::NROT_WEAK_ALPHA - 1.0,
                    "NROT index {index} ({value:.4}) draws at {volume} of \
                         {plan}, under the weak class's own floor",
                );
            }
        }
        assert!(
            painted_and_drawn > 200,
            "precondition: only {painted_and_drawn} of 255 NROT entries \
                 are painted at all, so the agreement above is vacuous",
        );
    }
    solid(nrot, 1.0, "the mesocyclone convention");
    solid(nrot, -1.0, "an anticyclonic couplet");
    solid(nrot, 2.5, "an extreme couplet");

    // KDP: sequential like reflectivity, clear under the estimator's own
    // significance and opaque in the heavy-rain shafts.
    let kdp = RadarProduct::SpecificDifferentialPhase;
    assert_eq!(alpha(kdp, 0.0), 0, "no differential phase gradient");
    assert_eq!(alpha(kdp, 0.2), 0, "drizzle and noise");
    solid(kdp, p::KDP_OPAQUE_DEG_KM, "a heavy rain shaft");
    solid(kdp, 4.0, "a rain core");
    let kdp_mid = alpha(kdp, 0.8);
    assert!(
        kdp_mid > 0 && kdp_mid < palette_alpha(kdp, 0.8),
        "moderate KDP fades rather than steps: {kdp_mid}",
    );

    // Reflectivity: bit-exact identity with the palette — the reference
    // look every other profile is measured against.
    {
        let range = value_range_for(MomentSlot::Reflectivity);
        let lut = colormap_lut(RadarProduct::Reflectivity, range);
        for index in 1..=255u8 {
            let (_, _, _, a) =
                get_color_for_value(RadarProduct::Reflectivity, ramp_value(range, index));
            assert_eq!(
                lut[usize::from(index) * 4 + 3],
                a,
                "reflectivity entry {index}"
            );
        }
    }

    // The drawability number the solid-block gate reads, per product, and
    // the tables' alpha ceiling for context. Every palette's plan-view
    // maximum is 180 — the radar layer's own translucency convention — so
    // 180 is also the ceiling here; ΦDP's 63 is that ceiling times its
    // flat 0.35, which puts its whole 255-entry ramp under the
    // see-through bar: translucent everywhere is the other honest way not
    // to be a wall.
    let scan = six_moment_scan();
    let mut measured = Vec::new();
    for product in VOLUME_PRODUCTS {
        let req = VoxelRequest {
            product,
            ..request(ODD)
        };
        let grid = build_voxels(&scan, &req, SITE.0, SITE.1).unwrap();
        let max_alpha = grid
            .lut()
            .chunks_exact(4)
            .skip(1)
            .map(|e| e[3])
            .max()
            .unwrap();
        measured.push((product.code(), grid.see_through_indices(), max_alpha));
    }
    assert_eq!(
        measured,
        vec![
            ("ref", 64, 180),
            ("vel", 41, 180),
            ("sw", 18, 180),
            // Was 53 on the profile that put a clear band across 0 dB.
            // The band moved off zero and narrowed to the HCA's own rain
            // interval, so 11 fewer entries are see-through — and the 11
            // are the ones around the hail value. The rest of the low
            // side is a plateau at ΦDP's translucency, which is over the
            // see-through bar without being a wall.
            ("zdr", 42, 180),
            ("phi", 255, 63),
            ("rho", 35, 180),
            // Velocity's own count, which is what sharing its band means.
            ("srv", 41, 180),
            // Only the unpainted core of the ramp: |NROT| under the
            // algorithm's significance floor, on a ±4 span. Everything
            // outside it starts at the weak class's quarter alpha, which
            // is over the see-through ceiling, so the count is the
            // unpainted band and nothing else.
            ("nrot", 27, 180),
            ("kdp", 60, 180),
        ],
        "see-through data entries and max data alpha, per product",
    );
}

/// The isosurface parameters translate the user's product-unit threshold
/// through each shape — sequential, diverging, at-or-below — against the
/// grid's own ramp, and a non-finite threshold falls back to the argued
/// default instead of poisoning the uniform.
#[test]
fn the_isosurface_params_translate_the_user_threshold_per_shape() {
    let scan = six_moment_scan();
    let grid = |product| {
        let req = VoxelRequest {
            product,
            ..request(ODD)
        };
        build_voxels(&scan, &req, SITE.0, SITE.1).unwrap()
    };

    // Sequential: reflectivity at 18 dBZ — no centre, the threshold is
    // the value's own index.
    let refl = grid(RadarProduct::Reflectivity);
    let (centre, threshold) = refl.iso_uniform_params(18.0);
    assert!(centre < 0.0, "a sequential product has no diverging centre");
    assert_eq!(
        threshold,
        f32::from(refl.value_to_index(18.0)) / 255.0,
        "the surface sits exactly where the ramp puts 18 dBZ",
    );

    // Diverging: velocity at ±20 m/s — centre at 0 m/s, threshold the
    // index distance to +20, so both lobes render.
    let vel = grid(RadarProduct::Velocity);
    let (centre, threshold) = vel.iso_uniform_params(20.0);
    let c = vel.value_to_index(0.0);
    assert_eq!(centre, f32::from(c) / 255.0, "centred on calm air");
    assert_eq!(
        threshold,
        f32::from(vel.value_to_index(20.0) - c) / 255.0,
        "the crossing distance is 20 m/s of ramp",
    );
    // A negative deviation is the same surface: the shape is |v|.
    assert_eq!(vel.iso_uniform_params(-20.0), (centre, threshold));

    // At-or-below: ρHV at 0.90 — centre at the ramp top, so "at or
    // under the bound" is the same diverging test.
    let rho = grid(RadarProduct::CorrelationCoefficient);
    let (centre, threshold) = rho.iso_uniform_params(0.90);
    assert_eq!(centre, 1.0, "centred on the ramp top");
    assert_eq!(
        threshold,
        f32::from(255 - rho.value_to_index(0.90)) / 255.0,
        "the crossing distance reaches down to the bound",
    );

    // Diverging about a centre that is NOT zero — the case velocity
    // cannot test, because dropping the `centre +` term from the
    // translation passes every assertion above. ZDR is the only product
    // with a non-zero diverging centre, so this is the whole coverage of
    // that term: at +0.25 dB the slider would otherwise sit a quarter of
    // a decibel off the surface it draws.
    let zdr = grid(RadarProduct::DifferentialReflectivity);
    let centre_db = volume_alpha_profile::ZDR_CENTRE_DB;
    assert_ne!(centre_db, 0.0, "precondition: ZDR's centre is off zero");
    let (centre, threshold) = zdr.iso_uniform_params(2.75);
    let c = zdr.value_to_index(centre_db);
    assert_eq!(
        centre,
        f32::from(c) / 255.0,
        "centred on the profile's declared ZDR centre",
    );
    assert_ne!(
        centre,
        f32::from(zdr.value_to_index(0.0)) / 255.0,
        "a centre read as 0 dB rather than the profile's would pass every \
             velocity assertion above",
    );
    assert_eq!(
        threshold,
        f32::from(zdr.value_to_index(centre_db + 2.75) - c) / 255.0,
        "the crossing distance is 2.75 dB of ramp FROM the declared centre",
    );
    // Which is to say the default surface is the +3 dB column and the
    // −2.5 dB tail, not the hail value at 0 — the profile shows that one.
    let default_db = default_iso_threshold(RadarProduct::DifferentialReflectivity);
    assert_eq!(default_db, volume_alpha_profile::ZDR_COLUMN_DB - centre_db);
    // The two lobes as the numbers a user sees, so that moving the centre
    // is a change to this pair and not a silent one. The centre is a
    // display choice, argued as such where it is declared; this is what
    // holding it at 0.25 dB draws.
    assert_eq!(
        (centre_db + default_db, centre_db - default_db),
        (3.0, -2.5),
        "the default ZDR surface's positive and negative lobes",
    );

    // The derived products carry their own ramps, so their thresholds
    // must be read through those and not through the source moment's.
    // NROT spans ±4 unitless where the velocity slot it borrows spans
    // ±63.5 m/s: a translation that reached for the slot would put the
    // meso surface sixteen times too low on the ramp.
    let nrot = grid(RadarProduct::NormalizedRotation);
    let (centre, threshold) = nrot.iso_uniform_params(1.0);
    let c = nrot.value_to_index(0.0);
    assert_eq!(centre, f32::from(c) / 255.0, "centred on no rotation");
    assert_eq!(
        threshold,
        f32::from(nrot.value_to_index(1.0) - c) / 255.0,
        "the crossing distance is |NROT| = 1 of ramp",
    );
    assert_eq!(
        nrot.value_range(),
        value_range_for_product(RadarProduct::NormalizedRotation, MomentSlot::Velocity,)
    );
    assert!(
        threshold > 0.1,
        "|NROT| = 1 is an eighth of a ±4 ramp; {threshold} says the \
             surface was translated through velocity's ±63.5 span",
    );
    // SRV keeps velocity's ramp and velocity's centre; KDP is sequential
    // on its own display clamp.
    let srv = grid(RadarProduct::StormRelativeVelocity);
    assert_eq!(srv.value_range(), vel.value_range());
    assert_eq!(srv.iso_uniform_params(20.0), vel.iso_uniform_params(20.0));
    let kdp = grid(RadarProduct::SpecificDifferentialPhase);
    let (centre, threshold) = kdp.iso_uniform_params(1.5);
    assert!(centre < 0.0, "KDP is sequential");
    assert_eq!(threshold, f32::from(kdp.value_to_index(1.5)) / 255.0);

    // Every product's shape and default agree with the grid the builder
    // actually produced — the loop the derived three were never in.
    for product in VOLUME_PRODUCTS {
        let g = grid(product);
        let default = default_iso_threshold(product);
        let (centre, threshold) = g.iso_uniform_params(default);
        assert!(
            threshold.is_finite() && (0.0..=1.0).contains(&threshold),
            "{}: default threshold {default} translates to {threshold}",
            product.name(),
        );
        match iso_shape(product) {
            IsoShape::Sequential => assert!(centre < 0.0, "{}", product.name()),
            IsoShape::DeviationFrom { centre: at } => assert_eq!(
                centre,
                f32::from(g.value_to_index(at)) / 255.0,
                "{}",
                product.name(),
            ),
            IsoShape::AtOrBelow => assert_eq!(centre, 1.0, "{}", product.name()),
        }
        // Non-finite input takes the argued default, for every product.
        assert_eq!(
            g.iso_uniform_params(f32::NAN),
            (centre, threshold),
            "{}: a NaN threshold must fall back, not poison the uniform",
            product.name(),
        );
    }

    // Non-finite input: the argued default, not a NaN in a uniform lane.
    let (_, fallback) = refl.iso_uniform_params(f32::NAN);
    assert_eq!(
        fallback,
        f32::from(refl.value_to_index(default_iso_threshold(RadarProduct::Reflectivity))) / 255.0,
    );
}

/// The sampler's velocity fold guard rides into the voxel grid unchanged.
///
/// A field folding at ±24.5 m/s with a hard seam: without the guard,
/// blends across the seam invent every speed between the endpoints —
/// including calm air — and the voxel grid stores the inventions. The
/// guard (armed per rung by `Blend::folds_at_measured_limit`, applied
/// across gates and across tilts) answers with one endpoint or the other,
/// so **every** valued cell must read exactly ±24.5. The grid samples
/// through `Column::at_height_km` with no fold logic of its own, which is
/// what this pins: nobody may give the voxel path its own sampling that
/// forgets the guard.
#[test]
fn the_velocity_fold_guard_rides_into_the_voxel_grid() {
    let seam_km = 10.0;
    let field: Field<'_> = &move |_az, slant| Some(if slant < seam_km { 24.5 } else { -24.5 });
    let scan = Scan::new(
        vcp(&[0.5, 4.5]),
        vec![
            vel_sweep(
                2,
                HIGH_DEG,
                &wrapped_azimuths(360, 211.0),
                HIGH_GATES,
                field,
            ),
            vel_sweep(1, LOW_DEG, &wrapped_azimuths(720, 293.5), LOW_GATES, field),
        ],
    );
    let req = VoxelRequest {
        product: RadarProduct::Velocity,
        half_width_km: 20.0,
        top_km_msl: 4.0,
        shape: VoxelShape {
            nx: 64,
            ny: 64,
            nz: 16,
        },
        ..request(ODD)
    };
    let grid = build_voxels(&scan, &req, SITE.0, SITE.1).expect("velocity builds");

    let (mut inbound, mut outbound) = (0usize, 0usize);
    for z in 0..16 {
        for y in 0..64 {
            for x in 0..64 {
                let Some(v) = grid.value_at(x, y, z).filter(|v| v.is_finite()) else {
                    continue;
                };
                assert!(
                    v == 24.5 || v == -24.5,
                    "cell ({x},{y},{z}) reads {v} m/s across a seam whose two \
                         sides measured only ±24.5 — a blend crossed the fold",
                );
                if v > 0.0 {
                    outbound += 1;
                } else {
                    inbound += 1;
                }
            }
        }
    }
    assert!(
        inbound > 100 && outbound > 100,
        "precondition: both sides of the seam must be in the grid \
             (inbound {inbound}, outbound {outbound}), or nothing straddled",
    );
}

/// Differential phase is circular, so the two ends of its ramp are the
/// same measurement and a linear filter across the seam returns the
/// opposite phase. Named, measured, and left alone.
#[test]
fn the_wrapping_moment_is_named_and_its_seam_error_is_measured() {
    let scan = six_moment_scan();
    for product in VOLUME_PRODUCTS {
        let req = VoxelRequest {
            product,
            ..request(ODD)
        };
        let grid = build_voxels(&scan, &req, SITE.0, SITE.1).unwrap();
        assert_eq!(
            grid.wraps(),
            product == RadarProduct::DifferentialPhase,
            "{}",
            product.name(),
        );
    }

    // The seam: 1° and 359° are 2° apart on the circle. Filtered halfway
    // between their indices the fetch reads 180°, the opposite phase — an
    // error of 180°, the worst there is.
    let range = value_range_for(MomentSlot::DifferentialPhase);
    let (a, b) = (ramp_index(range, 1.0), ramp_index(range, 359.0));
    let middle = ramp_value_at(range, fetched_index(a, b, 0.5));
    assert!(
        (middle - 180.0).abs() < 1.5,
        "a fetch across the PhiDP seam reads {middle}, where the truth is \
             0 / 360",
    );
}

// ── The status the grid drops, stated ───────────────────────────────────

/// Every non-`Value` status collapses to one index. The grid carries no
/// status plane — a raymarcher has no use for one — so "below the lowest
/// beam" and "range folded" are the same byte here, and a hover readout
/// that needs the distinction must ask the sampler, not the grid.
#[test]
fn every_reason_for_no_value_collapses_to_the_one_index() {
    let range = value_range_for(MomentSlot::Reflectivity);
    for status in [
        SampleStatus::BelowThreshold,
        SampleStatus::RangeFolded,
        SampleStatus::BelowLowestBeam,
        SampleStatus::AboveVolume,
        SampleStatus::BeyondRange,
        SampleStatus::NoCoverage,
    ] {
        let sample = Sample::missing(status);
        assert_eq!(sample.value(), None, "{status:?}");
        assert_eq!(ramp_index(range, sample.value_or_nan()), NO_DATA_INDEX);
    }
}

// ── The wire codec ──────────────────────────────────────────────────────

/// Where the three trailing planes' length prefixes sit in an encoded
/// grid: after the 104-byte header, then after each preceding plane.
///
/// Written out here rather than taken from the encoder, so a layout change
/// that moved a field has to be made in both places — the mutations these
/// offsets support are the whole point of the tests below, and an offset
/// derived from the code under test would follow it wherever it went.
/// `the_length_prefixes_are_where_the_tests_think_they_are` checks them.
const HEADER_BYTES: usize = 4 + 2 + 2 + 3 * 4 + 3 * 16 + 16 + 8 + 4 + 8;
/// The first axis of the shape, so a test can plant an unsupported one.
const SHAPE_AT: usize = 4 + 2 + 2;
const LUT_LEN_AT: usize = HEADER_BYTES;
const INDEX_LEN_AT: usize = LUT_LEN_AT + 4 + LUT_LEN;

fn value_len_at(cells: usize) -> usize {
    INDEX_LEN_AT + 4 + cells
}

fn prefix_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes"))
}

/// A grid with a real echo edge in it, on the deliberately-asymmetric
/// [`ODD`] shape, and with the value plane present.
fn wire_fixture() -> VoxelGrid {
    let scan = scan_of(&|az, slant| (az < 120.0 && slant < 90.0).then_some(48.0));
    build_voxels(&scan, &request(ODD), SITE.0, SITE.1).expect("the fixture grid builds")
}

/// The "no value plane" encoding is unambiguous only because a supported
/// shape has at least one cell — otherwise `0` would mean both "absent"
/// and "as many values as this grid has cells".
///
/// A claim about [`VoxelShape::is_supported`] rather than about the codec,
/// which is why it is asserted over the boundary and the named shapes
/// rather than over one fixture.
#[test]
fn a_supported_shape_always_has_a_cell_so_an_absent_plane_is_unambiguous() {
    let smallest = VoxelShape {
        nx: 1,
        ny: 1,
        nz: 1,
    };
    for shape in [smallest, ODD, WASM_SHAPE, MOBILE_SHAPE, DESKTOP_SHAPE] {
        assert!(shape.is_supported(), "{shape:?}");
        assert!(
            shape.cells() >= 1,
            "{shape:?} is supported but has no cells, so an absent value \
                 plane and a full one encode to the same four bytes",
        );
    }
    // And the reason it holds: a zero axis is not supported in the first
    // place, on any of the three.
    for zeroed in [
        VoxelShape { nx: 0, ..smallest },
        VoxelShape { ny: 0, ..smallest },
        VoxelShape { nz: 0, ..smallest },
    ] {
        assert!(!zeroed.is_supported(), "{zeroed:?}");
        assert_eq!(zeroed.cells(), 0);
    }
}

/// The three offsets the mutation tests below index by are the three the
/// encoder actually wrote.
///
/// Every refusal test plants a value at one of them, so an offset that had
/// drifted would leave those tests corrupting a byte of some other field
/// and passing for the wrong reason — the classic way a suite of negative
/// assertions goes green while testing nothing.
#[test]
fn the_length_prefixes_are_where_the_tests_think_they_are() {
    let grid = wire_fixture();
    let bytes = grid.to_bytes();
    let cells = ODD.cells();
    assert_eq!(prefix_at(&bytes, SHAPE_AT), ODD.nx as u32);
    assert_eq!(prefix_at(&bytes, SHAPE_AT + 4), ODD.ny as u32);
    assert_eq!(prefix_at(&bytes, SHAPE_AT + 8), ODD.nz as u32);
    assert_eq!(prefix_at(&bytes, LUT_LEN_AT), LUT_LEN as u32);
    assert_eq!(prefix_at(&bytes, INDEX_LEN_AT), cells as u32);
    assert_eq!(prefix_at(&bytes, value_len_at(cells)), cells as u32);
    assert_eq!(bytes.len(), value_len_at(cells) + 4 + cells * 4);
}

/// The version this layout ships is **1**, and it sits where a decoder
/// from another build reads it — as does the magic, `RDVX` by literal.
///
/// `a_malformed_grid_payload_is_refused_rather_than_misread` plants
/// `0xFF 0xFF` in the version and watches the decode refuse, which pins
/// that *a* version check exists — not *which* version ships. Both ends of
/// this codec move together, so every other test here round-trips through
/// one build and passes whatever the constant says; the constant is only
/// load-bearing *between* builds.
///
/// Between builds is where it is the only defence — **in a dev build**.
/// `rustdar-web`'s page/worker handshake is `build_token =
/// version/PROTOCOL_VERSION/GITHUB_SHA`, and `GITHUB_SHA` is absent outside
/// CI, so locally it degrades to `…/dev` and a stale worker shares a token
/// with a fresh page. In a deployed build the SHA differs across the deploy
/// boundary, the tokens disagree, and `worker_port::handle_message` terminates
/// the worker at the HELLO handshake — before any payload is exchanged, so
/// this constant is never reached. `RDVX` has
/// never been bumped, so the exposure is the *first* bump being forgotten:
/// a layout change that reorders two same-width fields — the three `f64`
/// axis ranges, or `site` against them — round-trips perfectly through its
/// own build's codec, so the stale worker would decode a fresh payload
/// into the new field order and raymarch a volume with its axes swapped.
///
/// The magic is here for the same reason at lower stakes. The relabel loop
/// in that test pins `RDVX` only against `RDRI` and `RDXS`, its two
/// port-mates; any *unused* four bytes would have stayed green, and the
/// far end of the port has no matching constant to move with it. A changed
/// magic is at least a clean refusal rather than a misparse.
///
/// Byte 4 is where the version starts because [`to_bytes`](VoxelGrid::to_bytes)
/// writes [`MAGIC`] and then it — the same reading [`SHAPE_AT`] is built
/// on. Mirrors `render_input`'s and `xsect`'s tests of the same name.
///
/// # Why the coverage-premultiplied texture did NOT bump it
///
/// `rustdar-frontend` now uploads the grid as `Rg16Float` with
/// `R = coverage x index` and `G = coverage`, and quadrupling a texture is the
/// shape of change that usually earns a bump. This one does not, because
/// **not one byte of this payload changed, in layout or in meaning**:
/// coverage is exactly `index != NO_DATA_INDEX` (pinned by
/// `coverage_is_exactly_whether_the_index_is_the_no_data_one`), so the
/// second channel is a function of the first, and it is synthesised at
/// upload time by `volume::raymarch::coverage_premultiplied` rather than
/// carried. Putting it on the wire would double the worker transfer and the
/// host residency — 8 MiB to 16 MiB at [`DESKTOP_SHAPE`] — to move no
/// information.
///
/// So old and new payloads are not merely unconfusable, they are
/// *identical*, and both decode correctly under both renderers. A bump here
/// would be a pure cost, and the cost is a **dev-build** one: the build-token
/// handshake degrades to `…/dev` outside CI, so there a stale worker does
/// share a token with a fresh page and reaches this decode, and a gratuitous
/// bump would make that pair drop every reply — a pane stuck on "Building
/// the … volume…" until the developer reloads — in exchange for refusing
/// payloads that are correct. (In a deployed build the SHA differs and the
/// mismatched worker is already terminated at HELLO, so a bump would cost
/// nothing there and buy nothing either.)
///
/// The bump obligation is unchanged for anything that touches the bytes:
/// if coverage ever stops being derivable from the index (a non-binary
/// coverage from sub-cell occupancy, say, which would have to be carried),
/// that is a layout change and it bumps.
#[test]
fn the_format_version_is_the_one_this_layout_ships() {
    assert_eq!(FORMAT_VERSION, 1);
    let bytes = wire_fixture().to_bytes();
    assert_eq!(&bytes[..4], b"RDVX", "the magic moved");
    assert_eq!(
        u16::from_le_bytes([bytes[4], bytes[5]]),
        1,
        "the version is not where a decoder from another build looks for it",
    );
    // The claim above, executed: the payload's index plane is one byte per
    // cell, so a build that started carrying a coverage plane would grow it
    // and would have to bump.
    let grid = wire_fixture();
    assert_eq!(
        grid.indices().len(),
        grid.shape().cells(),
        "the payload carries more than one byte per cell — a coverage plane \
         on the wire is a layout change and must bump FORMAT_VERSION",
    );
}

/// A real grid survives the wire, for every product — derivations
/// included — with and without the value plane.
///
/// This is what [`VoxelGrid`]'s hand-written `PartialEq` was written for:
/// the value plane is `NaN` wherever the radar did not reach, which on a
/// cube over a cone is most of it, and under derived semantics
/// `assert_eq!` here would fail on a byte-identical payload with nothing
/// in the message saying why.
#[test]
fn a_grid_round_trips_through_its_wire_form() {
    let scan = six_moment_scan();
    for product in VOLUME_PRODUCTS {
        for values_wanted in [true, false] {
            let req = VoxelRequest {
                product,
                values_wanted,
                ..request(ODD)
            };
            let grid = build_voxels(&scan, &req, SITE.0, SITE.1)
                .unwrap_or_else(|| panic!("{} builds", product.name()));
            let what = format!("{} values={values_wanted}", product.name());

            if values_wanted {
                // precondition: without a NaN in the plane the round trip
                // would pass under a derived `PartialEq` too, and the
                // claim about the codec would be weaker than it looks.
                assert!(
                    grid.values().unwrap().iter().any(|v| v.is_nan()),
                    "{what}: the value plane has no NaN in it",
                );
                assert!(
                    grid.values().unwrap().iter().any(|v| v.is_finite()),
                    "{what}: the value plane has no numbers in it",
                );
            }

            let decoded = VoxelGrid::from_bytes(&grid.to_bytes())
                .unwrap_or_else(|| panic!("{what} did not decode"));
            assert_eq!(grid, decoded, "{what} changed in transit");
            // The absent plane is a state of its own and has to survive as
            // one: `Some(vec![NaN; cells])` compares unequal to `None`, so
            // this is not implied by the assertion above, but stating it
            // says which way round the failure would be.
            assert_eq!(
                decoded.values().is_some(),
                values_wanted,
                "{what}: the value plane's presence did not survive",
            );
            assert_eq!(decoded.product(), product, "{what}");
            assert_eq!(decoded.shape(), ODD, "{what}");
            assert_eq!(decoded.lut(), grid.lut(), "{what}");
            assert_eq!(decoded.tilt_count(), grid.tilt_count(), "{what}");
            // And re-encoding is byte-identical, which says more than
            // equality does: `PartialEq` compares the value plane bitwise,
            // but an encoder that reordered two fields of equal width
            // would still satisfy it.
            assert_eq!(grid.to_bytes(), decoded.to_bytes(), "{what}");
        }
    }

    // The comparison is not vacuous: two grids that differ decode to two
    // grids that differ, in the plane, in the shape and in the box.
    let a = build_voxels(&scan, &request(ODD), SITE.0, SITE.1).unwrap();
    let elsewhere = build_voxels(
        &scan,
        &VoxelRequest {
            half_width_km: 61.0,
            ..request(ODD)
        },
        SITE.0,
        SITE.1,
    )
    .unwrap();
    let lean = build_voxels(
        &scan,
        &VoxelRequest {
            values_wanted: false,
            ..request(ODD)
        },
        SITE.0,
        SITE.1,
    )
    .unwrap();
    for (name, other) in [("a different box", &elsewhere), ("no value plane", &lean)] {
        assert_ne!(
            VoxelGrid::from_bytes(&a.to_bytes()).unwrap(),
            VoxelGrid::from_bytes(&other.to_bytes()).unwrap(),
            "{name} decoded to the same grid",
        );
    }
}

/// `to_bytes` reserves exactly what it writes. A grid is up to 40 MiB, so
/// a wrong estimate is a copy of all of it.
#[test]
fn the_encoded_length_of_a_grid_is_exact() {
    let scan = six_moment_scan();
    // Both plane states and two shapes, so the estimate is pinned against
    // more than one total — the optional plane is the term most likely to
    // be dropped from it.
    for shape in [
        ODD,
        VoxelShape {
            nx: 4,
            ny: 5,
            nz: 3,
        },
    ] {
        for values_wanted in [true, false] {
            let req = VoxelRequest {
                values_wanted,
                shape,
                ..request(shape)
            };
            let grid = build_voxels(&scan, &req, SITE.0, SITE.1).unwrap();
            assert_eq!(
                grid.encoded_len(),
                grid.to_bytes().len(),
                "{shape:?} values={values_wanted}",
            );
        }
    }
}

/// The header's geometry numbers must all be finite, and the two fields
/// that are **functions of the product** must agree with the product.
///
/// `CrossSection::from_parts` already refuses a non-finite axis; this is
/// the same hole one level over, plus the pair a grid states twice. None of
/// these fails downstream — that is the point. A `NaN` extent divides into
/// a `NaN` cell size and every `cell_centre_km` answers `NaN`; an infinite
/// one collapses the cell size to zero and stacks every cell centre in one
/// place; a mismatched ramp reads the indices off a scale they were not
/// quantised against; a mismatched table paints another product's colours
/// over this product's numbers. Each renders, and each is wrong in a way
/// that looks like weather.
#[test]
fn a_grid_header_that_cannot_describe_its_own_product_is_refused() {
    let good = wire_fixture().to_bytes();
    assert!(
        VoxelGrid::from_bytes(&good).is_some(),
        "precondition: the unmutated payload must decode, or every \
             assertion below passes for the wrong reason"
    );

    // The four f64 pairs and the lone f64, by offset into the header.
    for (name, at) in [
        ("x_range.0", 20),
        ("x_range.1", 28),
        ("y_range.0", 36),
        ("y_range.1", 44),
        ("z_range.0", 52),
        ("z_range.1", 60),
        ("site.0", 68),
        ("site.1", 76),
        ("widest_tilt_gap_deg", 96),
    ] {
        for (what, bits) in [("NaN", f64::NAN), ("inf", f64::INFINITY)] {
            let mut bad = good.clone();
            bad[at..at + 8].copy_from_slice(&bits.to_le_bytes());
            assert!(
                VoxelGrid::from_bytes(&bad).is_none(),
                "{name} = {what} decoded",
            );
        }
    }
    // precondition: those offsets really name the header fields, rather
    // than landing in the padding of a layout that has since moved.
    let mut moved = good.clone();
    moved[20..28].copy_from_slice(&(-999.0f64).to_le_bytes());
    assert!(
        VoxelGrid::from_bytes(&moved).is_some_and(|g| g.x_range_km().0 == -999.0),
        "offset 20 is not x_range.0, so the finiteness assertions above are \
             corrupting some other field into invalidity",
    );

    // The ramp is `value_range_for(slot)` and nothing else, so any other
    // pair is a payload whose indices mean something this build cannot
    // reproduce.
    //
    // Planted **with the colour table that matches it**, which is the whole
    // point: a bare range edit is also caught by the table check below,
    // because that recomputes the table from the decoded range — so it
    // leaves the range check itself untested. Only a self-consistent
    // wrong-ramp payload — which is exactly what a build with a different
    // quantisation would send — isolates it.
    let mut ramp = good.clone();
    let bogus = (0.0f32, 60.0f32);
    assert_ne!(
        bogus,
        value_range_for(MomentSlot::Reflectivity),
        "precondition: the planted range is the real one",
    );
    ramp[84..88].copy_from_slice(&bogus.0.to_le_bytes());
    ramp[88..92].copy_from_slice(&bogus.1.to_le_bytes());
    ramp[LUT_LEN_AT + 4..LUT_LEN_AT + 4 + LUT_LEN]
        .copy_from_slice(&colormap_lut(RadarProduct::Reflectivity, bogus));
    assert!(
        VoxelGrid::from_bytes(&ramp).is_none(),
        "a value range this product's quantisation never produces decoded, \
             carrying a colour table built to agree with it — so `index_to_value` \
             would have read every index off the wrong scale",
    );

    // A length-correct table built for a different product. The fixture is
    // reflectivity; velocity's ramp colours the same 256 indices
    // completely differently.
    let alien = colormap_lut(
        RadarProduct::Velocity,
        value_range_for(MomentSlot::Velocity),
    );
    assert_eq!(alien.len(), LUT_LEN, "precondition: same length");
    let mut swapped = good.clone();
    swapped[LUT_LEN_AT + 4..LUT_LEN_AT + 4 + LUT_LEN].copy_from_slice(&alien);
    assert_ne!(
        good[LUT_LEN_AT + 4..LUT_LEN_AT + 4 + LUT_LEN],
        swapped[LUT_LEN_AT + 4..LUT_LEN_AT + 4 + LUT_LEN],
        "precondition: the two palettes are identical here, so swapping \
             them proves nothing",
    );
    assert!(
        VoxelGrid::from_bytes(&swapped).is_none(),
        "a colour table built for another product decoded, and the \
             raymarch would have painted it",
    );
}

/// The bytes arrive off a message port. Every malformed shape has to be a
/// clean `None` — the two ends of that port can be different builds.
#[test]
fn a_malformed_grid_payload_is_refused_rather_than_misread() {
    let grid = wire_fixture();
    let good = grid.to_bytes();
    let cells = ODD.cells();
    let values_prefix_at = value_len_at(cells);

    assert!(VoxelGrid::from_bytes(&[]).is_none(), "empty");
    assert!(VoxelGrid::from_bytes(b"nope").is_none(), "wrong magic");

    // A **whole** payload relabelled, including with the two magics that
    // share this port. Mutation testing is why: a four-byte buffer cannot
    // pin the magic test, because it fails on the version read instead —
    // deleting the magic comparison outright left every short-buffer
    // assertion here green, and a section frame would then have been
    // decoded as a grid.
    for wrong in [*b"nope", *b"RDRI", *b"RDXS"] {
        let mut relabelled = good.clone();
        relabelled[..4].copy_from_slice(&wrong);
        assert!(
            VoxelGrid::from_bytes(&relabelled).is_none(),
            "a whole payload labelled {} decoded as a grid",
            String::from_utf8_lossy(&wrong),
        );
    }

    let mut wrong_version = good.clone();
    wrong_version[4] = 0xFF;
    wrong_version[5] = 0xFF;
    assert!(
        VoxelGrid::from_bytes(&wrong_version).is_none(),
        "an unknown version decoded",
    );

    // A product code this build does not have, and one it has but cannot
    // resample. The second is the interesting half: `RadarProduct` knows
    // what VIL is, so only the `samplable` refusal stops a payload whose
    // `value_range` came from no moment's ramp.
    let mut unknown_product = good.clone();
    unknown_product[6..8].copy_from_slice(&0xFFFEu16.to_le_bytes());
    assert!(
        VoxelGrid::from_bytes(&unknown_product).is_none(),
        "an unknown product code decoded",
    );
    let mut underivable = good.clone();
    underivable[6..8].copy_from_slice(
        &RadarProduct::VerticallyIntegratedLiquid
            .wire_code()
            .to_le_bytes(),
    );
    assert!(
        samplable(RadarProduct::VerticallyIntegratedLiquid).is_none(),
        "precondition: VIL became samplable, so this is no longer the \
             refusal this assertion is about",
    );
    assert!(
        VoxelGrid::from_bytes(&underivable).is_none(),
        "a product with no native moment decoded",
    );

    // An axis outside `1..=MAX_AXIS`, on each of the three, at both ends.
    for axis in 0..3 {
        for bad in [0u32, (MAX_AXIS + 1) as u32, u32::MAX] {
            let mut broken = good.clone();
            broken[SHAPE_AT + axis * 4..SHAPE_AT + axis * 4 + 4]
                .copy_from_slice(&bad.to_le_bytes());
            assert!(
                VoxelGrid::from_bytes(&broken).is_none(),
                "axis {axis} of {bad} decoded",
            );
        }
    }
    // A *supported* shape that is not the one the planes are sized for is
    // refused too, and by the plane checks rather than by `is_supported` —
    // this is the cross-build case, since the three named shapes differ.
    let mut reshaped = good.clone();
    reshaped[SHAPE_AT..SHAPE_AT + 4].copy_from_slice(&((ODD.nx + 1) as u32).to_le_bytes());
    assert!(
        VoxelGrid::from_bytes(&reshaped).is_none(),
        "a shape claiming more cells than the index plane holds decoded — \
             every accessor indexes that plane with an offset from the shape",
    );

    // An unsupported shape whose planes **agree** with it, so nothing but
    // `is_supported` can object. Mutation testing needs these two: every
    // bad-axis assertion above is caught downstream by the index-plane
    // length instead, and deleting `is_supported` from `from_bytes`
    // altogether left all of them green.
    //
    // Zero is the dangerous half, and it is why `is_supported` refuses a
    // zero axis rather than yielding an empty grid: without the guard this
    // frame decodes into a grid of no cells, whose extents a renderer
    // divides by a zero dimension to get an infinity, and which is
    // indistinguishable from a volume with nothing in it.
    for axis in 0..3 {
        let mut empty = good[..INDEX_LEN_AT].to_vec();
        empty[SHAPE_AT + axis * 4..SHAPE_AT + axis * 4 + 4].copy_from_slice(&0u32.to_le_bytes());
        // Both planes sized to match `cells() == 0`: none, and absent.
        empty.extend_from_slice(&0u32.to_le_bytes());
        empty.extend_from_slice(&0u32.to_le_bytes());
        assert!(
            VoxelGrid::from_bytes(&empty).is_none(),
            "axis {axis} of zero decoded into a grid with no cells",
        );
    }

    // And the other end, past `MAX_AXIS`, again with planes to match: one
    // axis at the GLES 3.0 guarantee, bumped one over it, and both planes
    // grown by the one cell that implies.
    let tall = VoxelShape {
        nx: MAX_AXIS,
        ny: 1,
        nz: 1,
    };
    let over_shape = build_voxels(&scan_of(&|_, _| Some(40.0)), &request(tall), SITE.0, SITE.1)
        .expect("a shape at the guarantee builds");
    let mut over = over_shape.to_bytes();
    let tall_cells = tall.cells();
    over[SHAPE_AT..SHAPE_AT + 4].copy_from_slice(&((MAX_AXIS + 1) as u32).to_le_bytes());
    over[INDEX_LEN_AT..INDEX_LEN_AT + 4].copy_from_slice(&((tall_cells + 1) as u32).to_le_bytes());
    over.insert(INDEX_LEN_AT + 4 + tall_cells, NO_DATA_INDEX);
    // The insert above pushed the value prefix along by that one byte.
    let moved = value_len_at(tall_cells) + 1;
    over[moved..moved + 4].copy_from_slice(&((tall_cells + 1) as u32).to_le_bytes());
    over.extend_from_slice(&f32::NAN.to_le_bytes());
    assert!(
        VoxelGrid::from_bytes(&over).is_none(),
        "an axis of {} — one over the GLES 3.0 guarantee — decoded, with \
             planes sized to agree with it",
        MAX_AXIS + 1,
    );

    for cut in [
        1,
        8,
        SHAPE_AT,
        HEADER_BYTES,
        LUT_LEN_AT + 4,
        INDEX_LEN_AT,
        INDEX_LEN_AT + 4,
        values_prefix_at,
        values_prefix_at + 4,
        good.len() / 2,
        good.len() - 1,
    ] {
        assert!(
            VoxelGrid::from_bytes(&good[..cut]).is_none(),
            "truncated to {cut} bytes",
        );
    }

    let mut trailing = good.clone();
    trailing.push(0);
    assert!(
        VoxelGrid::from_bytes(&trailing).is_none(),
        "trailing bytes mean the layouts disagree",
    );

    // A length that cannot fit in what remains, on each of the three
    // planes. The value plane is the one that matters: four bytes an
    // element, so a believed `u32::MAX` reserves 16 GiB before the read
    // fails.
    for (name, at) in [
        ("table", LUT_LEN_AT),
        ("index", INDEX_LEN_AT),
        ("value", values_prefix_at),
    ] {
        let mut absurd = good.clone();
        absurd[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(
            VoxelGrid::from_bytes(&absurd).is_none(),
            "an absurd {name} plane length reached a read",
        );
    }

    // Each plane one element short of what the shape declares, with its
    // prefix moved to match, so the frame is well-formed right through to
    // `at_end` and only the plane check can object.
    for (name, at, element) in [
        ("table", LUT_LEN_AT, 1usize),
        ("index", INDEX_LEN_AT, 1),
        ("value", values_prefix_at, 4),
    ] {
        let mut short = good.clone();
        let count = prefix_at(&short, at) as usize;
        let plane_end = at + 4 + count * element;
        short[at..at + 4].copy_from_slice(&((count - 1) as u32).to_le_bytes());
        short.drain(plane_end - element..plane_end);
        assert!(
            VoxelGrid::from_bytes(&short).is_none(),
            "a {name} plane one element short decoded",
        );
    }

    // A value plane that is neither absent nor the size of the grid. One
    // element is the shape a sender that meant "no plane" but wrote a
    // sentinel would produce, and it must not be read as either.
    let mut one_value = good.clone();
    one_value.truncate(values_prefix_at + 4 + 4);
    one_value[values_prefix_at..values_prefix_at + 4].copy_from_slice(&1u32.to_le_bytes());
    assert!(
        VoxelGrid::from_bytes(&one_value).is_none(),
        "a one-element value plane decoded",
    );

    // Absent, on the other hand, is a state: zero and nothing after it is
    // the encoding `values_wanted: false` produces, and it decodes.
    let mut absent = good.clone();
    absent.truncate(values_prefix_at + 4);
    absent[values_prefix_at..values_prefix_at + 4].copy_from_slice(&0u32.to_le_bytes());
    let decoded = VoxelGrid::from_bytes(&absent)
        .expect("a grid with no value plane is a grid, not a malformed one");
    assert_eq!(decoded.values(), None);
    assert_eq!(decoded.indices(), grid.indices());

    // precondition: the fixture the mutations were made against decodes,
    // so every refusal above is the mutation's doing and not the
    // fixture's.
    assert_eq!(
        VoxelGrid::from_bytes(&good).expect("the unmutated payload decodes"),
        grid,
    );
    assert!(
        VoxelGrid::from_bytes(&over_shape.to_bytes()).is_some(),
        "precondition: the shape-at-the-guarantee payload does not decode \
             unmutated either, so the assertion about it says nothing",
    );
}

/// The capacity guard, tested directly, because nothing end to end can
/// see it.
///
/// [`Reader::bounded`] does not change *what* [`VoxelGrid::from_bytes`]
/// answers. `take` bounds every read, so a believed length fails on the
/// read either way and the payload is refused with or without it. What it
/// changes is whether four billion elements are reserved **first** — a
/// 16 GiB allocation on the way to a `None`, on a worker thread, in a
/// browser tab. Mutation testing confirms the gap rather than assuming it:
/// deleting the call from `from_bytes` leaves the whole suite green, which
/// is why the helper is named and pinned here instead.
#[test]
fn the_capacity_guard_refuses_a_length_the_buffer_cannot_hold() {
    let bytes = [0u8; 16];
    let r = Reader::new(&bytes);
    assert_eq!(r.bounded(4, 4), Some(4), "16 bytes hold four f32");
    assert_eq!(r.bounded(0, 4), Some(0));
    assert_eq!(r.bounded(5, 4), None, "20 bytes claimed from 16");
    assert_eq!(r.bounded(u32::MAX, 4), None, "16 GiB claimed from 16 bytes");

    // It measures against what is *left*, not against the whole buffer —
    // otherwise a length prefix late in a frame would be judged against
    // bytes already consumed.
    let mut part_way = Reader::new(&bytes);
    part_way.take(8).expect("half the buffer");
    assert_eq!(part_way.bounded(2, 4), Some(2));
    assert_eq!(part_way.bounded(3, 4), None);

    // And the multiply cannot overflow into a pass.
    assert_eq!(Reader::new(&bytes).bounded(u32::MAX, usize::MAX), None);
}

/// Coverage is **exactly** `index != NO_DATA_INDEX`, for every product — the
/// premise the renderer's coverage-premultiplied texture rests on.
///
/// `rustdar-frontend` uploads this grid as `Rg16Float` with `R = coverage x
/// index` and `G = coverage`, and it synthesises that second channel at upload
/// time from the index plane alone rather than carrying it on the wire. That is
/// only lossless if no measurement can encode as index 0 and no absence can
/// encode as anything else — which is exactly what
/// `no_measurement_encodes_as_the_no_data_index` and the `NaN` arm of
/// `ramp_index` say, read here from the renderer's side of the contract.
///
/// This replaces `only_the_bottom_transparent_sequential_ramps_may_blend_into_no_data`,
/// which pinned the per-product blend-or-march-nearest census that
/// `no_data_blends_at_ramp_bottom` held. That table is gone: with coverage in
/// the texture a filtered sample beside empty air lands inside the convex hull
/// of the stored indices around it, for every product, so there is no longer a
/// per-product decision to pin.
#[test]
fn coverage_is_exactly_whether_the_index_is_the_no_data_one() {
    for product in RadarProduct::all() {
        let Some(slot) = crate::derive::volume_slot(*product) else {
            continue;
        };
        let range = value_range_for_product(*product, slot);
        // An absence is index 0 and nothing else is.
        for absent in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(
                ramp_index(range, absent),
                NO_DATA_INDEX,
                "{}: {absent} does not encode as no-data, so coverage 0 would \
                 lose a cell the grid says is empty",
                product.code(),
            );
        }
        // And no finite measurement anywhere on or beyond the ramp does, so
        // coverage 1 never lands on a cell the grid says is empty.
        let (lo, hi) = range;
        let span = f64::from(hi) - f64::from(lo);
        for step in 0..=512 {
            let value = (f64::from(lo) + span * f64::from(step) / 256.0) as f32;
            assert_ne!(
                ramp_index(range, value),
                NO_DATA_INDEX,
                "{}: {value} encodes as the no-data index, so the renderer \
                 would give a measured cell coverage 0",
                product.code(),
            );
        }
    }
}
