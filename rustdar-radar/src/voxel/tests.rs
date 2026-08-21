use super::*;
use crate::beam;
use crate::sampler::{Sample, SampleStatus, samplable};
use nexrad_model::data::{
    ChannelConfiguration, ElevationCut, MomentData, PulseWidth, Radial, RadialStatus, Scan, Sweep,
    VolumeCoveragePattern, WaveformType,
};

// ── Fixtures ────────────────────────────────────────────────────────────

const REFL_SCALE: f32 = 2.0;
const REFL_OFFSET: f32 = 66.0;
const FIRST_GATE_M: u16 = 2125;
const GATE_M: u16 = 250;

const SITE: (f64, f64) = (35.33306, -97.2775);
const SITE_ELEV_FT: f64 = 1214.0 + 62.0;

fn encode_refl(dbz: f64) -> u8 {
    ((dbz * f64::from(REFL_SCALE) + f64::from(REFL_OFFSET)).round() as i64).clamp(2, 255) as u8
}

fn round_trip_refl(dbz: f64) -> f32 {
    (f32::from(encode_refl(dbz)) - REFL_OFFSET) / REFL_SCALE
}

fn gate_slant_km(j: usize) -> f64 {
    f64::from(FIRST_GATE_M) / 1000.0 + j as f64 * f64::from(GATE_M) / 1000.0
}

type Field<'f> = &'f dyn Fn(f64, f64) -> Option<f64>;

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

const LOW_DEG: f32 = 0.53;
const HIGH_DEG: f32 = 4.47;
const LOW_GATES: usize = 600; // to 151.9 km slant
const HIGH_GATES: usize = 200; // stops at 51.9 km — range truncation

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

fn six_moment_scan() -> Scan {
    // (scale, offset) per moment, from the ICD.
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
                let moment = |slot: usize| {
                    let (scale, offset) = CODECS[slot];
                    let floor = usize::from(if slot == 2 { 129u8 } else { 2 });
                    let bytes: Vec<u8> = if slot == 1 {
                        let r = f64::from(az).to_radians();
                        let cos_el = f64::from(elevation_deg).to_radians().cos();
                        let vr = (18.0 * r.sin() + -11.0 * r.cos()) * cos_el;
                        let code = ((vr * f64::from(scale) + f64::from(offset)).round() as i64)
                            .clamp(2, 255) as u8;
                        vec![code; n_gates]
                    } else {
                        (0..n_gates)
                            .map(|j| (floor + ((j * 7 + slot * 31 + i) % (256 - floor))) as u8)
                            .collect()
                    };
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
            sweep(2, HIGH_DEG, 41.0, SIX_MOMENT_HIGH_GATES),
        ],
    )
}

const SIX_MOMENT_HIGH_GATES: usize = 320;

fn one_rung_carries_data(carrier: Option<usize>) -> Scan {
    let full: Field<'_> = &|_, _| Some(45.0);
    let empty: Field<'_> = &|_, _| None;
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
        half_extent_km: Some(HalfExtentKm::square(60.0)),
        base_km_msl: 0.0,
        top_km_msl: 12.0,
        product: RadarProduct::Reflectivity,
        shape,
        values_wanted: true,
    }
}

const ODD: VoxelShape = VoxelShape {
    nx: 11,
    ny: 13,
    nz: 7,
};

const SLOTS: [MomentSlot; 6] = [
    MomentSlot::Reflectivity,
    MomentSlot::Velocity,
    MomentSlot::SpectrumWidth,
    MomentSlot::DifferentialReflectivity,
    MomentSlot::DifferentialPhase,
    MomentSlot::CorrelationCoefficient,
];

const SAMPLABLE: [RadarProduct; 6] = [
    RadarProduct::Reflectivity,
    RadarProduct::Velocity,
    RadarProduct::SpectrumWidth,
    RadarProduct::DifferentialReflectivity,
    RadarProduct::DifferentialPhase,
    RadarProduct::CorrelationCoefficient,
];

const DERIVED: [RadarProduct; 3] = [
    RadarProduct::StormRelativeVelocity,
    RadarProduct::NormalizedRotation,
    RadarProduct::SpecificDifferentialPhase,
];

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

fn ramp_of(product: RadarProduct) -> (f32, f32) {
    value_range_for_product(product, crate::derive::volume_slot(product).unwrap())
}

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

#[test]
fn the_named_shapes_cost_what_the_module_doc_says() {
    const MIB: usize = 1024 * 1024;
    assert_eq!(WASM_SHAPE.cells(), MIB, "wasm: 1 MiB of indices");
    assert_eq!(MOBILE_SHAPE.cells(), 3_538_944, "mobile: 3.375 MiB");
    assert_eq!(DESKTOP_SHAPE.cells(), 8 * MIB, "desktop: 8 MiB");
    assert_eq!(DESKTOP_SHAPE.cells() * 4, 32 * MIB);
}

#[test]
fn default_shape_is_the_targets() {
    const GUARANTEE: usize = 256;
    #[cfg(target_arch = "wasm32")]
    assert_eq!(
        default_shape(GUARANTEE),
        shape_for_budget(WASM_SHAPE, GUARANTEE)
    );
    #[cfg(not(target_arch = "wasm32"))]
    assert_eq!(default_shape(GUARANTEE), DESKTOP_SHAPE);
    assert_ne!(
        default_shape(GUARANTEE),
        shape_for_budget(MOBILE_SHAPE, GUARANTEE),
        "this crate has no build script, so it cannot see the `mobile` \
             cfg; the frontend selects MOBILE_SHAPE explicitly",
    );
}

// ── The rebalance ───────────────────────────────────────────────────────
const REPORTED_LIMITS: [usize; 5] = [256, 512, 704, 1024, 2048];

const TIERS: [(&str, VoxelShape); 3] = [
    ("wasm", WASM_SHAPE),
    ("mobile", MOBILE_SHAPE),
    ("desktop", DESKTOP_SHAPE),
];

#[test]
fn the_rebalanced_shapes_are_the_ones_the_rule_documents() {
    const RING_KM: f64 = 920.25;
    const SPAN_KM: f64 = DEFAULT_TOP_KM_MSL - DEFAULT_BASE_KM_MSL;
    let expected = [
        (
            "wasm",
            WASM_SHAPE,
            VoxelShape {
                nx: 256,
                ny: 256,
                nz: 16,
            },
            3.595,
            1.125,
        ),
        (
            "mobile",
            MOBILE_SHAPE,
            VoxelShape {
                nx: 320,
                ny: 320,
                nz: 32,
            },
            2.876,
            0.5625,
        ),
        (
            "desktop",
            DESKTOP_SHAPE,
            VoxelShape {
                nx: 512,
                ny: 512,
                nz: 32,
            },
            1.797,
            0.5625,
        ),
    ];
    for (name, shipped, want, horizontal_km, vertical_km) in expected {
        let got = shape_for_budget(shipped, 2048);
        assert_eq!(got, want, "{name}");
        assert!(
            (RING_KM / got.nx as f64 - horizontal_km).abs() < 0.001,
            "{name}: {} cells over the ring is {} km, not the documented \
             {horizontal_km}",
            got.nx,
            RING_KM / got.nx as f64,
        );
        assert!(
            (SPAN_KM / got.nz as f64 - vertical_km).abs() < 0.001,
            "{name}: {} layers over {SPAN_KM} km is {} km, not the \
             documented {vertical_km}",
            got.nz,
            SPAN_KM / got.nz as f64,
        );
    }
}

#[test]
fn a_rebalanced_shape_never_outgrows_the_budget_it_came_from() {
    for (name, shipped) in TIERS {
        for limit in REPORTED_LIMITS {
            let got = shape_for_budget(shipped, limit);
            assert!(
                got.cells() <= shipped.cells(),
                "{name} at a {limit} limit: {got:?} is {} cells against a \
                 {} cell budget",
                got.cells(),
                shipped.cells(),
            );
            assert!(got.is_supported(), "{name} at a {limit} limit: {got:?}");
        }
    }
}

#[test]
fn every_axis_stays_within_the_limit_the_device_reported() {
    for (name, shipped) in TIERS {
        for limit in REPORTED_LIMITS {
            let got = shape_for_budget(shipped, limit);
            for (axis, n) in [("nx", got.nx), ("ny", got.ny), ("nz", got.nz)] {
                assert!(
                    n <= limit,
                    "{name} at a {limit} limit: {axis} is {n}, which the \
                     device cannot allocate",
                );
            }
        }
    }
    let huge = shape_for_budget(DESKTOP_SHAPE, usize::MAX);
    assert!(huge.is_supported(), "{huge:?}");
}

#[test]
fn the_horizontal_axis_is_a_multiple_of_the_copy_alignment() {
    for (name, shipped) in TIERS {
        for limit in REPORTED_LIMITS {
            let got = shape_for_budget(shipped, limit);
            assert_eq!(
                got.nx % HORIZONTAL_AXIS_MULTIPLE,
                0,
                "{name} at a {limit} limit: {} is not a multiple of \
                 {HORIZONTAL_AXIS_MULTIPLE}, so every row of its staging \
                 buffer is padded",
                got.nx,
            );
            assert_eq!(got.nx, got.ny, "{name} at a {limit} limit");
        }
    }
    assert_eq!(shape_for_budget(DESKTOP_SHAPE, 32).nx, 32);
    assert_eq!(shape_for_budget(DESKTOP_SHAPE, 0).nx, 1);
}

#[test]
fn a_conservative_adapter_gets_the_shape_that_shipped() {
    assert_eq!(shape_for_budget(DESKTOP_SHAPE, 256), DESKTOP_SHAPE);
    assert_eq!(
        shape_for_budget(DESKTOP_SHAPE, 256),
        VoxelShape {
            nx: 256,
            ny: 256,
            nz: 128
        },
    );
    assert_eq!(
        shape_for_budget(WASM_SHAPE, 256),
        VoxelShape {
            nx: 256,
            ny: 256,
            nz: 16
        },
    );
    assert_eq!(shape_for_budget(WASM_SHAPE, 128), WASM_SHAPE);
}

#[test]
fn the_deeper_vertical_is_taken_only_where_it_buys_horizontal() {
    for (name, shipped) in [("mobile", MOBILE_SHAPE), ("desktop", DESKTOP_SHAPE)] {
        let got = shape_for_budget(shipped, 2048);
        assert!(
            got.nz >= NZ_PREFERRED,
            "{name}: {got:?} took the shallow vertical although it could \
             afford {NZ_PREFERRED} layers",
        );
        assert!(
            got.nx > shipped.nx,
            "{name}: {got:?} is no wider than the {} it shipped, so the \
             deeper vertical bought nothing",
            shipped.nx,
        );
    }
    let web = shape_for_budget(WASM_SHAPE, 2048);
    assert_eq!(web.nz, NZ_MIN, "the web falls back: {web:?}");
    assert!(
        web.nx > WASM_SHAPE.nx,
        "the fallback has to be the arm that gains: {web:?}",
    );
    let free = (WASM_SHAPE.cells() / NZ_PREFERRED).isqrt();
    assert_eq!(free, 181, "32 layers leave the web 181 cells across");
    assert_eq!(
        free - free % HORIZONTAL_AXIS_MULTIPLE,
        WASM_SHAPE.nx,
        "aligned, that is the axis the web already had",
    );
}

#[test]
fn the_vertical_rungs_are_the_ones_the_beam_justifies() {
    const BEAM: f64 = 0.95_f64 * std::f64::consts::PI / 180.0;
    const REACH_KM: f64 = 460.125;
    const SPAN_KM: f64 = DEFAULT_TOP_KM_MSL - DEFAULT_BASE_KM_MSL;

    let honest_share = |nz: usize| {
        let cell_km = SPAN_KM / nz as f64;
        let from_km = cell_km / BEAM;
        1.0 - (from_km / REACH_KM).powi(2)
    };

    for (nz, want) in [
        (64, 0.9986),
        (NZ_PREFERRED, 0.9946),
        (24, 0.9903),
        (NZ_MIN, 0.9783),
        (8, 0.9130),
    ] {
        assert!(
            (honest_share(nz) - want).abs() < 0.0001,
            "{nz} layers is honest over {}, not the documented {want}",
            honest_share(nz),
        );
    }

    let below = 1.0 - honest_share(8);
    let at = 1.0 - honest_share(NZ_MIN);
    assert!(
        (below - 4.0 * at).abs() < 1e-9,
        "the quadrupling is structural: {below} against {at}",
    );
    assert!(
        below > 0.08 && at < 0.025,
        "{NZ_MIN} is the last rung above the cliff only if the step below \
         it loses a tenth of the region ({below}) where it loses a \
         fortieth ({at})",
    );
    let beyond = honest_share(64) - honest_share(NZ_PREFERRED);
    assert!(
        beyond < 0.005,
        "{NZ_PREFERRED} is only a stopping point if {beyond} is under half \
         a point",
    );
}

#[test]
fn both_target_classes_get_their_own_default_shape() {
    assert_eq!(default_shape_for(true), WASM_SHAPE);
    assert_eq!(default_shape_for(false), DESKTOP_SHAPE);
    assert_ne!(default_shape_for(true), default_shape_for(false));
}

#[test]
fn an_axis_outside_the_arithmetic_bound_is_refused() {
    let scan = scan_of(&|_, _| Some(40.0));
    for bad in [
        VoxelShape { nx: 0, ..ODD },
        VoxelShape { ny: 0, ..ODD },
        VoxelShape { nz: 0, ..ODD },
        VoxelShape {
            nx: MAX_AXIS + 1,
            ..ODD
        },
        VoxelShape {
            ny: MAX_AXIS + 1,
            ..ODD
        },
        VoxelShape {
            nz: MAX_AXIS + 1,
            ..ODD
        },
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
        "{MAX_AXIS} is the bound itself, so it is allowed",
    );
}

#[test]
fn the_arithmetic_bound_is_the_largest_cubable_axis() {
    assert_eq!(MAX_AXIS, 1625);
    let cube = |n: u128| n * n * n;
    assert!(
        cube(MAX_AXIS as u128) <= u128::from(u32::MAX),
        "{MAX_AXIS}³ = {} overflows a 32-bit cell count",
        cube(MAX_AXIS as u128),
    );
    assert!(
        cube(MAX_AXIS as u128 + 1) > u128::from(u32::MAX),
        "{}³ fits too, so the bound is not the largest",
        MAX_AXIS + 1,
    );
    assert!(u16::try_from(MAX_AXIS).is_ok());
}

// ── Refusals ────────────────────────────────────────────────────────────

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
        let cases = [
            VoxelRequest {
                half_extent_km: Some(HalfExtentKm::square(bad)),
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

#[test]
fn the_half_width_is_clamped_rather_than_refused() {
    let scan = scan_of(&|_, _| Some(40.0));
    // `MAX_HALF_DIAGONAL_KM / hypot(600, 300)` — the factor both axes of the
    // last row take. The rule itself is asserted as a property below the loop.
    const SCALE: f64 = 0.9908470001860922;
    for (asked, want) in [
        (
            HalfExtentKm::square(0.0),
            HalfExtentKm::square(MIN_HALF_WIDTH_KM),
        ),
        (
            HalfExtentKm::square(1.0),
            HalfExtentKm::square(MIN_HALF_WIDTH_KM),
        ),
        (
            HalfExtentKm::square(-500.0),
            HalfExtentKm::square(MIN_HALF_WIDTH_KM),
        ),
        (HalfExtentKm::square(60.0), HalfExtentKm::square(60.0)),
        (
            HalfExtentKm::square(10_000.0),
            HalfExtentKm::square(MAX_HALF_WIDTH_KM),
        ),
        (
            HalfExtentKm {
                east_km: 200.0,
                north_km: 80.0,
            },
            HalfExtentKm {
                east_km: 200.0,
                north_km: 80.0,
            },
        ),
        (
            HalfExtentKm {
                east_km: 4.0,
                north_km: 100.0,
            },
            HalfExtentKm {
                east_km: MIN_HALF_WIDTH_KM,
                north_km: 100.0,
            },
        ),
        (
            HalfExtentKm {
                east_km: 600.0,
                north_km: 300.0,
            },
            HalfExtentKm {
                east_km: 600.0 * SCALE,
                north_km: 300.0 * SCALE,
            },
        ),
    ] {
        let req = VoxelRequest {
            half_extent_km: Some(asked),
            ..request(ODD)
        };
        let grid = build_voxels(&scan, &req, SITE.0, SITE.1)
            .unwrap_or_else(|| panic!("{asked:?} should clamp, not refuse"));
        let (x0, x1) = grid.x_range_km();
        let (y0, y1) = grid.y_range_km();
        assert!(
            (x1 - x0 - 2.0 * want.east_km).abs() < 1e-6
                && (y1 - y0 - 2.0 * want.north_km).abs() < 1e-6,
            "asked {asked:?}, wanted {want:?}, got x {:?} y {:?}",
            grid.x_range_km(),
            grid.y_range_km(),
        );
    }

    let over = HalfExtentKm {
        east_km: 600.0,
        north_km: 300.0,
    };
    let stopped = over.clamped();
    assert!(
        (stopped.corner_km() - MAX_HALF_DIAGONAL_KM).abs() < 1e-9,
        "the stop is on the corner, so a stopped box's corner sits on it: {}",
        stopped.corner_km(),
    );
    assert!(
        (stopped.east_km / stopped.north_km - over.east_km / over.north_km).abs() < 1e-12,
        "the stop must not change the box's aspect ratio: {:?} against {:?}",
        stopped.east_km / stopped.north_km,
        over.east_km / over.north_km,
    );
}

#[test]
fn the_cap_circumscribes_the_widest_ring_rather_than_fitting_inside_it() {
    assert!(
        (HalfExtentKm::square(MAX_HALF_WIDTH_KM).corner_km() - MAX_HALF_DIAGONAL_KM).abs() < 1e-9,
        "a box at the square cap must have its corner exactly on the bound, \
         got {}",
        HalfExtentKm::square(MAX_HALF_WIDTH_KM).corner_km(),
    );
    assert!(
        (MAX_HALF_DIAGONAL_KM - crate::types::MAX_EXTENT_KM * std::f64::consts::SQRT_2).abs()
            < 1e-9,
        "the corner bound must be the plan view's cap read through the \
         circumscribed geometry — strictly wider than the cap itself, or it \
         squashes the very box it exists to admit, and drift-proof because it \
         is still derived from it",
    );
    assert!(
        (MAX_HALF_WIDTH_KM - crate::types::MAX_EXTENT_KM).abs() < 1e-9,
        "the square cap must be 470.00 km, got {MAX_HALF_WIDTH_KM}",
    );
    assert!(
        (MAX_HALF_WIDTH_KM - 460.125 - 9.875).abs() < 1e-9,
        "the margin over the widest real surveillance cut moved",
    );

    let stopped = HalfExtentKm::square(10_000.0).clamped();
    assert!(
        (stopped.east_km - MAX_HALF_WIDTH_KM).abs() < 1e-9
            && (stopped.north_km - MAX_HALF_WIDTH_KM).abs() < 1e-9,
        "a square ask past the stop must land on the cap, got {stopped:?}",
    );
}

// ── The box's own extent ────────────────────────────────────────────────

const SURVEILLANCE_GATES: usize = 1832;

fn long_range_scan(field: Field<'_>) -> Scan {
    Scan::new(
        vcp(&[0.5, 1.5]),
        vec![
            refl_sweep(
                2,
                1.51,
                &wrapped_azimuths(360, 41.0),
                SURVEILLANCE_GATES,
                field,
            ),
            refl_sweep(
                1,
                LOW_DEG,
                &wrapped_azimuths(720, 293.5),
                SURVEILLANCE_GATES,
                field,
            ),
        ],
    )
}

#[test]
fn the_box_is_the_smallest_square_holding_the_datas_range_circle() {
    for reach in [120.0f64, 300.0, 460.125] {
        let half = box_half_width_km(reach);
        assert!(
            (half - reach).abs() < 1e-9,
            "a {reach} km reach must earn a {reach} km half-width so the ring \
             is tangent to the box's sides, got {half}",
        );
    }
    assert!(
        (box_half_width_km(460.125) - 460.125).abs() < 1e-3,
        "the WSR-88D's own surveillance reach must earn 460.125 km — a 920.25 \
         km box holding the whole ring — got {}",
        box_half_width_km(460.125),
    );
}

#[test]
fn the_box_stops_at_both_ends_and_refuses_to_guess_from_no_reach() {
    assert_eq!(box_half_width_km(60_000.0), MAX_HALF_WIDTH_KM);
    assert_eq!(box_half_width_km(f64::INFINITY), MAX_HALF_WIDTH_KM);
    assert!(
        (MAX_HALF_WIDTH_KM - crate::types::MAX_EXTENT_KM).abs() < 1e-9,
        "the box's cap must be the raster's cap read through this rule's own \
         geometry, or one of them can be raised without the other",
    );
    assert_eq!(box_half_width_km(1.0), MIN_HALF_WIDTH_KM);
    for no_reach in [f64::NAN, 0.0, -1.0] {
        assert_eq!(
            box_half_width_km(no_reach),
            BASE_HALF_WIDTH_KM,
            "{no_reach} is not a reach; the box must fall back rather than \
             clamp it",
        );
    }
}

#[test]
fn the_reach_is_the_volumes_longest_cut_measured_over_the_ground() {
    let field = &|_: f64, _: f64| Some(30.0);
    let scan = scan_of(field);
    let reach = volume_reach_km(&scan, RadarProduct::Reflectivity);
    let low_slant = gate_slant_km(LOW_GATES);
    assert!(
        (reach - low_slant * f64::from(LOW_DEG).to_radians().cos()).abs() < 1e-9,
        "expected the low cut's {low_slant} km shortened to the ground, got {reach}",
    );
    assert!(
        reach < low_slant,
        "a ground range is shorter than the slant range it came from: \
         {reach} against {low_slant}",
    );

    assert_eq!(volume_reach_km(&scan, RadarProduct::Velocity), 0.0);
    assert_eq!(
        box_half_width_km(volume_reach_km(&scan, RadarProduct::Velocity)),
        BASE_HALF_WIDTH_KM,
    );
}

#[test]
fn a_long_range_volumes_box_holds_the_echo_the_fixed_box_cut_off() {
    const BEACON_KM: (f64, f64) = (0.0, 280.0);
    const BEACON_RADIUS_KM: f64 = 22.0;
    let field = &|az_deg: f64, slant_km: f64| -> Option<f64> {
        let az = az_deg.to_radians();
        let (x, y) = (slant_km * az.sin(), slant_km * az.cos());
        ((x - BEACON_KM.0).hypot(y - BEACON_KM.1) <= BEACON_RADIUS_KM).then_some(55.0)
    };
    let scan = long_range_scan(field);

    let following = VoxelRequest {
        half_extent_km: None,
        top_km_msl: 18.0,
        shape: DESKTOP_SHAPE,
        ..request(DESKTOP_SHAPE)
    };
    let grid = build_voxels(&scan, &following, SITE.0, SITE.1).expect("a buildable grid");
    let (y0, y1) = grid.y_range_km();
    assert!(
        y1 > BEACON_KM.1 + BEACON_RADIUS_KM,
        "the fixture is broken: the beacon's far edge is outside the box it \
         is meant to be inside ({y0} .. {y1} km)",
    );

    let cut = grid.value_to_index(50.0).max(1);
    let shape = grid.dims();
    let mut lit = 0usize;
    let mut farthest = 0.0f64;
    for iy in 0..shape.ny {
        for ix in 0..shape.nx {
            for iz in 0..shape.nz {
                if grid.index_at(ix, iy, iz).expect("an in-grid cell") >= cut {
                    let (cx, cy, _) = grid.cell_centre_km(ix, iy, iz).expect("an in-grid cell");
                    lit += 1;
                    farthest = farthest.max(cx.hypot(cy));
                    break;
                }
            }
        }
    }
    assert!(
        lit > 0 && farthest > 230.0,
        "the beacon 280 km out must be in the volume: {lit} columns lit, the \
         farthest at {farthest:.1} km",
    );

    let fixed = VoxelRequest {
        half_extent_km: Some(HalfExtentKm::square(BASE_HALF_WIDTH_KM)),
        ..following
    };
    let old = build_voxels(&scan, &fixed, SITE.0, SITE.1).expect("a buildable grid");
    let old_cut = old.value_to_index(50.0).max(1);
    let old_lit = old
        .indices()
        .iter()
        .filter(|&&index| index >= old_cut)
        .count();
    assert_eq!(
        old_lit, 0,
        "the fixed box must not reach this beacon, or the pin above is not \
         measuring the change",
    );
}

#[test]
fn a_short_range_volumes_box_tightens_onto_its_data() {
    const GATES: usize = 347; // 2.125 + 86.75 = 88.875 km of slant range
    let field = &|_: f64, _: f64| Some(12.0);
    let scan = Scan::new(
        vcp(&[0.5, 1.5]),
        vec![
            vel_sweep(1, LOW_DEG, &wrapped_azimuths(360, 117.5), GATES, field),
            vel_sweep(2, 1.51, &wrapped_azimuths(360, 41.0), GATES, field),
        ],
    );

    let req = VoxelRequest {
        half_extent_km: None,
        product: RadarProduct::Velocity,
        shape: DESKTOP_SHAPE,
        ..request(DESKTOP_SHAPE)
    };
    let grid = build_voxels(&scan, &req, SITE.0, SITE.1).expect("a buildable grid");
    let (x0, x1) = grid.x_range_km();
    let half = 0.5 * (x1 - x0);
    let reach = volume_reach_km(&scan, RadarProduct::Velocity);
    assert!(
        (half - reach).abs() < 1e-9,
        "an {reach} km volume must earn a {reach} km half-width — its own ring, \
         circumscribed — got {half}",
    );
    assert!(
        half < BASE_HALF_WIDTH_KM,
        "a volume that stops at {reach} km must not be given the box a \
         460 km one gets: {half} km",
    );
    let cells = f64::from(DESKTOP_SHAPE.nx as u32);
    assert!(
        2.0 * half / cells < 2.0 * BASE_HALF_WIDTH_KM / cells,
        "tightening onto the data has to buy resolution, or there is no \
         reason to prefer it",
    );
}

// ── Orientation and cell centres ────────────────────────────────────────

#[test]
fn the_grid_is_indexed_x_east_y_north_z_up() {
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
    let req = VoxelRequest {
        half_extent_km: Some(HalfExtentKm::square(40.0)),
        base_km_msl: 0.5,
        top_km_msl: 5.5,
        ..request(shape)
    };
    let grid = build_voxels(&scan, &req, SITE.0, SITE.1).unwrap();

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

    assert_eq!(
        grid.index_at(east, north, shape.nz - 1),
        Some(NO_DATA_INDEX),
    );

    // ── and on a box that is not square ──────────────────────────────
    let rect = HalfExtentKm {
        east_km: 60.0,
        north_km: 25.0,
    };
    let req = VoxelRequest {
        half_extent_km: Some(rect),
        ..req
    };
    let grid = build_voxels(&scan, &req, SITE.0, SITE.1).unwrap();
    assert_eq!(grid.x_range_km(), (-60.0, 60.0));
    assert_eq!(grid.y_range_km(), (-25.0, 25.0));

    let strong = grid.value_at(east, north, iz).unwrap();
    assert!(
        (strong - round_trip_refl(60.0)).abs() < 0.05,
        "north-east should read the 60 dBZ quadrant on a rectangular box too, \
         read {strong}",
    );
    for (x, y, corner) in [
        (west, north, "north-west"),
        (east, south, "south-east"),
        (west, south, "south-west"),
    ] {
        let weak = grid.value_at(x, y, iz).unwrap();
        assert!(
            (weak - round_trip_refl(15.0)).abs() < 0.05,
            "{corner} should read the 15 dBZ background on a rectangular box \
             too, read {weak}",
        );
    }
}

#[test]
fn cell_centres_sit_at_the_half_step() {
    let scan = scan_of(&|_, slant| Some(20.0 + beam::ground_range_km(slant, f64::from(LOW_DEG))));
    let shape = VoxelShape {
        nx: 2,
        ny: 1,
        nz: 3,
    };
    let req = VoxelRequest {
        half_extent_km: Some(HalfExtentKm::square(40.0)),
        base_km_msl: 0.5,
        top_km_msl: 1.7,
        ..request(shape)
    };
    let grid = build_voxels(&scan, &req, SITE.0, SITE.1).unwrap();

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

    // ── and on a box that is not square ──────────────────────────────
    let shape = VoxelShape {
        nx: 2,
        ny: 2,
        nz: 3,
    };
    let req = VoxelRequest {
        half_extent_km: Some(HalfExtentKm {
            east_km: 40.0,
            north_km: 10.0,
        }),
        base_km_msl: 0.5,
        top_km_msl: 1.7,
        ..request(shape)
    };
    let grid = build_voxels(&scan, &req, SITE.0, SITE.1).unwrap();
    assert_eq!(
        grid.cell_centre_km(0, 0, 0).map(|c| (c.0, c.1)),
        Some((-20.0, -5.0)),
    );
    assert_eq!(
        grid.cell_centre_km(1, 1, 0).map(|c| (c.0, c.1)),
        Some((20.0, 5.0)),
    );

    let want = round_trip_refl(20.0 + 20.0f64.hypot(5.0));
    for ix in 0..2 {
        for iy in 0..2 {
            for iz in 0..shape.nz {
                let read = grid.value_at(ix, iy, iz).unwrap();
                assert!(
                    (read - want).abs() < 0.3,
                    "column ({ix}, {iy}) row {iz} sits at 20.62 km ground \
                     range, so the field reads {want} dBZ; got {read}",
                );
            }
        }
    }
}

#[test]
fn the_height_axis_is_msl_above_the_sites_own_elevation() {
    crate::sites::fixture::install();
    let scan = scan_of(&|_, _| Some(35.0));
    let nz = 240;
    let (base_km_msl, top_km_msl) = (0.0, 12.0);
    let dz = (top_km_msl - base_km_msl) / nz as f64;
    let shape = VoxelShape { nx: 2, ny: 1, nz };
    let req = VoxelRequest {
        half_extent_km: Some(HalfExtentKm::square(40.0)),
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

#[test]
fn the_centre_may_sit_away_from_the_site() {
    let scan = scan_of(&|_, _| Some(30.0));
    let east_lon = SITE.1 + 50.0 / (rustdar_geo::KM_PER_DEGREE_LAT * SITE.0.to_radians().cos());
    let req = VoxelRequest {
        centre: (SITE.0, east_lon),
        half_extent_km: Some(HalfExtentKm::square(20.0)),
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
    assert_eq!(grid.anchor(), SITE);

    let centred = build_voxels(&scan, &request(ODD), SITE.0, SITE.1).unwrap();
    assert_eq!(centred.x_range_km(), (-60.0, 60.0));
    assert_eq!(centred.y_range_km(), (-60.0, 60.0));
}

#[test]
fn the_output_carries_everything_a_model_matrix_needs() {
    let scan = scan_of(&|_, _| Some(35.0));
    let req = VoxelRequest {
        half_extent_km: Some(HalfExtentKm::square(37.5)),
        base_km_msl: 0.75,
        top_km_msl: 15.25,
        ..request(ODD)
    };
    let grid = build_voxels(&scan, &req, SITE.0, SITE.1).unwrap();
    assert_eq!(grid.dims(), ODD);
    assert_eq!(grid.x_range_km(), (-37.5, 37.5));
    assert_eq!(grid.y_range_km(), (-37.5, 37.5));
    assert_eq!(grid.z_range_km_msl(), (0.75, 15.25));
    assert_eq!(grid.anchor(), SITE);
    assert_eq!(
        crate::fields::product_for(grid.field()),
        Some(RadarProduct::Reflectivity)
    );
    assert_eq!(
        grid.value_range(),
        (-32.5, 95.0),
        "255 data levels of 0.5 dBZ from −32.0, with index 0 half a step \
             under the bottom of them",
    );

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
    assert_eq!(grid.cell_centre_km(11, 0, 0), None);
    assert_eq!(grid.cell_centre_km(0, 13, 0), None);
    assert_eq!(grid.cell_centre_km(0, 0, 7), None);
    assert_eq!(grid.index_at(11, 0, 0), None);
    assert_eq!(grid.value_at(0, 0, 7), None);
}

#[test]
fn the_grid_reports_the_ladder_it_was_built_from() {
    let scan = scan_of(&|_, _| Some(35.0));
    let grid = build_voxels(&scan, &request(ODD), SITE.0, SITE.1).unwrap();
    assert_eq!(grid.levels(), 2);
    assert!(
        (grid.widest_level_gap_deg() - (f64::from(HIGH_DEG) - f64::from(LOW_DEG))).abs() < 1e-6,
        "0.53° and 4.47° are 3.94° apart; reported {}",
        grid.widest_level_gap_deg(),
    );
    assert!(grid.widest_level_gap_deg() > 3.0);

    let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();
    assert_eq!(grid.levels(), sampler.tilt_count());
    assert_eq!(grid.widest_level_gap_deg(), sampler.widest_tilt_gap_deg());
}

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
    assert_eq!(grid.levels(), 1);
    assert_eq!(grid.widest_level_gap_deg(), 0.0);
    assert!(
        grid.indices().iter().all(|&i| i == NO_DATA_INDEX),
        "one rung has no vertical extent, so nothing may be filled in",
    );
    let two = scan_of(&|_, _| Some(50.0));
    let filled = build_voxels(&two, &request(ODD), SITE.0, SITE.1).unwrap();
    assert!(filled.indices().iter().any(|&i| i != NO_DATA_INDEX));
}

#[test]
fn a_layer_is_quantised_to_the_ladder_rather_than_to_nz() {
    crate::sites::fixture::install();
    let nz = 200;
    let (base_km_msl, top_km_msl) = (0.0, 12.0);
    let dz = (top_km_msl - base_km_msl) / nz as f64;
    let shape = VoxelShape { nx: 2, ny: 1, nz };
    let req = VoxelRequest {
        half_extent_km: Some(HalfExtentKm::square(200.0)),
        base_km_msl,
        top_km_msl,
        ..request(shape)
    };
    let site_km_msl = SITE_ELEV_FT * 0.0003048;
    let beam = |deg: f64| beam::height_at_ground_km(100.0, deg);
    let (low, middle, high) = (beam(0.53), beam(2.47), beam(4.51));

    // ── a layer measured on exactly one rung ──
    let grid = build_voxels(&one_rung_carries_data(Some(1)), &req, SITE.0, SITE.1).unwrap();
    assert_eq!(grid.levels(), 3, "all three rungs must survive");
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
    assert_eq!(missed.levels(), 3, "the ladder is the same one");
    assert!(
        missed.indices().iter().all(|&i| i == NO_DATA_INDEX),
        "a layer between tilts is measured by nothing and painted nowhere, \
             however fine the grid",
    );
}

// ── The builder adds no geometry of its own ─────────────────────────────

#[test]
fn every_cell_is_the_samplers_own_answer() {
    crate::sites::fixture::install();
    let scan = scan_of(&|az, slant| (az < 200.0).then_some(10.0 + (slant % 37.0) + az / 12.0));
    let shape = VoxelShape {
        nx: 9,
        ny: 8,
        nz: 6,
    };
    let req = VoxelRequest {
        half_extent_km: Some(HalfExtentKm::square(55.0)),
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
    assert!(
        with_data > 0 && with_data < shape.cells(),
        "precondition: the fixture must produce both data and no-data \
             cells; got {with_data} of {}",
        shape.cells(),
    );
}

#[test]
fn nothing_is_extrapolated_outside_the_ladder() {
    let scan = scan_of(&|_, _| Some(45.0));
    let shape = VoxelShape {
        nx: 3,
        ny: 3,
        nz: 40,
    };
    let req = VoxelRequest {
        half_extent_km: Some(HalfExtentKm::square(220.0)),
        base_km_msl: 0.0,
        top_km_msl: 25.0,
        ..request(shape)
    };
    let grid = build_voxels(&scan, &req, SITE.0, SITE.1).unwrap();

    let centre = (shape.nx / 2, shape.ny / 2);
    assert!(
        (0..shape.nz).all(|iz| grid.index_at(centre.0, centre.1, iz) == Some(NO_DATA_INDEX)),
        "the cone of silence must stay empty",
    );

    assert!(
        (0..shape.nz).all(|iz| grid.index_at(0, 0, iz) == Some(NO_DATA_INDEX)),
        "311 km is past the last gate of both tilts",
    );

    let top = shape.nz - 1;
    assert!(
        (0..shape.nx)
            .all(|ix| (0..shape.ny).all(|iy| grid.index_at(ix, iy, top) == Some(NO_DATA_INDEX))),
        "25 km MSL is above the highest tilt at every range in this box",
    );

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
    assert_eq!(lean.indices(), full.indices());
}

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
    assert!(
        !a.values()
            .unwrap()
            .iter()
            .zip(b.values().unwrap())
            .all(|(x, y)| x == y),
        "an element-wise `==` over the value planes disagrees, which is \
             exactly what `#[derive(PartialEq)]` would have used",
    );

    let moved = VoxelRequest {
        half_extent_km: Some(HalfExtentKm::square(61.0)),
        ..request(ODD)
    };
    assert_ne!(a, build_voxels(&scan, &moved, SITE.0, SITE.1).unwrap());
    let lean = VoxelRequest {
        values_wanted: false,
        ..request(ODD)
    };
    assert_ne!(a, build_voxels(&scan, &lean, SITE.0, SITE.1).unwrap());
}

fn hand_built(values: Option<Vec<f32>>) -> VolumeGrid {
    let value_range = value_range_for(MomentSlot::Reflectivity);
    VolumeGrid::from_parts(VolumeParts {
        indices: vec![0, 7, 200, 255],
        values,
        dims: VoxelShape {
            nx: 2,
            ny: 2,
            nz: 1,
        },
        anchor: SITE,
        x_range_km: (-10.0, 10.0),
        y_range_km: (-10.0, 10.0),
        z_range_km_msl: (0.0, 5.0),
        field: crate::fields::known::REFLECTIVITY,
        transfer: transfer_table_over(
            colormap_lut(RadarProduct::Reflectivity, value_range),
            RadarProduct::Reflectivity,
            value_range,
        ),
        levels: 2,
        widest_level_gap_deg: 3.94,
    })
}

#[test]
fn the_value_plane_is_compared_bit_for_bit_and_its_absence_is_a_state() {
    let nan = f32::NAN;
    let a = hand_built(Some(vec![nan, -20.0, 45.0, 62.5]));
    assert_eq!(a, hand_built(Some(vec![nan, -20.0, 45.0, 62.5])));

    let different = hand_built(Some(vec![nan, -20.0, 45.25, 62.5]));
    assert_eq!(
        a.indices(),
        different.indices(),
        "precondition: only the value plane may differ, or this proves \
             nothing about `same_values`",
    );
    assert_ne!(a, different, "a different value plane is a different grid");

    assert_ne!(a, hand_built(Some(vec![nan, -20.0, 45.0])));

    let other_nan = hand_built(Some(vec![
        f32::from_bits(nan.to_bits() ^ 1),
        -20.0,
        45.0,
        62.5,
    ]));
    assert!(other_nan.values().unwrap()[0].is_nan());
    assert_ne!(a, other_nan);

    assert_eq!(hand_built(None), hand_built(None));
    assert_ne!(a, hand_built(None));
    assert_ne!(hand_built(None), a);
}

#[test]
fn debug_is_a_summary_rather_than_the_grid() {
    let scan = scan_of(&|az, _| (az < 90.0).then_some(48.0));
    let grid = build_voxels(&scan, &request(ODD), SITE.0, SITE.1).unwrap();
    let text = format!("{grid:?}");
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(text.len() < 400, "{} chars: {text}", text.len());
    // WO-M14a: the summary names the grid's FIELD, and a field is an open id
    // now rather than this crate's enum — so the identity token moved from
    // the product's short code `ref` to the registered spelling
    // `Reflectivity`. The substrate cannot reach `ProductSpec::code`: it has
    // the id and no registry to look one up in. Same property, new spelling.
    assert!(text.contains("Reflectivity"), "{text}");
    assert!(text.contains("11x13x7"), "{text}");

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
        assert!(
            (f64::from(lo) - f64::from(range.0) - step).abs() < step * 1e-4,
            "{slot:?}: index 0 must sit one full step ({step}) below {lo}, \
                 sits {} below",
            f64::from(lo) - f64::from(range.0),
        );
    }
}

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
    let range = value_range_for(MomentSlot::DifferentialPhase);
    for step in 0..=3600 {
        let value = step as f32 / 10.0;
        assert_ne!(ramp_index(range, value), NO_DATA_INDEX, "PhiDP {value}");
    }
    let refl = value_range_for(MomentSlot::Reflectivity);
    assert_eq!(ramp_index(refl, -1000.0), 1);
    assert_eq!(ramp_index(refl, 1000.0), 255);
    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert_eq!(ramp_index(refl, bad), NO_DATA_INDEX, "{bad}");
    }
}

#[test]
fn a_value_outside_the_declared_span_clamps_to_the_nearest_data_level() {
    let range = value_range_for(MomentSlot::SpectrumWidth);
    let (lo, hi) = data_levels(MomentSlot::SpectrumWidth);
    let impossible = (5.0 - 129.0) / 2.0;
    assert!(impossible < lo, "precondition: {impossible} is under {lo}");
    assert_eq!(
        ramp_index(range, impossible),
        1,
        "an under-range value takes the bottom data level, not no-data",
    );
    assert_eq!(ramp_index(range, hi + 100.0), 255);
    for slot in SLOTS {
        let range = value_range_for(slot);
        let (lo, hi) = data_levels(slot);
        assert_eq!(ramp_index(range, lo - 1e6), 1, "{slot:?}");
        assert_eq!(ramp_index(range, hi + 1e6), 255, "{slot:?}");
    }
}

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
    assert!((step(MomentSlot::DifferentialPhase) - 1.417_32).abs() < 1e-5);
    assert!((step(MomentSlot::CorrelationCoefficient) - 0.003_385_8).abs() < 1e-7);
}

// ── The colour table ────────────────────────────────────────────────────

#[test]
fn every_volume_product_builds_a_populated_grid_and_a_full_table() {
    assert_eq!(LUT_LEN, 1024);
    let scan = six_moment_scan();
    for product in VOLUME_PRODUCTS {
        let req = VoxelRequest {
            product,
            half_extent_km: Some(HalfExtentKm::square(40.0)),
            base_km_msl: 0.5,
            top_km_msl: 4.0,
            ..request(ODD)
        };
        let grid = build_voxels(&scan, &req, SITE.0, SITE.1).unwrap();
        assert_eq!(grid.lut().len(), LUT_LEN, "{}", product.name());
        assert_eq!(crate::fields::product_for(grid.field()), Some(product));
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

#[test]
fn the_table_is_the_palette_function_not_its_stops() {
    let zdr = get_legend_scale(RadarProduct::DifferentialReflectivity);
    assert!(
        zdr.thresholds.iter().all(|(v, _)| *v >= -2.0),
        "precondition: the ZDR stops start at −2 dB, so a table built \
             from them has no colour under it",
    );
    let range = value_range_for(MomentSlot::DifferentialReflectivity);
    let lut = colormap_lut(RadarProduct::DifferentialReflectivity, range);
    assert_eq!(&lut[4..8], &[66, 66, 66, 180], "ZDR's floor colour");

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
            "WO-M14a: the filter IS stored now — baked into the transfer \
             table at build time rather than matched on the product on \
             demand — so what this asserts is that the stored byte still \
             agrees with the scale it was baked from",
        );
    }
    assert_eq!(
        samplable(RadarProduct::HydrometeorClassification),
        None,
        "HHC is the scale where a blended step would be a wrong category, \
             and it is not a moment",
    );
    assert!(!get_legend_scale(RadarProduct::HydrometeorClassification).is_gradient);
}

// ── The boundary, which is the whole point of the encoding ──────────────

fn fetched_index(a: u8, b: u8, t: f64) -> f64 {
    f64::from(a) * (1.0 - t) + f64::from(b) * t
}

fn ramp_value_at(range: (f32, f32), index: f64) -> f64 {
    f64::from(range.0) + (f64::from(range.1) - f64::from(range.0)) * index / 255.0
}

fn alpha_at(lut: &[u8], index: f64) -> u8 {
    lut[(index.round() as usize).min(255) * 4 + 3]
}

#[test]
fn an_echo_edge_fades_instead_of_fabricating_a_mid_value() {
    let scan = scan_of(&|az, slant| {
        ((40.0..80.0).contains(&az) && (20.0..50.0).contains(&slant)).then_some(65.0)
    });
    let shape = VoxelShape {
        nx: 64,
        ny: 64,
        nz: 24,
    };
    let req = VoxelRequest {
        half_extent_km: Some(HalfExtentKm::square(60.0)),
        base_km_msl: 0.5,
        top_km_msl: 8.0,
        ..request(shape)
    };
    let grid = build_voxels(&scan, &req, SITE.0, SITE.1).unwrap();
    let range = grid.value_range();

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
    // The 65 dBZ core resamples to index 195 exactly, which is
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
    assert_eq!(faded_at, 43.0 / 64.0);

    // ── the rejected encoding: index 0 out of band ──
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
    assert_ne!(
        get_color_for_value(RadarProduct::Reflectivity, fabricated as f32).3,
        0,
        "and the alpha floor cannot rescue it: the floor applies to the \
             fetched index, and {fabricated} dBZ is a perfectly ordinary echo",
    );

    let ours_half = ramp_value_at(range, fetched_index(data, empty, 0.5));
    assert!(
        ours_half < fabricated - 10.0,
        "bottom-of-ramp must read materially weaker halfway across the \
             edge than the out-of-band encoding: {ours_half} dBZ against \
             {fabricated} dBZ",
    );

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

#[test]
fn the_half_edge_costs_of_both_encodings_are_measured_per_moment() {
    let rows: Vec<(&str, f64, f64)> = SLOTS
        .iter()
        .zip(SAMPLABLE)
        .map(|(&slot, product)| {
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
            ("ref", 16.25, 32.352),
            ("vel", -17.0, -3.119),
            ("sw", 1.875, 1.985),
            ("zdr", -3.219, -0.258),
            ("phi", 29.055, 29.203),
            ("rho", 0.588, 0.714),
        ],
        "half-edge fetch, shipped vs out-of-band, per moment",
    );

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
            ("ref", 64),
            ("vel", 0),
            ("sw", 9),
            ("zdr", 0),
            ("phi", 0),
            ("rho", 0),
            ("srv", 0),
            ("nrot", 0),
            ("kdp", 50),
        ],
        "the fade band per product",
    );

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

    // The two degenerate tables, built directly now that the band is measured
    // once when the table is: entry 0 is forced transparent, so an otherwise
    // opaque table has a band of zero, and an entirely transparent one fades
    // the whole ramp.
    let table_of = |lut: Vec<u8>| {
        transfer_table_over(
            lut,
            RadarProduct::Reflectivity,
            value_range_for(MomentSlot::Reflectivity),
        )
    };
    let mut opaque_lut = vec![255; LUT_LEN];
    opaque_lut[3] = 0;
    let opaque = table_of(opaque_lut);
    assert_eq!(opaque.fade_band(), 0);
    let clear = table_of(vec![0; LUT_LEN]);
    assert_eq!(clear.fade_band(), u8::MAX);
}

#[test]
fn the_default_transparency_profile_is_measured_per_product() {
    let alpha = |product: RadarProduct, value: f32| {
        let range = ramp_of(product);
        let lut = colormap_lut(product, range);
        lut[usize::from(ramp_index(range, value)) * 4 + 3]
    };
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

    assert_eq!(alpha(RadarProduct::SpectrumWidth, 1.0), 0, "laminar flow");
    solid(RadarProduct::SpectrumWidth, 10.0, "turbulence");

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
    let hail = alpha(zdr, p::ZDR_TUMBLING_DB);
    assert!(
        hail >= palette_alpha(zdr, p::ZDR_TUMBLING_DB) / 3,
        "tumbling hail at 0 dB renders at {hail} of {}: a hole where the \
             HCA's own bounds (HSDA_MAX_ZDR = {}) put the signature",
        palette_alpha(zdr, p::ZDR_TUMBLING_DB),
        crate::hca::HSDA_MAX_ZDR,
    );
    // Measured over four volumes: ZDR in [−0.5, +0.5] is 68 % of every data
    // voxel in the box, so the low side plateaus rather than ramping to full.
    assert_eq!(
        p::ZDR_TUMBLING_ALPHA,
        p::PHI_ALPHA,
        "the plateau is the translucency this module already argues for a \
             moment with no honest background band",
    );
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

    let nrot = RadarProduct::NormalizedRotation;
    assert_eq!(
        p::NROT_CLEAR,
        crate::nrot::SIGNIFICANT as f32,
        "the volume must go visible exactly where the algorithm calls a \
             bin painted and the palette gives it its first colour",
    );
    assert_eq!(alpha(nrot, 0.0), 0, "no rotation");
    assert_eq!(alpha(nrot, 0.2), 0, "under the significance floor");
    // Measured: 8 033 of the 8 039 voxels a real tornado-warned volume
    // painted came back at alpha 2–4 of 180, six of them visible.
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

    // Every palette's plan-view maximum alpha is 180 — the radar layer's own
    // translucency convention — and ΦDP's 63 is that ceiling times its flat
    // 0.35, which puts its whole 255-entry ramp under the see-through bar.
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
            ("zdr", 42, 180),
            ("phi", 255, 63),
            ("rho", 35, 180),
            ("srv", 41, 180),
            ("nrot", 21, 180),
            ("kdp", 60, 180),
        ],
        "see-through data entries and max data alpha, per product",
    );
}

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

    let refl = grid(RadarProduct::Reflectivity);
    let (centre, threshold) = refl.iso_uniform_params(18.0);
    assert!(centre < 0.0, "a sequential product has no diverging centre");
    assert_eq!(
        threshold,
        f32::from(refl.value_to_index(18.0)) / 255.0,
        "the surface sits exactly where the ramp puts 18 dBZ",
    );

    let vel = grid(RadarProduct::Velocity);
    let (centre, threshold) = vel.iso_uniform_params(20.0);
    let c = vel.value_to_index(0.0);
    assert_eq!(centre, f32::from(c) / 255.0, "centred on calm air");
    assert_eq!(
        threshold,
        f32::from(vel.value_to_index(20.0) - c) / 255.0,
        "the crossing distance is 20 m/s of ramp",
    );
    assert_eq!(vel.iso_uniform_params(-20.0), (centre, threshold));

    let rho = grid(RadarProduct::CorrelationCoefficient);
    let (centre, threshold) = rho.iso_uniform_params(0.90);
    assert_eq!(centre, 1.0, "centred on the ramp top");
    assert_eq!(
        threshold,
        f32::from(255 - rho.value_to_index(0.90)) / 255.0,
        "the crossing distance reaches down to the bound",
    );

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
    let default_db = default_iso_threshold(RadarProduct::DifferentialReflectivity);
    assert_eq!(default_db, volume_alpha_profile::ZDR_COLUMN_DB - centre_db);
    assert_eq!(
        (centre_db + default_db, centre_db - default_db),
        (3.0, -2.5),
        "the default ZDR surface's positive and negative lobes",
    );

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
        (threshold - 0.1).abs() < 2.0 / 255.0,
        "|NROT| = 1 is a tenth of a ±5 ramp; {threshold} says the \
             surface was translated through velocity's ±63.5 span",
    );
    let srv = grid(RadarProduct::StormRelativeVelocity);
    assert_eq!(srv.value_range(), vel.value_range());
    assert_eq!(srv.iso_uniform_params(20.0), vel.iso_uniform_params(20.0));
    let kdp = grid(RadarProduct::SpecificDifferentialPhase);
    let (centre, threshold) = kdp.iso_uniform_params(1.5);
    assert!(centre < 0.0, "KDP is sequential");
    assert_eq!(threshold, f32::from(kdp.value_to_index(1.5)) / 255.0);

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
        assert_eq!(
            g.iso_uniform_params(f32::NAN),
            (centre, threshold),
            "{}: a NaN threshold must fall back, not poison the uniform",
            product.name(),
        );
    }

    let (_, fallback) = refl.iso_uniform_params(f32::NAN);
    assert_eq!(
        fallback,
        f32::from(refl.value_to_index(default_iso_threshold(RadarProduct::Reflectivity))) / 255.0,
    );
}

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
        half_extent_km: Some(HalfExtentKm::square(20.0)),
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

    // The seam: 1° and 359° are 2° apart on the circle, so a fetch filtered
    // halfway between their indices reads 180°, the worst error there is.
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

const HEADER_BYTES: usize = 4 + 2 + 2 + 3 * 4 + 3 * 16 + 16 + 8 + 4 + 8;
const SHAPE_AT: usize = 4 + 2 + 2;
const LUT_LEN_AT: usize = HEADER_BYTES;
const INDEX_LEN_AT: usize = LUT_LEN_AT + 4 + LUT_LEN;

fn value_len_at(cells: usize) -> usize {
    INDEX_LEN_AT + 4 + cells
}

fn prefix_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes"))
}

/// [`to_bytes`] for a grid whose field this build certainly registers — every
/// fixture in this file is built through `build_voxels` or from a registered
/// product, so the refusal arm is unreachable here and has its own test.
fn encoded(grid: &VolumeGrid) -> Vec<u8> {
    to_bytes(grid).expect("a registered field has a wire code")
}

fn wire_fixture() -> VolumeGrid {
    let scan = scan_of(&|az, slant| (az < 120.0 && slant < 90.0).then_some(48.0));
    build_voxels(&scan, &request(ODD), SITE.0, SITE.1).expect("the fixture grid builds")
}

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
    for zeroed in [
        VoxelShape { nx: 0, ..smallest },
        VoxelShape { ny: 0, ..smallest },
        VoxelShape { nz: 0, ..smallest },
    ] {
        assert!(!zeroed.is_supported(), "{zeroed:?}");
        assert_eq!(zeroed.cells(), 0);
    }
}

#[test]
fn the_length_prefixes_are_where_the_tests_think_they_are() {
    let grid = wire_fixture();
    let bytes = encoded(&grid);
    let cells = ODD.cells();
    assert_eq!(prefix_at(&bytes, SHAPE_AT), ODD.nx as u32);
    assert_eq!(prefix_at(&bytes, SHAPE_AT + 4), ODD.ny as u32);
    assert_eq!(prefix_at(&bytes, SHAPE_AT + 8), ODD.nz as u32);
    assert_eq!(prefix_at(&bytes, LUT_LEN_AT), LUT_LEN as u32);
    assert_eq!(prefix_at(&bytes, INDEX_LEN_AT), cells as u32);
    assert_eq!(prefix_at(&bytes, value_len_at(cells)), cells as u32);
    assert_eq!(bytes.len(), value_len_at(cells) + 4 + cells * 4);
}

#[test]
fn the_format_version_is_the_one_this_layout_ships() {
    assert_eq!(FORMAT_VERSION, 1);
    let bytes = encoded(&wire_fixture());
    assert_eq!(&bytes[..4], b"RDVX", "the magic moved");
    assert_eq!(
        u16::from_le_bytes([bytes[4], bytes[5]]),
        1,
        "the version is not where a decoder from another build looks for it",
    );
    let grid = wire_fixture();
    assert_eq!(
        grid.indices().len(),
        grid.dims().cells(),
        "the payload carries more than one byte per cell — a coverage plane \
         on the wire is a layout change and must bump FORMAT_VERSION",
    );
}

fn layout_fixture() -> VolumeGrid {
    VolumeGrid::from_parts(VolumeParts {
        indices: vec![0, 1, 128, NO_DATA_INDEX, 255, 7],
        values: Some(vec![0.0, -1.5, 2.25, f32::NAN, 0.5, -0.75]),
        dims: VoxelShape {
            nx: 3,
            ny: 2,
            nz: 1,
        },
        x_range_km: (-1.5, 2.5),
        y_range_km: (-3.25, 4.75),
        z_range_km_msl: (0.5, 8.5),
        anchor: (35.25, -97.5),
        field: crate::fields::known::REFLECTIVITY,
        transfer: transfer_table_over(
            (0..LUT_LEN).map(|i| (i % 251) as u8).collect(),
            RadarProduct::Reflectivity,
            (-32.0, 96.0),
        ),
        levels: 5,
        widest_level_gap_deg: 1.25,
    })
}

/// A grid carrying a field this build does not register has **no wire form**.
///
/// The FieldId <-> wire-code map is this crate's private table, so a payload
/// it cannot name has nothing to write at byte 6 — and writing anything else
/// there would decode as a different moment on the far end.
#[test]
fn a_grid_whose_field_this_build_cannot_name_has_no_wire_form() {
    let good = wire_fixture();
    assert!(
        to_bytes(&good).is_some(),
        "precondition: a registered field must encode, or the refusal below \
         passes for the wrong reason",
    );

    let same_but_alien = |field: crate::fields::Id| {
        VolumeGrid::from_parts(VolumeParts {
            indices: good.indices().to_vec(),
            values: good.values().map(<[f32]>::to_vec),
            dims: good.dims(),
            anchor: good.anchor(),
            x_range_km: good.x_range_km(),
            y_range_km: good.y_range_km(),
            z_range_km_msl: good.z_range_km_msl(),
            field,
            transfer: good.transfer().clone(),
            levels: good.levels(),
            widest_level_gap_deg: good.widest_level_gap_deg(),
        })
    };

    // The control: everything but the field, and it still encodes.
    assert_eq!(
        to_bytes(&same_but_alien(good.field().clone())),
        to_bytes(&good),
        "rebuilding the fixture through its own accessors must not change it",
    );
    assert!(
        to_bytes(&same_but_alien(crate::fields::Id::new(
            "NotAMomentThisBuildHas"
        )))
        .is_none(),
        "a field with no wire code must have no payload",
    );
}

#[test]
fn the_wire_layout_is_the_one_this_version_ships() {
    let bytes = encoded(&layout_fixture());
    assert_eq!(
        (
            FORMAT_VERSION,
            bytes.len(),
            crate::wire::layout_digest(&bytes)
        ),
        (1, 1170, 0x794c_df89_7bd1_0a4d),
        "the bytes `to_bytes` writes are not the bytes version 1 shipped. \
         Something about this payload's layout moved — a field added, \
         removed, reordered, retyped, or written at a different width. That \
         is the change `FORMAT_VERSION` exists to announce, and a stale \
         worker that shares a build token with a fresh page (locally it \
         always does: `GITHUB_SHA` is absent outside CI, so the token \
         degrades to `…/dev`) will decode the new bytes into the old field \
         order and raymarch a volume with its axes swapped, with no error \
         anywhere. Bump `FORMAT_VERSION`, then write the new length and \
         digest here — in that order, and never the numbers alone.",
    );
}

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
                assert!(
                    grid.values().unwrap().iter().any(|v| v.is_nan()),
                    "{what}: the value plane has no NaN in it",
                );
                assert!(
                    grid.values().unwrap().iter().any(|v| v.is_finite()),
                    "{what}: the value plane has no numbers in it",
                );
            }

            let decoded =
                from_bytes(&encoded(&grid)).unwrap_or_else(|| panic!("{what} did not decode"));
            assert_eq!(grid, decoded, "{what} changed in transit");
            assert_eq!(
                decoded.values().is_some(),
                values_wanted,
                "{what}: the value plane's presence did not survive",
            );
            assert_eq!(
                crate::fields::product_for(decoded.field()),
                Some(product),
                "{what}"
            );
            assert_eq!(decoded.dims(), ODD, "{what}");
            assert_eq!(decoded.lut(), grid.lut(), "{what}");
            assert_eq!(decoded.levels(), grid.levels(), "{what}");
            assert_eq!(encoded(&grid), encoded(&decoded), "{what}");
        }
    }

    let a = build_voxels(&scan, &request(ODD), SITE.0, SITE.1).unwrap();
    let elsewhere = build_voxels(
        &scan,
        &VoxelRequest {
            half_extent_km: Some(HalfExtentKm::square(61.0)),
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
            from_bytes(&encoded(&a)).unwrap(),
            from_bytes(&encoded(other)).unwrap(),
            "{name} decoded to the same grid",
        );
    }
}

#[test]
fn the_encoded_length_of_a_grid_is_exact() {
    let scan = six_moment_scan();
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
                encoded_len(&grid),
                encoded(&grid).len(),
                "{shape:?} values={values_wanted}",
            );
        }
    }
}

#[test]
fn a_grid_header_that_cannot_describe_its_own_product_is_refused() {
    let good = encoded(&wire_fixture());
    assert!(
        from_bytes(&good).is_some(),
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
            assert!(from_bytes(&bad).is_none(), "{name} = {what} decoded",);
        }
    }
    let mut moved = good.clone();
    moved[20..28].copy_from_slice(&(-999.0f64).to_le_bytes());
    assert!(
        from_bytes(&moved).is_some_and(|g| g.x_range_km().0 == -999.0),
        "offset 20 is not x_range.0, so the finiteness assertions above are \
             corrupting some other field into invalidity",
    );

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
        from_bytes(&ramp).is_none(),
        "a value range this product's quantisation never produces decoded, \
             carrying a colour table built to agree with it — so `index_to_value` \
             would have read every index off the wrong scale",
    );

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
        from_bytes(&swapped).is_none(),
        "a colour table built for another product decoded, and the \
             raymarch would have painted it",
    );
}

#[test]
fn a_malformed_grid_payload_is_refused_rather_than_misread() {
    let grid = wire_fixture();
    let good = encoded(&grid);
    let cells = ODD.cells();
    let values_prefix_at = value_len_at(cells);

    assert!(from_bytes(&[]).is_none(), "empty");
    assert!(from_bytes(b"nope").is_none(), "wrong magic");

    for wrong in [*b"nope", *b"RDRI", *b"RDXS"] {
        let mut relabelled = good.clone();
        relabelled[..4].copy_from_slice(&wrong);
        assert!(
            from_bytes(&relabelled).is_none(),
            "a whole payload labelled {} decoded as a grid",
            String::from_utf8_lossy(&wrong),
        );
    }

    let mut wrong_version = good.clone();
    wrong_version[4] = 0xFF;
    wrong_version[5] = 0xFF;
    assert!(
        from_bytes(&wrong_version).is_none(),
        "an unknown version decoded",
    );

    let mut unknown_product = good.clone();
    unknown_product[6..8].copy_from_slice(&0xFFFEu16.to_le_bytes());
    assert!(
        from_bytes(&unknown_product).is_none(),
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
        from_bytes(&underivable).is_none(),
        "a product with no native moment decoded",
    );

    for axis in 0..3 {
        for bad in [0u32, (MAX_AXIS + 1) as u32, u32::MAX] {
            let mut broken = good.clone();
            broken[SHAPE_AT + axis * 4..SHAPE_AT + axis * 4 + 4]
                .copy_from_slice(&bad.to_le_bytes());
            assert!(
                from_bytes(&broken).is_none(),
                "axis {axis} of {bad} decoded",
            );
        }
    }
    let mut reshaped = good.clone();
    reshaped[SHAPE_AT..SHAPE_AT + 4].copy_from_slice(&((ODD.nx + 1) as u32).to_le_bytes());
    assert!(
        from_bytes(&reshaped).is_none(),
        "a shape claiming more cells than the index plane holds decoded — \
             every accessor indexes that plane with an offset from the shape",
    );

    for axis in 0..3 {
        let mut empty = good[..INDEX_LEN_AT].to_vec();
        empty[SHAPE_AT + axis * 4..SHAPE_AT + axis * 4 + 4].copy_from_slice(&0u32.to_le_bytes());
        empty.extend_from_slice(&0u32.to_le_bytes());
        empty.extend_from_slice(&0u32.to_le_bytes());
        assert!(
            from_bytes(&empty).is_none(),
            "axis {axis} of zero decoded into a grid with no cells",
        );
    }

    let tall = VoxelShape {
        nx: MAX_AXIS,
        ny: 1,
        nz: 1,
    };
    let over_shape = build_voxels(&scan_of(&|_, _| Some(40.0)), &request(tall), SITE.0, SITE.1)
        .expect("a shape at the guarantee builds");
    let mut over = encoded(&over_shape);
    let tall_cells = tall.cells();
    over[SHAPE_AT..SHAPE_AT + 4].copy_from_slice(&((MAX_AXIS + 1) as u32).to_le_bytes());
    over[INDEX_LEN_AT..INDEX_LEN_AT + 4].copy_from_slice(&((tall_cells + 1) as u32).to_le_bytes());
    over.insert(INDEX_LEN_AT + 4 + tall_cells, NO_DATA_INDEX);
    let moved = value_len_at(tall_cells) + 1;
    over[moved..moved + 4].copy_from_slice(&((tall_cells + 1) as u32).to_le_bytes());
    over.extend_from_slice(&f32::NAN.to_le_bytes());
    assert!(
        from_bytes(&over).is_none(),
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
            from_bytes(&good[..cut]).is_none(),
            "truncated to {cut} bytes",
        );
    }

    let mut trailing = good.clone();
    trailing.push(0);
    assert!(
        from_bytes(&trailing).is_none(),
        "trailing bytes mean the layouts disagree",
    );

    for (name, at) in [
        ("table", LUT_LEN_AT),
        ("index", INDEX_LEN_AT),
        ("value", values_prefix_at),
    ] {
        let mut absurd = good.clone();
        absurd[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(
            from_bytes(&absurd).is_none(),
            "an absurd {name} plane length reached a read",
        );
    }

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
            from_bytes(&short).is_none(),
            "a {name} plane one element short decoded",
        );
    }

    let mut one_value = good.clone();
    one_value.truncate(values_prefix_at + 4 + 4);
    one_value[values_prefix_at..values_prefix_at + 4].copy_from_slice(&1u32.to_le_bytes());
    assert!(
        from_bytes(&one_value).is_none(),
        "a one-element value plane decoded",
    );

    let mut absent = good.clone();
    absent.truncate(values_prefix_at + 4);
    absent[values_prefix_at..values_prefix_at + 4].copy_from_slice(&0u32.to_le_bytes());
    let decoded =
        from_bytes(&absent).expect("a grid with no value plane is a grid, not a malformed one");
    assert_eq!(decoded.values(), None);
    assert_eq!(decoded.indices(), grid.indices());

    assert_eq!(
        from_bytes(&good).expect("the unmutated payload decodes"),
        grid,
    );
    assert!(
        from_bytes(&encoded(&over_shape)).is_some(),
        "precondition: the shape-at-the-guarantee payload does not decode \
             unmutated either, so the assertion about it says nothing",
    );
}

#[test]
fn the_capacity_guard_refuses_a_length_the_buffer_cannot_hold() {
    let bytes = [0u8; 16];
    let r = Reader::new(&bytes);
    assert_eq!(r.bounded(4, 4), Some(4), "16 bytes hold four f32");
    assert_eq!(r.bounded(0, 4), Some(0));
    assert_eq!(r.bounded(5, 4), None, "20 bytes claimed from 16");
    assert_eq!(r.bounded(u32::MAX, 4), None, "16 GiB claimed from 16 bytes");

    let mut part_way = Reader::new(&bytes);
    part_way.take(8).expect("half the buffer");
    assert_eq!(part_way.bounded(2, 4), Some(2));
    assert_eq!(part_way.bounded(3, 4), None);

    assert_eq!(Reader::new(&bytes).bounded(u32::MAX, usize::MAX), None);
}

#[test]
fn coverage_is_exactly_whether_the_index_is_the_no_data_one() {
    for product in RadarProduct::all() {
        let Some(slot) = crate::derive::volume_slot(*product) else {
            continue;
        };
        let range = value_range_for_product(*product, slot);
        for absent in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(
                ramp_index(range, absent),
                NO_DATA_INDEX,
                "{}: {absent} does not encode as no-data, so coverage 0 would \
                 lose a cell the grid says is empty",
                product.code(),
            );
        }
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

#[cfg(not(target_arch = "wasm32"))]
fn serial_reference_grid(
    scan: &Scan,
    req: &VoxelRequest,
    lat: f64,
    lon: f64,
) -> (Vec<u8>, Vec<f32>) {
    let slot = crate::derive::volume_slot(req.product).expect("a native product");
    let sampler =
        VolumeSampler::new(crate::nyquist::Volume::from(scan), req.product).expect("a sampler");

    let half = match req.half_extent_km {
        Some(picked) => picked.clamped(),
        None => HalfExtentKm::square(box_half_width_km(volume_reach_km(scan, req.product))),
    };
    let (bearing_deg, range_km) =
        rustdar_geo::site_bearing_range_km(lat, lon, req.centre.0, req.centre.1);
    let bearing = bearing_deg.to_radians();
    let (cx, cy) = (range_km * bearing.sin(), range_km * bearing.cos());
    let x_range_km = (cx - half.east_km, cx + half.east_km);
    let y_range_km = (cy - half.north_km, cy + half.north_km);
    let z_range_km_msl = (req.base_km_msl, req.top_km_msl);
    let site_km_msl = crate::eet::radar_height_ft_near(lat, lon, crate::sites::Datum::Feedhorn)
        .unwrap_or(0.0)
        * 0.0003048;
    let value_range = value_range_for_product(req.product, slot);

    let (nx, ny, nz) = (req.shape.nx, req.shape.ny, req.shape.nz);
    let cells = req.shape.cells();
    let mut indices = vec![NO_DATA_INDEX; cells];
    let mut values = vec![f32::NAN; cells];
    let heights_km: Vec<f64> = (0..nz)
        .map(|iz| axis_centre(z_range_km_msl, nz, iz) - site_km_msl)
        .collect();

    let plane = ny * nx;
    let mut column = Column::new();
    for iy in 0..ny {
        let y_km = axis_centre(y_range_km, ny, iy);
        for ix in 0..nx {
            let x_km = axis_centre(x_range_km, nx, ix);
            let ground_range_km = x_km.hypot(y_km);
            let azimuth_deg = x_km.atan2(y_km).to_degrees().rem_euclid(360.0);
            sampler.column_into(azimuth_deg, ground_range_km, &mut column);
            for (iz, &height_km) in heights_km.iter().enumerate() {
                let Some(value) = column
                    .at_height_km(height_km)
                    .value()
                    .filter(|v| v.is_finite())
                else {
                    continue;
                };
                let offset = iz * plane + iy * nx + ix;
                indices[offset] = ramp_index(value_range, value);
                values[offset] = value;
            }
        }
    }
    (indices, values)
}

// Named rather than gating the whole module: a module-wide gate would stop
// type-checking the other ~600 tests against wasm32, the arm that compiles
// `par.rs`'s sequential stand-ins.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn the_rows_build_the_grid_the_one_buffer_serial_loop_built() {
    assert!(
        rayon::current_num_threads() > 1,
        "single-threaded pool: this test cannot observe a race"
    );
    let scan = six_moment_scan();
    let shapes = [
        ODD,
        VoxelShape {
            nx: 37,
            ny: 41,
            nz: 23,
        },
    ];
    for shape in shapes {
        for product in [RadarProduct::Reflectivity, RadarProduct::Velocity] {
            let req = VoxelRequest {
                product,
                ..request(shape)
            };
            let at = format!(
                "{} at {}x{}x{}",
                product.code(),
                shape.nx,
                shape.ny,
                shape.nz
            );
            let build = || build_voxels(&scan, &req, SITE.0, SITE.1).expect("a grid");
            let grid = build();

            let filled = grid
                .indices()
                .iter()
                .filter(|&&i| i != NO_DATA_INDEX)
                .count();
            assert!(
                filled > shape.cells() / 10,
                "{at}: only {filled} of {} cells carry data; the fixture has \
                 stopped filling rows and this proves nothing",
                shape.cells(),
            );

            let (indices, values) = serial_reference_grid(&scan, &req, SITE.0, SITE.1);
            for (cell, (&got, &want)) in grid.indices().iter().zip(&indices).enumerate() {
                assert_eq!(
                    got, want,
                    "{at}: index cell {cell} is {got}, the serial loop's is {want}",
                );
            }
            let built = grid.values().expect("the value plane was asked for");
            for (cell, (&got, &want)) in built.iter().zip(&values).enumerate() {
                assert_eq!(
                    got.to_bits(),
                    want.to_bits(),
                    "{at}: value cell {cell} is {got}, the serial loop's is {want}",
                );
            }

            for run in 1..4 {
                assert_eq!(build(), grid, "{at}: run {run} differs from run 0");
            }
        }
    }
}

// ── The request the seam shapes (WO-M14b-2) ─────────────────────────────

/// A context aimed at `centre` with `extent`, on a device that affords
/// `cells` at `max_axis`. The payload is never read by `request_for`, so a
/// stand-in states that rather than pretending a volume is involved.
fn ctx_for(
    field: rustdar_source::product::FieldId,
    centre: rustdar_geo::GeoPoint,
    half_extent_km: Option<(f64, f64)>,
    cells: [u32; 3],
    max_axis: u32,
) -> rustdar_source::volume::VolumeJobContext {
    rustdar_source::volume::VolumeJobContext {
        payload: Box::new(()),
        field,
        centre,
        half_extent_km,
        cells,
        max_axis,
    }
}

/// **The two horizontal axes of a picked reach do not swap.**
///
/// The reach crosses the seam as a bare pair — the substrate cannot name
/// `HalfExtentKm` — and this side puts the names back on. **Every other
/// fixture in this workspace picks a SQUARE region**, where a transposition
/// is invisible; this one is deliberately asymmetric, and it is the only
/// thing standing between an east/north swap and a box that is resampled
/// sideways.
#[test]
fn the_reach_that_crosses_the_seam_keeps_east_east_and_north_north() {
    let request = request_for(&ctx_for(
        crate::fields::known::REFLECTIVITY,
        rustdar_geo::GeoPoint {
            lat: 36.1,
            lon: -98.4,
        },
        Some((12.5, 47.5)),
        [128, 128, 32],
        2048,
    ))
    .expect("Reflectivity is a field this build registers");
    assert_eq!(
        request.half_extent_km,
        Some(HalfExtentKm {
            east_km: 12.5,
            north_km: 47.5,
        }),
        "the pair is read (east, north); a swap would give (47.5, 12.5)",
    );
    assert_eq!(
        request.centre,
        (36.1, -98.4),
        "the centre is read (lat, lon)",
    );
}

/// **The vertical extent and the value plane are this side's answer, not the
/// caller's.** Neither is on the context at all: a caller that wanted a
/// different box top or a second buffer of values in their own units would
/// have to say so here, where the reason can be written down.
#[test]
fn the_box_top_and_the_value_plane_are_decided_on_this_side_of_the_seam() {
    for extent in [None, Some((30.0, 30.0))] {
        let request = request_for(&ctx_for(
            crate::fields::known::VELOCITY,
            rustdar_geo::GeoPoint {
                lat: 35.0,
                lon: -97.0,
            },
            extent,
            [128, 128, 32],
            2048,
        ))
        .expect("Velocity is a field this build registers");
        assert_eq!(request.base_km_msl, DEFAULT_BASE_KM_MSL);
        assert_eq!(request.top_km_msl, DEFAULT_TOP_KM_MSL);
        assert!(
            !request.values_wanted,
            "the 3D view paints palette indices through a transfer table; a \
             second plane of values in their own units is a buffer nothing up \
             there reads",
        );
    }
}

/// **A field this build registers no product for shapes no request.**
///
/// The id crosses the seam as an open string. The frontend matched it against
/// this layer's own rows before asking — but the ask and the answer are
/// separated by an action channel, so this side refuses rather than resolving
/// an unknown id to whatever the first row happens to be.
#[test]
fn an_unregistered_field_shapes_no_request() {
    let alien = rustdar_source::product::FieldId::from_static("NotAFieldThisBuildHas");
    assert!(
        crate::fields::product_for(&alien).is_none(),
        "precondition: the id must really be unregistered, or this test \
         passes for the wrong reason",
    );
    assert!(
        request_for(&ctx_for(
            alien,
            rustdar_geo::GeoPoint {
                lat: 35.0,
                lon: -97.0,
            },
            None,
            [128, 128, 32],
            2048,
        ))
        .is_none(),
    );
    // Control on the same walk: a registered id through the identical call
    // does shape one, so the `None` above is about the field and not about
    // the fixture.
    assert!(
        request_for(&ctx_for(
            crate::fields::known::REFLECTIVITY,
            rustdar_geo::GeoPoint {
                lat: 35.0,
                lon: -97.0,
            },
            None,
            [128, 128, 32],
            2048,
        ))
        .is_some(),
    );
}

/// **The device's ceiling is spent here, and it binds.**
///
/// `cells` is a budget and `max_axis` is a hard limit; the frontend hands over
/// both and never resolves them into a shape, because how a radar volume is
/// best sampled — wide and shallow — is this side's arithmetic.
#[test]
fn the_budget_is_spent_against_the_axis_the_device_reported() {
    let shape_at = |max_axis: u32| {
        request_for(&ctx_for(
            crate::fields::known::REFLECTIVITY,
            rustdar_geo::GeoPoint {
                lat: 35.0,
                lon: -97.0,
            },
            None,
            [256, 256, 64],
            max_axis,
        ))
        .expect("Reflectivity is a field this build registers")
        .shape
    };
    let small = shape_at(64);
    for (axis, n) in [("nx", small.nx), ("ny", small.ny), ("nz", small.nz)] {
        assert!(
            n <= 64,
            "a device that reported 64 was asked for {n} cells of {axis}",
        );
    }
    assert!(
        shape_at(2048).cells() > small.cells(),
        "the ceiling has to actually bind, or the assertion above holds over \
         a shape the ceiling never touched",
    );
}
