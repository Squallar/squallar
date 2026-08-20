use super::*;
use nexrad_level3::model::{Level3Message, ProductDescriptionBlock, RadialPacket, RadialRun};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

const LAT: f64 = 35.3333;
const LON: f64 = -97.2778;
const SCALE: f32 = 2.0;
const OFFSET: f32 = 66.0;
const PRODUCT: types::RadarProduct = types::RadarProduct::Reflectivity;
const N_RADIALS: usize = 360;
const N_BINS: usize = 600;

fn packet(silence: Option<usize>) -> RadialPacket {
    let radials = (0..N_RADIALS)
        .map(|i| {
            let az = (i as f64).to_radians();
            let (s, c) = az.sin_cos();
            let gate_values = (0..N_BINS)
                .map(|j| {
                    if silence == Some(i) {
                        return 0; // a gate value <= 1 is skipped
                    }
                    let r = j as f64 * 0.25;
                    let (x, y) = (r * s, r * c);
                    let core = |cx: f64, cy: f64, w: f64, amp: f64| {
                        let d2 = (x - cx).powi(2) + (y - cy).powi(2);
                        amp * (-d2 / (2.0 * w * w)).exp()
                    };
                    let dbz = 20.0
                        + core(40.0, 60.0, 18.0, 55.0)
                        + core(-70.0, -30.0, 25.0, 45.0)
                        + core(10.0, -90.0, 12.0, 60.0)
                        + 6.0 * (x / 30.0).sin() * (y / 30.0).cos();
                    ((dbz * SCALE as f64 + OFFSET as f64).round() as i64).clamp(2, 250) as u16
                })
                .collect();
            RadialRun {
                start_angle: i as f32,
                angle_delta: 1.0,
                gate_values,
            }
        })
        .collect();
    RadialPacket {
        first_range_bin: 0,
        num_range_bins: N_BINS as u16,
        i_center: 0,
        j_center: 0,
        scale_factor: 4.0,
        is_legacy: false,
        xdr_data_scale: None,
        xdr_data_offset: None,
        radials,
    }
}

fn render(p: &RadialPacket) -> (Vec<u8>, Vec<f32>) {
    let SweepRender { image, values, .. } =
        render_level3_radial_to_image(p, PRODUCT, LAT, LON, SCALE, OFFSET, None, types::IMAGE_SIZE)
            .unwrap();
    (image, values)
}

fn digest(image: &[u8], values: &[f32]) -> u64 {
    let mut h = DefaultHasher::new();
    image.hash(&mut h);
    for v in values {
        v.to_bits().hash(&mut h);
    }
    h.finish()
}

#[test]
fn fixture_covers_a_realistic_share_of_the_image() {
    let (image, values) = render(&packet(None));
    let painted = image.chunks_exact(4).filter(|px| px[3] != 0).count();
    // 600 bins of 0.25 km reach 150 km, and the frame is now 150 km too, so
    // the disc fills it corner to corner: pi/4 of the raster, against the
    // 21% of it the same packet covered while every short product was
    // drawn on a 230 km frame.
    let reach_km = N_BINS as f64 * 0.25;
    let px_per_km = types::IMAGE_SIZE as f64 / (2.0 * reach_km);
    let disc = std::f64::consts::PI * (reach_km * px_per_km).powi(2);
    assert!(
        (painted as f64) > disc * 0.9 && (painted as f64) < disc * 1.1,
        "painted {painted}, expected about {disc:.0} for a {N_BINS}-gate disc"
    );
    assert!(values.iter().any(|v| !v.is_nan()));
}

#[test]
fn values_round_trip_through_the_cell_unaltered() {
    let (_, values) = render(&packet(None));
    for &v in values.iter().filter(|v| !v.is_nan()) {
        let gate = v * SCALE + OFFSET;
        assert!(
            gate.fract() == 0.0 && (2.0..=250.0).contains(&gate),
            "value {v} is not (gate - {OFFSET}) / {SCALE} for any gate the fixture wrote"
        );
    }
}

/// Pins the *direction* of the tie-break, not just its stability.
fn assert_later_radial_wins(
    both: &[f32],
    only_first: &[f32],
    only_second: &[f32],
    first_value: f32,
) {
    let contested = only_first
        .iter()
        .zip(only_second)
        .filter(|(a, b)| !a.is_nan() && !b.is_nan())
        .count();
    assert!(
        contested > 20,
        "only {contested} pixels are reached by both radials; this fixture cannot \
             observe the tie-break"
    );

    let stolen = both
        .iter()
        .zip(only_second)
        .filter(|(got, second)| !second.is_nan() && **got == first_value)
        .count();
    assert_eq!(
        stolen, 0,
        "{stolen} of {contested} contested pixels kept radial 0's value even though \
             radial 1 reached them; the later radial is no longer winning"
    );
}

#[test]
fn level3_later_radial_wins_a_contested_pixel() {
    fn two_radials(first: u16, second: u16) -> RadialPacket {
        let run = |start: f32, gate: u16| RadialRun {
            start_angle: start,
            angle_delta: 1.0,
            gate_values: vec![gate; N_BINS],
        };
        RadialPacket {
            first_range_bin: 0,
            num_range_bins: N_BINS as u16,
            i_center: 0,
            j_center: 0,
            scale_factor: 4.0,
            is_legacy: false,
            xdr_data_scale: None,
            xdr_data_offset: None,
            radials: vec![run(90.0, first), run(91.0, second)],
        }
    }
    let grid = |first, second| render(&two_radials(first, second)).1;
    assert_later_radial_wins(
        &grid(200, 100),
        &grid(200, 0),
        &grid(0, 100),
        (200.0 - OFFSET) / SCALE,
    );
}

#[test]
fn overlapping_radials_contend_for_pixels() {
    let (_, full) = render(&packet(None));
    let (_, cut) = render(&packet(Some(N_RADIALS / 2)));

    let contested = full
        .iter()
        .zip(&cut)
        .filter(|(a, b)| !a.is_nan() && !b.is_nan() && a.to_bits() != b.to_bits())
        .count();

    assert!(
        contested > 100,
        "only {contested} pixels contended; the fixture has stopped overlapping and \
             the determinism tests prove nothing"
    );
}

#[test]
fn parallel_render_is_deterministic() {
    assert!(
        rayon::current_num_threads() > 1,
        "single-threaded pool: this test cannot observe a race"
    );
    let p = packet(None);
    let first = {
        let (i, v) = render(&p);
        digest(&i, &v)
    };
    for run in 1..10 {
        let (i, v) = render(&p);
        assert_eq!(digest(&i, &v), first, "render {run} differs from render 0");
    }
}

#[test]
fn parallel_matches_single_thread() {
    let p = packet(None);
    let (i, v) = render(&p);
    let parallel = digest(&i, &v);

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    let sequential = pool.install(|| {
        let (i, v) = render(&p);
        digest(&i, &v)
    });

    assert_eq!(parallel, sequential);
}

#[test]
fn colour_agrees_with_value_at_every_pixel() {
    let (image, values) = render(&packet(None));
    for (idx, (px, &v)) in image.chunks_exact(4).zip(&values).enumerate() {
        let expected = if v.is_nan() {
            (0, 0, 0, 0)
        } else {
            get_color_for_value(PRODUCT, v)
        };
        assert_eq!(
            (px[0], px[1], px[2], px[3]),
            expected,
            "pixel {idx} holds a colour its value did not produce (value {v})"
        );
    }
}

// ── Level II and NROT ────────────────────────────────────────────────────

const L2_ELEVATION: f32 = 0.5;

fn l2_sweep(gates: &[u8], azimuths: &[f32], declared_spacing: f32, velocity: bool) -> Scan {
    use nexrad_model::data::{MomentData, PulseWidth, RadialStatus, Sweep, VolumeCoveragePattern};
    assert_eq!(gates.len(), azimuths.len(), "one gate byte per radial");
    let radials = gates
        .iter()
        .zip(azimuths)
        .enumerate()
        .map(|(i, (&byte, &azimuth))| {
            let moment = MomentData::from_fixed_point(
                L2_GATES as u16,
                0,
                (L2_GATE_KM * 1000.0) as u16,
                8,
                SCALE,
                OFFSET,
                vec![byte; L2_GATES],
            );
            let (refl, vel) = if velocity {
                (None, Some(moment))
            } else {
                (Some(moment), None)
            };
            Radial::new(
                0,
                i as u16,
                azimuth,
                declared_spacing,
                RadialStatus::IntermediateRadialData,
                1,
                L2_ELEVATION,
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

fn l2_scan(gates: &[u8], velocity: bool) -> Scan {
    let azimuths: Vec<f32> = (0..gates.len()).map(|i| 90.0 + i as f32).collect();
    l2_sweep(gates, &azimuths, 1.0, velocity)
}

fn render_l2(gates: &[u8], product: types::RadarProduct) -> (Vec<u8>, Vec<f32>) {
    let scan = l2_scan(gates, product != types::RadarProduct::Reflectivity);
    let SweepRender { image, values, .. } =
        render_radar_to_image(&scan, L2_ELEVATION, product, LAT, LON).unwrap();
    (image, values)
}

fn probe_at(extent_km: f64, side_px: usize, az_deg: f64, range_km: f64) -> usize {
    let bounds = types::ImageBounds::from_radar_site(LAT, LON, extent_km);
    let proj = MercatorProjection::from_bounds(LAT, &bounds, extent_km, side_px);
    let (sin_az, cos_az) = az_deg.to_radians().sin_cos();
    let (sin_d, cos_d) = (range_km / rustdar_geo::EARTH_RADIUS_KM).sin_cos();
    let (px, py) = proj.pixel_at(sin_az, cos_az, sin_d, cos_d);
    py as usize * side_px + px as usize
}

const L2_GATES: usize = 600;
/// See [`L2_GATES`]. 600 of these reach 150 km of beam.
const L2_GATE_KM: f64 = 0.25;

fn l2_ground_reach_km() -> f64 {
    crate::beam::ground_range_km(L2_GATES as f64 * L2_GATE_KM, f64::from(L2_ELEVATION))
}

/// The raw gate codes Level II reserves below the data range.
const RAW_BELOW_THRESHOLD: u8 = 0;
const RAW_RANGE_FOLDED: u8 = 1;

/// The RGBA a pixel holds.
fn pixel_at(image: &[u8], idx: usize) -> (u8, u8, u8, u8) {
    let px = &image[idx * 4..idx * 4 + 4];
    (px[0], px[1], px[2], px[3])
}

fn assert_probes(values: &[f32], painted: bool, probes: &[(f64, f64)], why: &str) {
    for &(az, range) in probes {
        let v = values[probe_at(l2_ground_reach_km(), types::IMAGE_SIZE, az, range)];
        assert_eq!(
            !v.is_nan(),
            painted,
            "({az}°, {range} km) is {} — {why}",
            if v.is_nan() { "unpainted" } else { "painted" },
        );
    }
}

#[test]
fn level2_later_radial_wins_a_contested_pixel() {
    let grid = |g: &[u8]| render_l2(g, PRODUCT).1;
    assert_later_radial_wins(
        &grid(&[200, 100]),
        &grid(&[200, 0]),
        &grid(&[0, 100]),
        (200.0 - OFFSET) / SCALE,
    );
}

#[test]
fn level2_colour_agrees_with_value_at_every_pixel() {
    let (image, values) = render_l2(&[200, 100, 180, 120], PRODUCT);
    assert!(
        values.iter().any(|v| !v.is_nan()),
        "level II fixture painted nothing"
    );
    for (px, &v) in image.chunks_exact(4).zip(&values) {
        let want = if v.is_nan() {
            (0, 0, 0, 0)
        } else {
            get_color_for_value(PRODUCT, v)
        };
        assert_eq!((px[0], px[1], px[2], px[3]), want);
    }
}

fn nrot_scan(n_radials: usize) -> Scan {
    nrot_sector(n_radials, 360.0 / n_radials as f32)
}

fn nrot_sector(n_radials: usize, step_deg: f32) -> Scan {
    use nexrad_model::data::{MomentData, PulseWidth, RadialStatus, Sweep, VolumeCoveragePattern};
    let radials = (0..n_radials)
        .map(|i| {
            let theta = i as f64 / n_radials as f64 * std::f64::consts::TAU;
            // Byte 129 is 0 m/s at scale 2 / offset 129; ±8 cycles of
            // ±35 m/s gives shear well past the 0.5 display threshold.
            let ms = 35.0 * (8.0 * theta).sin();
            let byte = (129.0 + ms * 2.0).round().clamp(2.0, 254.0) as u8;
            Radial::new(
                0,
                i as u16,
                i as f32 * step_deg,
                step_deg,
                RadialStatus::IntermediateRadialData,
                1,
                L2_ELEVATION,
                None,
                Some(MomentData::from_fixed_point(
                    400,
                    0,
                    250,
                    8,
                    2.0,
                    129.0,
                    vec![byte; 400],
                )),
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

#[test]
fn nrot_colour_comes_from_the_nrot_palette() {
    let scan = nrot_scan(360);
    let SweepRender { image, values, .. } = render_radar_to_image(
        &scan,
        L2_ELEVATION,
        types::RadarProduct::NormalizedRotation,
        LAT,
        LON,
    )
    .unwrap();

    let painted = image.chunks_exact(4).filter(|px| px[3] != 0).count();
    assert!(
        painted > 10_000,
        "NROT fixture painted only {painted} pixels"
    );

    for (px, &v) in image.chunks_exact(4).zip(&values) {
        let want = if v.is_nan() {
            (0, 0, 0, 0)
        } else {
            get_color_for_value(types::RadarProduct::NormalizedRotation, v)
        };
        assert_eq!((px[0], px[1], px[2], px[3]), want);
    }
}

#[test]
fn nrot_render_is_deterministic() {
    let scan = nrot_scan(360);
    let once = || {
        let SweepRender { image, values, .. } = render_radar_to_image(
            &scan,
            L2_ELEVATION,
            types::RadarProduct::NormalizedRotation,
            LAT,
            LON,
        )
        .unwrap();
        digest(&image, &values)
    };
    let first = once();
    for run in 1..6 {
        assert_eq!(once(), first, "NROT render {run} differs from render 0");
    }

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    assert_eq!(
        pool.install(once),
        first,
        "NROT parallel differs from sequential"
    );
}

// ── How wide a radial is painted ─────────────────────────────────────────

#[test]
fn a_sparse_sweep_leaves_holes_not_chord_triangles() {
    let scan = l2_sweep(&[200; 4], &[0.0, 90.0, 180.0, 270.0], 0.5, false);
    let SweepRender { values, .. } =
        render_radar_to_image(&scan, L2_ELEVATION, PRODUCT, LAT, LON).unwrap();

    assert_probes(
        &values,
        true,
        &[(0.0, 50.0), (90.0, 50.0), (180.0, 50.0), (270.0, 50.0)],
        "a radial paints where it looked",
    );
    assert_probes(
        &values,
        false,
        &[(30.0, 50.0), (60.0, 50.0), (120.0, 50.0), (300.0, 50.0)],
        "the radar never looked here and the display must not claim it did",
    );
}

#[test]
fn an_out_of_order_sweep_still_paints() {
    let azimuths: Vec<f32> = (0..360).map(|i| i as f32).collect();
    let gates: Vec<u8> = (0..360).map(|i| 2 + (i % 200) as u8).collect();

    let render = |ascending: bool| {
        let (az, g) = if ascending {
            (azimuths.clone(), gates.clone())
        } else {
            (
                azimuths.iter().rev().copied().collect(),
                gates.iter().rev().copied().collect(),
            )
        };
        let scan = l2_sweep(&g, &az, 1.0, false);
        render_radar_to_image(&scan, L2_ELEVATION, PRODUCT, LAT, LON)
            .unwrap()
            .values
    };

    let up = render(true);
    let down = render(false);

    let up_painted = up.iter().filter(|v| !v.is_nan()).count();
    let down_painted = down.iter().filter(|v| !v.is_nan()).count();
    assert_eq!(
        down_painted, up_painted,
        "a descending sweep painted {down_painted} px against the ascending {up_painted}",
    );

    let mask_differs = up
        .iter()
        .zip(&down)
        .filter(|(a, b)| a.is_nan() != b.is_nan())
        .count();
    assert_eq!(
        mask_differs, 0,
        "{mask_differs} pixels are painted in one collection order and not the other",
    );

    let value_differs = up
        .iter()
        .zip(&down)
        .filter(|(a, b)| !a.is_nan() && a.to_bits() != b.to_bits())
        .count();
    assert!(
        value_differs > 0,
        "no contested pixel changed hands, so this fixture cannot tell a mask \
         comparison from a value comparison",
    );
}

#[test]
fn declared_spacing_drives_the_wedge_width() {
    let azimuths: Vec<f32> = (0..180).map(|i| i as f32 * 2.0).collect();
    let render = |declared: f32| {
        let scan = l2_sweep(&[200; 180], &azimuths, declared, false);
        render_radar_to_image(&scan, L2_ELEVATION, PRODUCT, LAT, LON)
            .unwrap()
            .values
    };

    let midpoints = [(1.0, 50.0), (3.0, 50.0), (181.0, 50.0)];
    assert_probes(
        &render(2.0),
        true,
        &midpoints,
        "radials declaring the 2° they are apart by tile with no seam",
    );
    assert_probes(
        &render(0.5),
        false,
        &midpoints,
        "radials declaring 0.5° cover 0.5°, whatever their neighbours do",
    );
}

#[test]
fn a_lying_declaration_is_clamped_to_the_sweeps_median() {
    let azimuths: Vec<f32> = (0..=400).map(|i| i as f32 * 0.5).collect();
    let scan = l2_sweep(&vec![200; azimuths.len()], &azimuths, 45.0, false);
    let SweepRender { values, .. } =
        render_radar_to_image(&scan, L2_ELEVATION, PRODUCT, LAT, LON).unwrap();

    assert_probes(
        &values,
        true,
        &[(0.0, 50.0), (100.0, 50.0), (200.0, 50.0)],
        "the arc itself still paints",
    );
    assert_probes(
        &values,
        false,
        &[(205.0, 50.0), (250.0, 50.0), (355.0, 50.0)],
        "45° is not a resolution the RDA has and the sweep's own spacing says so",
    );
}

#[test]
fn a_sweep_with_no_declaration_is_capped_at_a_sane_wedge() {
    let scan = l2_sweep(&[200, 200], &[0.0, 10.0], 0.0, false);
    let SweepRender { values, .. } =
        render_radar_to_image(&scan, L2_ELEVATION, PRODUCT, LAT, LON).unwrap();

    assert_probes(
        &values,
        true,
        &[(0.0, 50.0), (10.0, 50.0)],
        "both radials paint",
    );
    assert_probes(
        &values,
        false,
        &[(4.0, 50.0), (90.0, 50.0), (270.0, 50.0)],
        "two radials are two samples, not a sweep",
    );
}

const SURVEILLANCE_BINS: usize = 1832;

/// How much further apart than it declares this sweep's antenna ran, degrees.
const WOBBLE_DEG: f32 = 0.09;

fn long_range_sweep(azimuths: &[f32], side_ceiling: usize) -> SweepRender {
    use nexrad_model::data::{MomentData, PulseWidth, RadialStatus, Sweep, VolumeCoveragePattern};
    let radials = azimuths
        .iter()
        .enumerate()
        .map(|(i, &azimuth)| {
            let moment = MomentData::from_fixed_point(
                SURVEILLANCE_BINS as u16,
                0,
                (L2_GATE_KM * 1000.0) as u16,
                8,
                SCALE,
                OFFSET,
                vec![200; SURVEILLANCE_BINS],
            );
            Radial::new(
                0,
                i as u16,
                azimuth,
                0.5,
                RadialStatus::IntermediateRadialData,
                1,
                L2_ELEVATION,
                Some(moment),
                None,
                None,
                None,
                None,
                None,
                None,
            )
        })
        .collect();
    let scan = Scan::new(
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
    );
    // `MotionInputs::default()` deliberately, not a supplied vector: `PRODUCT` here is
    // reflectivity, and storm motion reaches a fill through exactly one arm of this
    // function, storm-relative velocity.
    render_radar_to_image_full_sized(
        &scan,
        L2_ELEVATION,
        PRODUCT,
        LAT,
        LON,
        crate::srv::MotionInputs::default(),
        None,
        None,
        &crate::nyquist::DeclaredNyquist::empty(),
        side_ceiling,
    )
    .unwrap()
}

#[test]
fn wedges_meet_where_the_antenna_wobbled() {
    let azimuths: Vec<f32> = (0..720)
        .map(|i| i as f32 * 0.5 + if i % 2 == 1 { WOBBLE_DEG } else { 0.0 })
        .collect();
    let out = long_range_sweep(&azimuths, LONG_RANGE_SIDE);
    let side = (out.image.len() / 4).isqrt();
    let mut unpainted = 0;
    for i in (0..azimuths.len()).step_by(2) {
        // The middle of the wide gap — between radial `i` and radial `i + 1`,
        // which sit `0.5 + WOBBLE_DEG` apart.
        let az = f64::from(azimuths[i]) + f64::from(0.5 + WOBBLE_DEG) / 2.0;
        if out.values[probe_at(out.max_range_km, side, az, 450.0)].is_nan() {
            unpainted += 1;
        }
    }
    assert_eq!(
        unpainted, 0,
        "{unpainted} of 360 gaps between radials the antenna ran wide on are \
         unpainted at 450 km, in a sweep that is solid echo everywhere",
    );
}

#[test]
fn a_dropped_radial_still_leaves_its_gap() {
    let azimuths: Vec<f32> = (0..720)
        .filter(|i| i % 4 != 1)
        .map(|i| i as f32 * 0.5)
        .collect();
    let out = long_range_sweep(&azimuths, LONG_RANGE_SIDE);
    let side = (out.image.len() / 4).isqrt();
    let mut painted = 0;
    for i in (0..720).filter(|i| i % 4 == 1) {
        if !out.values[probe_at(out.max_range_km, side, i as f64 * 0.5, 450.0)].is_nan() {
            painted += 1;
        }
    }
    assert_eq!(
        painted, 0,
        "{painted} of 180 dropped radials were painted over by a survivor \
         fanning across to where they should have been",
    );
}

#[test]
fn a_solid_field_leaves_no_pixel_of_itself_unpainted() {
    const RADIALS: usize = 360;
    const BINS: usize = 460;
    let radials = (0..RADIALS)
        .map(|i| RadialRun {
            start_angle: i as f32,
            angle_delta: 1.0,
            gate_values: vec![160; BINS],
        })
        .collect();
    let packet = RadialPacket {
        first_range_bin: 0,
        num_range_bins: BINS as u16,
        i_center: 0,
        j_center: 0,
        scale_factor: 1.0,
        is_legacy: false,
        xdr_data_scale: None,
        xdr_data_offset: None,
        radials,
    };
    let out = render_level3_radial_to_image(
        &packet,
        PRODUCT,
        LAT,
        LON,
        SCALE,
        OFFSET,
        None,
        LONG_RANGE_SIDE,
    )
    .unwrap();
    let side = (out.image.len() / 4).isqrt();
    let painted = |x: usize, y: usize| !out.values[y * side + x].is_nan();
    let mut holes = Vec::new();
    for y in 1..side - 1 {
        for x in 1..side - 1 {
            // Surrounded on all eight sides: inside the echo rather than at
            // its rim, where a pixel the truncating cast put one over is the
            // raster quantizing and not the lattice missing.
            if !painted(x, y)
                && (y - 1..=y + 1)
                    .all(|ny| (x - 1..=x + 1).all(|nx| (nx, ny) == (x, y) || painted(nx, ny)))
            {
                holes.push((x, y));
            }
        }
    }
    assert!(
        holes.is_empty(),
        "{} pixels of a solid disc were left unpainted with every neighbour \
         painted; the first few are {:?}",
        holes.len(),
        &holes[..holes.len().min(5)],
    );
}

#[test]
fn a_half_width_reaches_a_neighbour_only_as_far_as_its_own_sample_goes() {
    let h = |az: &[f64], declared: f64, median: f64| {
        super::l2_wedge_half_widths_deg(az, &vec![declared; az.len()], median)
    };
    // The wobble: 0.53° apart on a 0.5° declaration, so the wedges meet rather
    // than leaving 0.03° of measured sky unpainted.
    let wobbled: Vec<f64> = (0..720)
        .map(|i| i as f64 * 0.5 + if i % 2 == 1 { 0.03 } else { 0.0 })
        .collect();
    let got = h(&wobbled, 0.5, 0.5);
    assert!(
        got.iter().all(|w| (*w - 0.265).abs() < 1e-9),
        "a 0.53° gap wants a 0.265° half-width, got {:?}",
        &got[..4],
    );
    // A dropped radial is not a neighbour to meet: 1.0° apart on a 0.5°
    // declaration is past `MAX_ADJACENT_GAP_STEPS` of what the sample stands
    // for, so both survivors keep the width they declare.
    let sparse: Vec<f64> = (0..360).map(|i| i as f64).collect();
    assert!(
        h(&sparse, 0.5, 0.5).iter().all(|w| *w == 0.25),
        "a survivor must not fan across a dropped radial",
    );
    // One radial has no neighbour, and must not read its own azimuth as a
    // 360° gap.
    assert_eq!(h(&[17.0], 0.5, 0.5), vec![0.25]);
    let ring: Vec<f64> = (0..720).map(|i| i as f64 * 0.5).collect();
    let got = h(&ring, 0.5, 0.5);
    assert_eq!(got[0], 0.25);
    assert_eq!(got[719], 0.25);
}

// ── How far a render reaches, and how wide it is drawn ───────────────────

/// Gates in TPIT's long-range surveillance cut.
const TDWR_GATES: usize = 1390;
/// Its gate, km. 1390 of them reach 417.
const TDWR_GATE_KM: f64 = 0.3;
fn tdwr_ground_reach_km() -> f64 {
    crate::beam::ground_range_km(417.0, f64::from(L2_ELEVATION))
}

const TDWR_BEACON_GATES: usize = 2;

fn tdwr_long_range_sweep(beacon_km: f64) -> Scan {
    let beacon_gate = (beacon_km / TDWR_GATE_KM).round() as usize;
    tdwr_sweep_from_gates(
        (0..TDWR_GATES)
            .map(|g| {
                if g.abs_diff(beacon_gate) <= TDWR_BEACON_GATES {
                    200
                } else {
                    0
                }
            })
            .collect(),
    )
}

fn tdwr_filled_long_range_sweep() -> Scan {
    tdwr_sweep_from_gates(vec![200; TDWR_GATES])
}

fn tdwr_sweep_from_gates(gates: Vec<u8>) -> Scan {
    use nexrad_model::data::{MomentData, PulseWidth, RadialStatus, Sweep, VolumeCoveragePattern};

    let radials = (0..360)
        .map(|i| {
            Radial::new(
                0,
                i as u16,
                i as f32,
                1.0,
                RadialStatus::IntermediateRadialData,
                1,
                L2_ELEVATION,
                Some(MomentData::from_fixed_point(
                    TDWR_GATES as u16,
                    0,
                    (TDWR_GATE_KM * 1000.0) as u16,
                    8,
                    SCALE,
                    OFFSET,
                    gates.clone(),
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
    Scan::new(
        VolumeCoveragePattern::new(
            80,
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

fn painted_ranges_km_at(values: &[f32], extent_km: f64, site_lat: f64) -> Vec<f64> {
    let bounds = types::ImageBounds::from_radar_site(site_lat, LON, extent_km);
    // Off the grid's own length, so this reads a 4096-pixel render as readily
    // as a base one and cannot be pointed at the wrong picture.
    let side = values.len().isqrt();
    assert_eq!(side * side, values.len(), "a value grid must be square");
    let merc_span = bounds.mercator_y_max - bounds.mercator_y_min;
    (0..side)
        .flat_map(|row| (0..side).map(move |col| (row, col)))
        .filter(|&(row, col)| !values[row * side + col].is_nan())
        .map(|(row, col)| {
            let lon = bounds.min_lon
                + (col as f64 + 0.5) / side as f64 * (bounds.max_lon - bounds.min_lon);
            let merc_y = bounds.mercator_y_max - (row as f64 + 0.5) / side as f64 * merc_span;
            let lat = (2.0 * merc_y.exp().atan() - std::f64::consts::FRAC_PI_2).to_degrees();
            rustdar_geo::site_bearing_range_km(site_lat, LON, lat, lon).1
        })
        .collect()
}

fn painted_ranges_km(values: &[f32], extent_km: f64) -> Vec<f64> {
    painted_ranges_km_at(values, extent_km, LAT)
}

#[test]
fn a_tdwr_long_range_sweep_is_projected_at_its_own_reach() {
    const BEACON_KM: f64 = 400.2; // gate 1334's centre
    let scan = tdwr_long_range_sweep(BEACON_KM);
    let SweepRender {
        max_range_km: extent_km,
        values,
        ..
    } = render_radar_to_image(&scan, L2_ELEVATION, PRODUCT, LAT, LON).unwrap();

    assert!(
        (extent_km - tdwr_ground_reach_km()).abs() < 1e-9,
        "1390 gates of 0.3 km on a {L2_ELEVATION}\u{b0} cut cover {:.4} km of \
         ground; the render declares {extent_km} km",
        tdwr_ground_reach_km(),
    );

    for az in [0.0, 90.0, 180.0, 270.0] {
        let at = probe_at(extent_km, types::IMAGE_SIZE, az, BEACON_KM);
        assert!(
            !values[at].is_nan(),
            "the beacon 400 km out at {az}° is unpainted at the pixel this \
             render's own projection puts it in",
        );
    }

    // And the picture really is wider than the wall was, measured off the pixels: the
    // eastmost painted column stands at the beacon's outer edge, 170 km past where
    // every gate used to stop.
    let side = types::IMAGE_SIZE;
    let px_per_km = side as f64 / (2.0 * extent_km);
    let east = (0..side)
        .rev()
        .find(|&col| (0..side).any(|row| !values[row * side + col].is_nan()))
        .expect("the beacon painted something");
    let painted_km = (east as f64 + 0.5 - side as f64 / 2.0) / px_per_km;
    assert!(
        painted_km > types::BASE_EXTENT_KM,
        "the outermost painted column stands {painted_km:.1} km out, still \
         inside the {} km wall this render is here to be past",
        types::BASE_EXTENT_KM,
    );
    let band_edge_km =
        ((BEACON_KM / TDWR_GATE_KM).round() + TDWR_BEACON_GATES as f64 + 0.5) * TDWR_GATE_KM;
    assert!(
        (painted_km - band_edge_km).abs() < 2.0 / px_per_km,
        "the outermost painted column stands {painted_km:.2} km out against a \
         beacon band ending at {band_edge_km:.2} km",
    );
}

/// Gates in a TDWR's Doppler moments, and the gate they are.
const TDWR_DOPPLER_GATES: usize = 592;
/// See [`TDWR_DOPPLER_GATES`]. 592 of these reach 88.8 km of beam.
const TDWR_DOPPLER_GATE_KM: f64 = 0.15;

fn tdwr_doppler_volume() -> Scan {
    use nexrad_model::data::{MomentData, PulseWidth, RadialStatus, Sweep, VolumeCoveragePattern};

    let radials = (0..360)
        .map(|i| {
            Radial::new(
                0,
                i as u16,
                i as f32,
                1.0,
                RadialStatus::IntermediateRadialData,
                1,
                L2_ELEVATION,
                Some(MomentData::from_fixed_point(
                    TDWR_GATES as u16,
                    0,
                    (TDWR_GATE_KM * 1000.0) as u16,
                    8,
                    SCALE,
                    OFFSET,
                    vec![200; TDWR_GATES],
                )),
                Some(MomentData::from_fixed_point(
                    TDWR_DOPPLER_GATES as u16,
                    0,
                    (TDWR_DOPPLER_GATE_KM * 1000.0) as u16,
                    8,
                    2.0,
                    129.0,
                    vec![200; TDWR_DOPPLER_GATES],
                )),
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
            80,
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

#[test]
fn a_tdwr_doppler_sweep_is_projected_at_its_own_reach_not_the_base_extent() {
    let doppler_ground_km = crate::beam::ground_range_km(
        TDWR_DOPPLER_GATES as f64 * TDWR_DOPPLER_GATE_KM,
        f64::from(L2_ELEVATION),
    );

    for (name, lat, lon) in [
        ("TOKC", 35.2764, -97.5100),
        ("TDAL", 32.9264, -96.9683),
        ("TPIT", 40.5011, -80.4867),
        ("TATL", 33.6467, -84.2622),
    ] {
        let scan = tdwr_doppler_volume();

        let vel =
            render_radar_to_image(&scan, L2_ELEVATION, types::RadarProduct::Velocity, lat, lon)
                .expect("the Doppler moment is on the sweep");
        let refl = render_radar_to_image(&scan, L2_ELEVATION, PRODUCT, lat, lon)
            .expect("the reflectivity moment is on the same sweep");

        assert_ne!(
            vel.max_range_km,
            types::BASE_EXTENT_KM,
            "{name}: a velocity pane is still framed at 230 km",
        );
        assert!(
            (vel.max_range_km - doppler_ground_km).abs() < 1e-9,
            "{name}: 592 gates of 0.15 km cover {doppler_ground_km:.5} km of \
             ground; the render declares {}",
            vel.max_range_km,
        );

        assert!(
            (refl.max_range_km - tdwr_ground_reach_km()).abs() < 1e-9,
            "{name}: the reflectivity beside it declares {}",
            refl.max_range_km,
        );
        assert!(
            refl.max_range_km > vel.max_range_km * 4.6,
            "{name}: reflectivity reaches {:.2} km and velocity {:.2} km; a \
             volume-wide extent cannot express both",
            refl.max_range_km,
            vel.max_range_km,
        );

        let bounds = types::ImageBounds::from_radar_site(lat, lon, vel.max_range_km);
        let ring_km = (bounds.max_lat - lat) * rustdar_geo::KM_PER_DEGREE_LAT;
        assert!(
            (ring_km - doppler_ground_km).abs() < 1e-6,
            "{name}: the ring stands {ring_km:.4} km out around \
             {doppler_ground_km:.4} km of data",
        );

        let side = types::IMAGE_SIZE;
        let east = (0..side)
            .rev()
            .find(|&col| (0..side).any(|row| !vel.values[row * side + col].is_nan()))
            .expect("the Doppler disc painted something");
        assert!(
            east >= side - 2,
            "{name}: the velocity echo stops at column {east} of {side}",
        );
    }
}

#[test]
fn a_sweep_whose_leading_radial_is_blank_is_still_found_and_still_framed_by_its_own_reach() {
    use nexrad_model::data::Sweep;

    let full = tdwr_doppler_volume();
    let sweep = &full.sweeps()[0];

    let radials: Vec<Radial> = sweep
        .radials()
        .iter()
        .enumerate()
        .map(|(i, r)| {
            Radial::new(
                r.collection_timestamp(),
                r.azimuth_number(),
                r.azimuth_angle_degrees(),
                r.azimuth_spacing_degrees(),
                r.radial_status(),
                r.elevation_number(),
                r.elevation_angle_degrees(),
                r.reflectivity().cloned(),
                if i == 0 { None } else { r.velocity().cloned() },
                r.spectrum_width().cloned(),
                r.differential_reflectivity().cloned(),
                r.differential_phase().cloned(),
                r.correlation_coefficient().cloned(),
                r.clutter_filter_power().cloned(),
            )
        })
        .collect();
    assert!(radials[0].velocity().is_none(), "the fixture must be blank");
    assert!(radials[1].velocity().is_some(), "and only in front");

    let scan = Scan::new(
        full.coverage_pattern().clone(),
        vec![Sweep::new(1, radials)],
    );

    let vel = render_radar_to_image(&scan, L2_ELEVATION, types::RadarProduct::Velocity, LAT, LON)
        .expect("359 radials carry the moment and the sweep must still be found");

    let doppler_ground_km = crate::beam::ground_range_km(
        TDWR_DOPPLER_GATES as f64 * TDWR_DOPPLER_GATE_KM,
        f64::from(L2_ELEVATION),
    );
    assert!(
        (vel.max_range_km - doppler_ground_km).abs() < 1e-9,
        "one blank leading radial reframed the pane at {} km instead of \
         {doppler_ground_km:.5} km",
        vel.max_range_km,
    );
}

// ── Where a tilt's gates are drawn ───────────────────────────────────────

fn tilted_beacon_sweep(
    elevation_deg: f32,
    gate_km: f64,
    n_gates: usize,
    beacon_slant_km: f64,
    band_gates: usize,
) -> Scan {
    use nexrad_model::data::{
        ChannelConfiguration, ElevationCut, MomentData, PulseWidth, RadialStatus, Sweep,
        VolumeCoveragePattern, WaveformType,
    };

    let beacon_gate = (beacon_slant_km / gate_km).round() as usize;
    let gates: Vec<u8> = (0..n_gates)
        .map(|g| {
            if g.abs_diff(beacon_gate) <= band_gates {
                200
            } else {
                0
            }
        })
        .collect();
    let radials = (0..360)
        .map(|i| {
            Radial::new(
                0,
                i as u16,
                i as f32,
                1.0,
                RadialStatus::IntermediateRadialData,
                1,
                elevation_deg,
                Some(MomentData::from_fixed_point(
                    n_gates as u16,
                    0,
                    (gate_km * 1000.0) as u16,
                    8,
                    SCALE,
                    OFFSET,
                    gates.clone(),
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
    let cut = ElevationCut::new(
        f64::from(elevation_deg),
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
            80,
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

fn ring_bounds_km(values: &[f32], extent_km: f64) -> (f64, f64, f64) {
    let ranges = painted_ranges_km(values, extent_km);
    assert!(!ranges.is_empty(), "the fixture painted nothing at all");
    let near = ranges.iter().copied().fold(f64::INFINITY, f64::min);
    let far = ranges.iter().copied().fold(0.0f64, f64::max);
    (near, far, (near + far) / 2.0)
}

#[test]
fn a_45_degree_sweep_lands_at_the_same_ground_range_in_2d_and_3d() {
    const ELEV: f32 = 45.0;
    const SLANT_KM: f64 = 20.0;
    const GATE_KM: f64 = 0.25;
    let ground_km = crate::beam::ground_range_km(SLANT_KM, f64::from(ELEV));

    let scan = tilted_beacon_sweep(ELEV, GATE_KM, 600, SLANT_KM, 2);

    // ── The plan view ──
    let SweepRender {
        max_range_km: extent_km,
        values,
        ..
    } = render_radar_to_image(&scan, ELEV, PRODUCT, LAT, LON).unwrap();
    let px_per_km = types::IMAGE_SIZE as f64 / (2.0 * extent_km);

    assert!(
        !values[probe_at(extent_km, types::IMAGE_SIZE, 90.0, ground_km)].is_nan(),
        "the beacon is unpainted at the {ground_km:.3} km of ground it sits over",
    );
    assert!(
        values[probe_at(extent_km, types::IMAGE_SIZE, 90.0, SLANT_KM)].is_nan(),
        "the beacon is still painted out at its {SLANT_KM} km slant range, \
         which is 5.86 km past the ground it is over",
    );

    // ── The 3D sampler, over the same volume ──
    let sampler = crate::sampler::VolumeSampler::new(&scan, PRODUCT)
        .expect("a one-cut volume builds a one-rung ladder");
    let sampled_at = |g: f64| {
        sampler
            .column(90.0, g)
            .rungs()
            .first()
            .and_then(|r| r.sample.value())
            .is_some()
    };
    assert!(
        sampled_at(ground_km),
        "the sampler does not find the beacon at {ground_km:.3} km of ground",
    );
    assert!(
        !sampled_at(SLANT_KM),
        "the sampler answers at the slant range, so this fixture proves nothing",
    );

    // ── And the two bands are the same band ──
    let (_, _, centre_2d) = ring_bounds_km(&values, extent_km);
    let mut lo = f64::INFINITY;
    let mut hi = 0.0f64;
    for step in 0..2000 {
        let g = step as f64 * 0.02;
        if sampled_at(g) {
            lo = lo.min(g);
            hi = hi.max(g);
        }
    }
    let centre_3d = (lo + hi) / 2.0;
    assert!(
        (centre_2d - centre_3d).abs() < GATE_KM,
        "the plan view centres the band at {centre_2d:.3} km and the sampler \
         at {centre_3d:.3} km; they must agree to within one {GATE_KM} km gate",
    );
    assert!(
        (centre_2d - ground_km).abs() < 2.0 / px_per_km,
        "the painted band centres at {centre_2d:.3} km against {ground_km:.3} \
         km of ground",
    );
}

#[test]
fn a_tdwr_steep_tilt_renders_at_half_its_slant_range() {
    const ELEV: f32 = 60.0;
    const SLANT_KM: f64 = 24.0;
    const GATE_KM: f64 = 0.15;
    let scan = tilted_beacon_sweep(ELEV, GATE_KM, 592, SLANT_KM, 3);

    let SweepRender {
        max_range_km: extent_km,
        values,
        ..
    } = render_radar_to_image(&scan, ELEV, PRODUCT, LAT, LON).unwrap();
    let (near, far, centre) = ring_bounds_km(&values, extent_km);

    assert!(
        (centre - 12.0).abs() < 0.3,
        "a 24 km gate on a 60° tilt belongs 12.0 km out; the ring measures \
         {near:.2}..{far:.2} km, centred at {centre:.2}",
    );
    assert!(
        far < SLANT_KM * 0.6,
        "the outermost painted pixel stands {far:.2} km out, which is most of \
         the way to the {SLANT_KM} km slant range this tilt used to be drawn at",
    );
}

#[test]
fn a_frame_is_sized_by_the_ground_its_sweep_covers() {
    const ELEV: f32 = 0.2637;
    let scan = tilted_beacon_sweep(ELEV, TDWR_GATE_KM, TDWR_GATES, 400.2, TDWR_BEACON_GATES);
    let SweepRender {
        max_range_km: extent_km,
        ..
    } = render_radar_to_image(&scan, ELEV, PRODUCT, LAT, LON).unwrap();

    let expected = crate::beam::ground_range_km(417.0, f64::from(ELEV));
    assert!(
        (extent_km - expected).abs() < 1e-9,
        "1390 gates of 0.3 km on a {ELEV}° cut cover {expected:.4} km of \
         ground; the render declares {extent_km:.4} km",
    );
    assert!(
        extent_km < 417.0,
        "the frame is still the beam's length rather than the ground's",
    );
}

#[test]
fn a_low_tilt_beacon_moves_less_than_a_pixel() {
    const ELEV: f32 = 0.5;
    const SLANT_KM: f64 = 200.0;
    let scan = tilted_beacon_sweep(ELEV, 0.25, 900, SLANT_KM, 2);
    let SweepRender {
        max_range_km: extent_km,
        values,
        ..
    } = render_radar_to_image(&scan, ELEV, PRODUCT, LAT, LON).unwrap();

    let (_, _, centre) = ring_bounds_km(&values, extent_km);
    let px_per_km = types::IMAGE_SIZE as f64 / (2.0 * extent_km);
    let moved_px = (SLANT_KM - centre) * px_per_km;
    assert!(
        moved_px.abs() < 1.0,
        "a 0.5° beacon at {SLANT_KM} km moved {moved_px:.3} px, to {centre:.3} km",
    );
}

#[test]
fn a_sweep_inside_the_old_floor_is_drawn_at_its_own_reach() {
    let azimuths: Vec<f32> = (0..360).map(|i| i as f32).collect();
    let scan = l2_sweep(&[200; 360], &azimuths, 1.0, false);
    let SweepRender {
        max_range_km: extent_km,
        values,
        ..
    } = render_radar_to_image(&scan, L2_ELEVATION, PRODUCT, LAT, LON).unwrap();

    assert!(
        (extent_km - l2_ground_reach_km()).abs() < 1e-9,
        "600 gates of 0.25 km cover {:.5} km of ground; the render declares \
         {extent_km}",
        l2_ground_reach_km(),
    );
    assert_ne!(
        extent_km,
        types::BASE_EXTENT_KM,
        "a 150 km sweep must not be raised to a 230 km frame",
    );

    // Due east and due west of the site the projection's `cos φ₀ / cos φ`
    // factor is exactly 1 — the destination sits at the site's own latitude —
    // so the extreme columns of the painted disc are the reach in pixels,
    // with no Mercator row arithmetic in the way.
    let side = types::IMAGE_SIZE;
    let row = side / 2;
    let painted = |col: usize| !values[row * side + col].is_nan();
    let east = (0..side).rev().find(|&c| painted(c)).expect("echo east");
    let west = (0..side).find(|&c| painted(c)).expect("echo west");

    let px_per_km = side as f64 / (2.0 * extent_km);
    let radius_km = (east - west) as f64 / 2.0 / px_per_km;
    assert!(
        (radius_km - 150.0).abs() < 1.0,
        "600 gates of 0.25 km must fill 150 km of the frame; the disc measures \
         {radius_km:.2} km across columns {west}..{east} at {px_per_km:.4} px/km",
    );

    assert!(
        west <= 1 && east >= side - 2,
        "the echo runs columns {west}..{east} of {side}; a sweep drawn at its \
         own reach must reach its own edge",
    );
}

#[test]
fn nothing_is_painted_outside_the_extent_a_render_declares() {
    let azimuths: Vec<f32> = (0..360).map(|i| i as f32).collect();
    for (scan, why) in [
        (
            l2_sweep(&[200; 360], &azimuths, 1.0, false),
            "150 km of echo",
        ),
        (tdwr_filled_long_range_sweep(), "a filled 417 km TDWR cut"),
    ] {
        for site_lat in [LAT, 47.0411, 48.1946] {
            let SweepRender {
                max_range_km: extent_km,
                values,
                ..
            } = render_radar_to_image(&scan, L2_ELEVATION, PRODUCT, site_lat, LON).unwrap();
            let ranges = painted_ranges_km_at(&values, extent_km, site_lat);
            assert!(
                !ranges.is_empty(),
                "{why} at {site_lat}\u{b0}N painted nothing"
            );

            let furthest = ranges.iter().copied().fold(0.0f64, f64::max);
            let slop = 1.0 / (types::IMAGE_SIZE as f64 / (2.0 * extent_km));
            assert!(
                furthest <= extent_km + slop,
                "{why} at {site_lat}\u{b0}N: a pixel {furthest:.3} km out on a \
                 raster declaring {extent_km:.3} km, past the one-pixel \
                 ({slop:.3} km) bound by {:.3} km",
                furthest - extent_km - slop,
            );
            assert!(
                furthest > extent_km - 2.0 * slop,
                "{why} at {site_lat}\u{b0}N stops {:.3} km short of its own \
                 {extent_km:.3} km frame, so the bound above is vacuous",
                extent_km - furthest,
            );
        }
    }

    // And on the cardinals, measured as pixel radii rather than through the
    // bounds, so the row and column arithmetic is checked without the
    // geography's help.
    let SweepRender {
        max_range_km: extent_km,
        values,
        ..
    } = render_radar_to_image(
        &l2_sweep(&[200; 360], &azimuths, 1.0, false),
        L2_ELEVATION,
        PRODUCT,
        LAT,
        LON,
    )
    .unwrap();
    let side = types::IMAGE_SIZE;
    let px_per_km = side as f64 / (2.0 * extent_km);
    for (name, row, col) in [
        (
            "east",
            side / 2,
            (0..side)
                .rev()
                .find(|&c| !values[side / 2 * side + c].is_nan())
                .unwrap(),
        ),
        (
            "west",
            side / 2,
            (0..side)
                .find(|&c| !values[side / 2 * side + c].is_nan())
                .unwrap(),
        ),
        (
            "north",
            (0..side)
                .find(|&r| !values[r * side + side / 2].is_nan())
                .unwrap(),
            side / 2,
        ),
        (
            "south",
            (0..side)
                .rev()
                .find(|&r| !values[r * side + side / 2].is_nan())
                .unwrap(),
            side / 2,
        ),
    ] {
        let radius_px =
            ((row as f64 - side as f64 / 2.0).abs()).max((col as f64 - side as f64 / 2.0).abs());
        let radius_km = radius_px / px_per_km;
        assert!(
            radius_km <= extent_km + 1.0 / px_per_km,
            "the {name}most painted pixel stands {radius_km:.3} km out on a \
             raster declaring {extent_km:.3} km",
        );
    }
}

#[test]
fn nrot_does_not_smear_past_its_sector() {
    let scan = nrot_sector(72, 0.5);
    let SweepRender {
        max_range_km: extent_km,
        values,
        ..
    } = render_radar_to_image(
        &scan,
        L2_ELEVATION,
        types::RadarProduct::NormalizedRotation,
        LAT,
        LON,
    )
    .unwrap();

    let painted = values.iter().filter(|v| !v.is_nan()).count();
    assert!(painted > 1_000, "the NROT sector painted only {painted} px");

    for range in [20.0, 30.0, 40.0, 50.0] {
        for az in [37.0, -1.5] {
            let v = values[probe_at(extent_km, types::IMAGE_SIZE, az, range)];
            assert!(
                v.is_nan(),
                "({az}°, {range} km) is painted - the sector runs 0° to \
                 35.5° and the display must end where it does",
            );
        }
    }
}

#[test]
fn max_range_is_robust_to_a_truncated_first_radial() {
    let mut gates = vec![600u16; 8];
    gates[0] = 100;
    let scan = truncated_sweep(&gates);
    let radials = find_sweep(&scan, PRODUCT, L2_ELEVATION).expect("the sweep is reachable");

    assert_eq!(
        super::compute_max_range(radials, PRODUCT),
        150.0,
        "600 gates of 0.25 km reach 150 km; the 100-gate opener reaches 25",
    );
    assert_eq!(
        super::compute_max_range(radials, types::RadarProduct::Velocity),
        0.0,
        "a product no radial carries reaches nowhere",
    );
}

/// A sweep whose radials carry different gate counts, 1° apart from 0°.
fn truncated_sweep(gate_counts: &[u16]) -> Scan {
    use nexrad_model::data::{MomentData, PulseWidth, RadialStatus, Sweep, VolumeCoveragePattern};
    let radials = gate_counts
        .iter()
        .enumerate()
        .map(|(i, &n)| {
            Radial::new(
                0,
                i as u16,
                i as f32,
                1.0,
                RadialStatus::IntermediateRadialData,
                1,
                L2_ELEVATION,
                Some(MomentData::from_fixed_point(
                    n,
                    0,
                    250,
                    8,
                    SCALE,
                    OFFSET,
                    vec![200u8; n as usize],
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

#[test]
fn a_wedge_width_is_never_negative_absent_or_unbounded() {
    let w = super::l2_wedge_width_deg;
    assert_eq!(w(0.5, 0.5), 0.5, "the ordinary super-res case");
    assert_eq!(w(1.0, 1.0), 1.0, "the ordinary legacy case");
    assert_eq!(w(0.0, 0.5), 0.5, "no declaration falls back to the median");
    assert_eq!(w(-1.0, 0.5), 0.5, "nor is a negative one a narrow wedge");
    assert_eq!(w(f64::NAN, 0.5), 0.5, "nor a NaN");
    assert_eq!(w(0.5, 0.25), 0.375, "1.5 median steps is the tighter cap");
    assert_eq!(w(45.0, 2.0), 2.0, "MAX_WEDGE_DEG is the tighter cap");
    assert_eq!(w(0.0, 350.0), 2.0, "and it bounds the fallback too");
}

#[test]
fn write_key_ranks_radial_major_and_never_reads_as_empty() {
    let k = |radial, gate| write_key(GateId { radial, gate });
    assert!(k(0, 0) > 0);
    assert!(k(0, 1) > k(0, 0));
    assert!(k(1, 0) > k(0, N_BINS));
    assert!(k(719, 1831) > k(718, 1831));
}

fn message_with_lying_scale_factor(product_code: i16, bins: usize) -> Level3Message {
    use nexrad_level3::model::{DataLayer, MessageHeader, SymbologyBlock};

    let packet = RadialPacket {
        first_range_bin: 0,
        num_range_bins: bins as u16,
        i_center: 0,
        j_center: 0,
        scale_factor: 0.999,
        is_legacy: false,
        xdr_data_scale: Some(SCALE),
        xdr_data_offset: Some(OFFSET),
        radials: (0..N_RADIALS)
            .map(|i| RadialRun {
                start_angle: i as f32,
                angle_delta: 1.0,
                gate_values: vec![100; bins],
            })
            .collect(),
    };
    Level3Message {
        header: MessageHeader {
            message_code: product_code,
            date_of_message: 20661,
            time_of_message: 0,
            message_length: 0,
            source_id: 0,
            destination_id: 0,
            number_of_blocks: 3,
        },
        pdb: ProductDescriptionBlock {
            block_divider: -1,
            latitude: LAT,
            longitude: LON,
            height: 1000,
            product_code,
            operational_mode: 2,
            vcp: 212,
            sequence_number: 0,
            volume_scan_number: 1,
            volume_scan_date: 20661,
            volume_scan_time: 0,
            generation_date: 20661,
            generation_time: 0,
            product_specific_1: 0,
            product_specific_2: 0,
            elevation_number: 1,
            product_specific_3: 5,
            thresholds: [0; 16],
            product_specific_47_53: [0; 7],
            version: 0,
            spot_blank: 0,
            symbology_offset: 60,
            graphic_offset: 0,
            tabular_offset: 0,
        },
        symbology: Some(SymbologyBlock {
            block_id: 1,
            block_length: 0,
            num_layers: 1,
            layers: vec![DataLayer {
                layer_length: 0,
                packets: vec![nexrad_level3::model::DataPacket::DigitalRadial(packet)],
            }],
        }),
    }
}

#[test]
fn message_path_prefers_the_pdb_gate_spacing_over_the_packets() {
    const ICD_BINS: usize = 1200;
    const PACKET_BINS: usize = 460;

    let SweepRender {
        max_range_km: extent_km,
        ..
    } = render_level3_message_to_image(
        &message_with_lying_scale_factor(163, ICD_BINS),
        types::RadarProduct::SpecificDifferentialPhase,
        LAT,
        LON,
    )
    .unwrap();
    assert!(
        (extent_km - ICD_BINS as f64 * 0.25).abs() < 1e-9,
        "163 must render at the ICD's 0.25 km spacing, got an extent of {extent_km} km \
             from {ICD_BINS} gates"
    );

    let SweepRender {
        max_range_km: extent_km,
        ..
    } = render_level3_message_to_image(
        &message_with_lying_scale_factor(94, PACKET_BINS),
        PRODUCT,
        LAT,
        LON,
    )
    .unwrap();
    let packet_km = 1.0 / 0.999_f32 as f64;
    assert!(
        (extent_km - PACKET_BINS as f64 * packet_km).abs() < 1e-9,
        "a product with no PDB override must keep the packet's spacing, got an extent \
             of {extent_km} km from {PACKET_BINS} gates"
    );
}

// ── Which sweep a requested tilt reaches ─────────────────────────────────

fn settling_sweep(number: u8, first: f32, flown: f32, velocity: bool) -> nexrad_model::data::Sweep {
    const SETTLING: usize = 30;
    let radials = (0..N_RADIALS)
        .map(|i| {
            let elevation = if i < SETTLING {
                first + (flown - first) * (i as f32 / SETTLING as f32)
            } else {
                flown
            };
            let moment = |gates: usize| {
                nexrad_model::data::MomentData::from_fixed_point(
                    gates as u16,
                    0,
                    250,
                    8,
                    SCALE,
                    OFFSET,
                    vec![200u8; gates],
                )
            };
            Radial::new(
                0,
                i as u16,
                i as f32 * (360.0 / N_RADIALS as f32),
                360.0 / N_RADIALS as f32,
                nexrad_model::data::RadialStatus::IntermediateRadialData,
                number,
                elevation,
                Some(moment(600)),
                velocity.then(|| moment(400)),
                None,
                None,
                None,
                None,
                None,
            )
        })
        .collect();
    nexrad_model::data::Sweep::new(number, radials)
}

fn scan_of(sweeps: Vec<nexrad_model::data::Sweep>) -> Scan {
    Scan::new(crate::render_input::placeholder_coverage_pattern(0), sweeps)
}

#[test]
fn find_sweep_matches_the_flown_tilt_not_the_opening_radial() {
    let scan = scan_of(vec![settling_sweep(1, 0.68, 0.44, false)]);
    assert!(
        find_sweep(&scan, PRODUCT, 0.4).is_some(),
        "0.4° names the cut this sweep flew and must reach it",
    );
    assert!(
        find_sweep(&scan, PRODUCT, 0.7).is_none(),
        "0.7° is where the antenna was still settling, not a tilt the volume flew",
    );
}

#[test]
fn adjacent_cuts_are_reached_by_their_own_labels() {
    let low = settling_sweep(1, 0.676, 0.44, false);
    let high = settling_sweep(2, 0.739, 0.84, false);
    let scan = scan_of(vec![low, high]);

    let at = |e: f32| {
        find_sweep(&scan, PRODUCT, e).map(|r| crate::volumetric::sweep_elevation_deg(r).unwrap())
    };
    let (Some(a), Some(b)) = (at(0.4), at(0.8)) else {
        panic!(
            "both cuts must be reachable, got {:?} / {:?}",
            at(0.4),
            at(0.8)
        );
    };
    assert!(
        (a - 0.44).abs() < 1e-4,
        "0.4° must draw the 0.44° cut, drew {a}"
    );
    assert!(
        (b - 0.84).abs() < 1e-4,
        "0.8° must draw the 0.84° cut, drew {b}"
    );
    assert!(
        (a - b).abs() > 0.3,
        "the two labels must draw different sweeps, both drew {a}",
    );
    assert!(at(0.6).is_none(), "0.6° is not a tilt this volume flew");
}

#[test]
fn a_sails_repeat_still_resolves_to_the_newer_sweep() {
    let scan = scan_of(vec![
        settling_sweep(1, 0.30, 0.48, false),
        settling_sweep(2, 0.71, 0.48, false),
    ]);
    let found = find_sweep(&scan, PRODUCT, 0.5).expect("the cut is reachable");
    assert_eq!(
        found[0].azimuth_number(),
        scan.sweeps()[1].radials()[0].azimuth_number(),
        "the newer of two sweeps at one tilt must win",
    );
    assert_eq!(
        found[0].elevation_number(),
        2,
        "the newer sweep is elevation number 2",
    );
}

#[test]
fn a_split_cut_still_gives_reflectivity_its_surveillance_half() {
    let scan = scan_of(vec![
        settling_sweep(1, 0.30, 0.48, false),
        settling_sweep(2, 0.71, 0.48, true),
    ]);
    let found = find_sweep(&scan, PRODUCT, 0.5).expect("the cut is reachable");
    assert!(
        found[0].velocity().is_none(),
        "reflectivity must take the surveillance half, not the newer Doppler one",
    );
    let vel = find_sweep(&scan, types::RadarProduct::Velocity, 0.5).expect("velocity is there");
    assert!(
        vel[0].velocity().is_some(),
        "the velocity family still takes the Doppler half",
    );
}

#[test]
fn the_window_does_not_reach_the_next_cut_along() {
    let scan = scan_of(vec![settling_sweep(1, 0.20, 0.48, false)]);
    assert!(
        find_sweep(&scan, PRODUCT, 0.5).is_some(),
        "its own label reaches it"
    );
    for absent in [0.2, 0.3, 0.7, 0.9] {
        assert!(
            find_sweep(&scan, PRODUCT, absent).is_none(),
            "{absent}° is not a tilt this volume flew and must draw nothing",
        );
    }
}

#[test]
fn every_offered_label_reaches_the_cut_it_names() {
    for step in 0..=240u32 {
        let flown = step as f32 * 0.05;
        let scan = scan_of(vec![settling_sweep(1, flown + 0.31, flown, false)]);
        let label = (f64::from(flown) * 10.0).round() as f32 / 10.0;

        let found = find_sweep(&scan, PRODUCT, label).unwrap_or_else(|| {
            panic!("a cut flown at {flown}° is offered as {label}° and must be reachable")
        });
        let drawn = crate::volumetric::sweep_elevation_deg(found).expect("the sweep has radials");
        assert!(
            (drawn - f64::from(flown)).abs() < 1e-4,
            "{label}° drew a sweep at {drawn}°, not the {flown}° cut it names",
        );
        assert_eq!(
            find_closest_elevation(&scan, PRODUCT, flown),
            Some(label),
            "the loop's snap must agree with the label the picker offers",
        );
    }
}

#[test]
fn find_closest_elevation_snaps_to_the_flown_tilt() {
    let scan = scan_of(vec![
        settling_sweep(1, 0.68, 0.44, false),
        settling_sweep(2, 0.30, 0.84, false),
    ]);
    assert_eq!(find_closest_elevation(&scan, PRODUCT, 0.5), Some(0.4));
    assert_eq!(find_closest_elevation(&scan, PRODUCT, 0.8), Some(0.8));
}

#[test]
fn the_render_paths_site_height_is_the_feedhorn() {
    crate::sites::fixture::install();
    const KTLX: (f64, f64) = (35.33306, -97.2775);
    assert_eq!(
        super::render_site_height_ft(KTLX.0, KTLX.1),
        1214.0 + 62.0,
        "the feedhorn",
    );
    assert_ne!(
        super::render_site_height_ft(KTLX.0, KTLX.1),
        1214.0,
        "the ground under the tower is not the datum a beam height is above",
    );
}

const D_GATES: usize = 400;

fn dp_moment8(scale: f32, offset: f32, v: f64) -> nexrad_model::data::MomentData {
    let raw = ((v * f64::from(scale) + f64::from(offset)).round() as u16) as u8;
    nexrad_model::data::MomentData::from_fixed_point(
        D_GATES as u16,
        125,
        250,
        8,
        scale,
        offset,
        vec![raw; D_GATES],
    )
}

fn dp_moment16(
    scale: f32,
    offset: f32,
    at: &dyn Fn(usize) -> f64,
) -> nexrad_model::data::MomentData {
    let mut bytes = Vec::with_capacity(D_GATES * 2);
    for i in 0..D_GATES {
        let raw = (at(i) * f64::from(scale) + f64::from(offset)).round() as u16;
        bytes.extend_from_slice(&raw.to_be_bytes());
    }
    nexrad_model::data::MomentData::from_fixed_point(
        D_GATES as u16,
        125,
        250,
        16,
        scale,
        offset,
        bytes,
    )
}

fn dual_pol_tilt(number: u8, elev: f32) -> nexrad_model::data::Sweep {
    let radials = (0..360)
        .map(|k| {
            Radial::new(
                0,
                k as u16,
                k as f32 + 0.5,
                1.0,
                nexrad_model::data::RadialStatus::IntermediateRadialData,
                number,
                elev,
                Some(dp_moment8(2.0, 66.0, 30.0)),
                None,
                None,
                Some(dp_moment8(16.0, 128.0, 1.0)),
                Some(dp_moment16(10.0, 2.0, &|i| 60.0 + 0.02 * i as f64)),
                Some(dp_moment16(500.0, 2.0, &|_| 0.99)),
                None,
            )
        })
        .collect();
    nexrad_model::data::Sweep::new(number, radials)
}

#[test]
fn the_hybrid_classification_changes_with_the_environmental_heights() {
    crate::sites::fixture::install();
    /// Dry snow: what the low freezing level puts the beam above.
    const DS: f32 = 40.0;
    /// Rain: what the default melting layer puts the beam below.
    const RA: f32 = 60.0;

    // The fixture reaches 100 km, well inside the floor, so both renders take
    // the base raster and neither is asking anything of the size cascade.
    let scan = scan_of(vec![dual_pol_tilt(1, 0.5)]);
    let defaults = super::render_hhc_to_image(&scan, LAT, LON, None, None, types::IMAGE_SIZE)
        .expect("the fixture classifies");
    let sounding =
        super::render_hhc_to_image(&scan, LAT, LON, Some((0.8, 2.0)), None, types::IMAGE_SIZE)
            .expect("the fixture classifies");

    let painted = |grid: &[f32]| grid.iter().filter(|v| !v.is_nan()).count();
    let all_of = |grid: &[f32], class: f32| grid.iter().filter(|v| **v == class).count();

    let cells = painted(&defaults.values);
    assert!(
        cells > 0,
        "the fixture painted nothing, so it proves nothing"
    );
    assert_eq!(
        painted(&sounding.values),
        cells,
        "both environments must classify the same disc — a difference in \
         *coverage* would confound the difference in class",
    );
    assert_eq!(
        all_of(&defaults.values, RA),
        cells,
        "with the adaptation defaults the melting layer sits above the beam \
         and the whole disc is rain",
    );
    assert_eq!(
        all_of(&sounding.values, DS),
        cells,
        "with the sounding's 0.8 km freezing level the beam climbs out of the \
         melting layer and the whole disc is dry snow",
    );
    assert_ne!(
        defaults.image, sounding.image,
        "and the rasterized pixels differ too, which is what a pane shows",
    );
}

// ── The adaptive raster size ─────────────────────────────────────────────────

/// The side a static desktop or mobile pane offers, and the only 4096 in this file.
const LONG_RANGE_SIDE: usize = 4096;

#[test]
fn a_render_inside_the_floor_ignores_the_long_range_ceiling_entirely() {
    let azimuths: Vec<f32> = (0..360).map(|i| i as f32).collect();
    let scan = l2_sweep(&[200; 360], &azimuths, 1.0, false);

    let base = render_radar_to_image_full(
        &scan,
        L2_ELEVATION,
        PRODUCT,
        LAT,
        LON,
        crate::srv::MotionInputs::default(),
        None,
        None,
        &crate::nyquist::DeclaredNyquist::empty(),
    )
    .expect("the fixture renders");
    let offered = render_radar_to_image_full_sized(
        &scan,
        L2_ELEVATION,
        PRODUCT,
        LAT,
        LON,
        crate::srv::MotionInputs::default(),
        None,
        None,
        &crate::nyquist::DeclaredNyquist::empty(),
        LONG_RANGE_SIDE,
    )
    .expect("the fixture renders at the long-range ceiling too");

    assert_eq!(
        base.max_range_km, offered.max_range_km,
        "a 150 km sweep must project at the floor under either ceiling",
    );
    assert_eq!(
        base.image.len(),
        types::IMAGE_SIZE * types::IMAGE_SIZE * 4,
        "the floor's raster is the base size",
    );
    assert_eq!(
        base.image, offered.image,
        "the image moved under an unused ceiling"
    );
    // `NaN` is most of a value grid, and `NaN != NaN`, so the bits are what is
    // compared — a grid full of quiet NaNs would otherwise never be equal to
    // itself and this assertion would be vacuous.
    let bits = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
    assert_eq!(
        bits(&base.values),
        bits(&offered.values),
        "the value grid moved under an unused ceiling",
    );
}

#[test]
fn a_tdwr_long_range_sweep_takes_the_long_range_raster() {
    const BEACON_KM: f64 = 400.2; // gate 1334's centre
    let scan = tdwr_long_range_sweep(BEACON_KM);
    let SweepRender {
        image,
        max_range_km: extent_km,
        values,
        ..
    } = render_radar_to_image_full_sized(
        &scan,
        L2_ELEVATION,
        PRODUCT,
        LAT,
        LON,
        crate::srv::MotionInputs::default(),
        None,
        None,
        &crate::nyquist::DeclaredNyquist::empty(),
        LONG_RANGE_SIDE,
    )
    .expect("the fixture renders");

    assert_eq!(
        image.len(),
        LONG_RANGE_SIDE * LONG_RANGE_SIDE * 4,
        "a 417 km sweep under a {LONG_RANGE_SIDE} px ceiling must take it",
    );
    assert_eq!(values.len(), LONG_RANGE_SIDE * LONG_RANGE_SIDE);
    assert!(
        (extent_km - tdwr_ground_reach_km()).abs() < 1e-9,
        "the extent is the ground the sweep covers, not the raster's size: \
         {extent_km}",
    );

    for az in [0.0, 90.0, 180.0, 270.0] {
        let at = probe_at(extent_km, LONG_RANGE_SIDE, az, BEACON_KM);
        assert!(
            !values[at].is_nan(),
            "the beacon 400 km out at {az}° is unpainted at the pixel this \
             render's own projection puts it in",
        );
    }
}

#[test]
fn the_long_range_raster_keeps_the_floors_km_per_pixel() {
    const TOLERANCE: f64 = 0.001;

    let floor = types::IMAGE_SIZE as f64 / (2.0 * types::BASE_EXTENT_KM);
    for (extent_km, why) in [
        (300.125, "a WSR-88D Doppler cut, 2.125 + 1192 × 0.25 km"),
        (417.0, "a TDWR long-range reflectivity cut"),
        (
            460.125,
            "a WSR-88D surveillance cut, 2.125 + 1832 × 0.25 km",
        ),
    ] {
        // The super-res gate every row here is flown at: 0.25 km asks for more
        // pixels than `LONG_RANGE_SIDE` offers at all three extents, so the
        // ceiling is what binds and this is the same assertion it always was.
        let side = types::raster_side_px(extent_km, LONG_RANGE_SIDE, 0.25);
        let px_per_km = side as f64 / (2.0 * extent_km);
        assert!(
            px_per_km >= floor * (1.0 - TOLERANCE),
            "{why}: {extent_km} km on {side} px is {px_per_km:.4} px/km, more \
             than {:.1}% under the floor's {floor:.4}",
            TOLERANCE * 100.0,
        );
        // And that the base raster is what it would have been *without* the
        // second number, so the comparison above is against a real
        // alternative rather than a straw one.
        let unadapted = types::IMAGE_SIZE as f64 / (2.0 * extent_km);
        assert!(
            unadapted < floor,
            "{why}: the base raster would be {unadapted:.4} px/km, which is \
             not coarser than the floor's {floor:.4} — this row is measuring \
             nothing",
        );
    }

    // The widest real sweep, in the unit that decides whether a gate is drawn or
    // shared: pixels across one 250 m super-resolution gate.
    const SURVEILLANCE_KM: f64 = 460.125;
    const GATE_KM: f64 = 0.25;
    let px_per_gate = |side: usize| side as f64 / (2.0 * SURVEILLANCE_KM) * GATE_KM;
    assert!(
        px_per_gate(types::IMAGE_SIZE) < 0.6,
        "a 250 m gate on the base raster is {:.3} px across",
        px_per_gate(types::IMAGE_SIZE),
    );
    assert!(
        px_per_gate(LONG_RANGE_SIDE) > 1.0,
        "a 250 m gate on the long-range raster is {:.3} px across",
        px_per_gate(LONG_RANGE_SIDE),
    );

    // The cap: the extent where the long-range raster is furthest under the
    // floor, by the 2.1% the table names and no more.
    let at_cap = types::raster_side_px(types::MAX_EXTENT_KM, LONG_RANGE_SIDE, 0.25) as f64
        / (2.0 * types::MAX_EXTENT_KM);
    assert!(
        (at_cap / floor - 0.9787).abs() < 1e-3,
        "at the {} km cap the raster is {at_cap:.4} px/km, {:.4} of the \
         floor — the table above says 0.9787",
        types::MAX_EXTENT_KM,
        at_cap / floor,
    );
}

const DOPPLER_GATES: u16 = 1192;
/// See [`DOPPLER_GATES`].
const DOPPLER_GATE_M: u16 = 250;
/// See [`DOPPLER_GATES`].
const DOPPLER_FIRST_GATE_M: u16 = 2125;

fn doppler_ground_reach_km() -> f64 {
    // Widened before multiplying: 1192 × 250 is 298 000, and the three
    // constants are the `u16` fields `MomentData` stores them in.
    let slant_km = (f64::from(DOPPLER_FIRST_GATE_M)
        + f64::from(DOPPLER_GATES) * f64::from(DOPPLER_GATE_M))
        / 1000.0;
    crate::beam::ground_range_km(slant_km, f64::from(L2_ELEVATION))
}

/// A filled Doppler cut: every gate of every radial carries a velocity.
fn wsr88d_doppler_sweep() -> Scan {
    use nexrad_model::data::{MomentData, PulseWidth, RadialStatus, Sweep, VolumeCoveragePattern};

    let radials = (0..360)
        .map(|i| {
            Radial::new(
                0,
                i as u16,
                i as f32,
                1.0,
                RadialStatus::IntermediateRadialData,
                1,
                L2_ELEVATION,
                None,
                Some(MomentData::from_fixed_point(
                    DOPPLER_GATES,
                    DOPPLER_FIRST_GATE_M,
                    DOPPLER_GATE_M,
                    8,
                    SCALE,
                    OFFSET,
                    vec![200; DOPPLER_GATES as usize],
                )),
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

#[test]
fn a_base_size_ceiling_pays_for_the_extra_ground_in_scale() {
    let scan = wsr88d_doppler_sweep();
    let render_at = |ceiling| {
        render_radar_to_image_full_sized(
            &scan,
            L2_ELEVATION,
            types::RadarProduct::Velocity,
            LAT,
            LON,
            crate::srv::MotionInputs::default(),
            None,
            None,
            &crate::nyquist::DeclaredNyquist::empty(),
            ceiling,
        )
        .expect("the fixture renders")
    };
    let lean = render_at(types::IMAGE_SIZE);
    let wide = render_at(LONG_RANGE_SIDE);

    assert_eq!(
        lean.max_range_km, wide.max_range_km,
        "a device's texture ceiling moved how much ground the picture shows",
    );
    let extent_km = lean.max_range_km;
    assert!(
        (extent_km - doppler_ground_reach_km()).abs() < 1e-9,
        "1192 gates of 0.25 km from 2.125 km out cover {:.4} km of ground; the \
         render declares {extent_km}",
        doppler_ground_reach_km(),
    );
    assert!(
        extent_km > types::BASE_EXTENT_KM,
        "a Doppler cut stopping at {extent_km:.2} km would be inside the floor, \
         and this whole test would be measuring the floor's own render",
    );

    assert_eq!(lean.image.len(), types::IMAGE_SIZE * types::IMAGE_SIZE * 4);
    assert_eq!(wide.image.len(), LONG_RANGE_SIDE * LONG_RANGE_SIDE * 4);

    let floor = types::IMAGE_SIZE as f64 / (2.0 * types::BASE_EXTENT_KM);
    let px_per_km = |side: usize| side as f64 / (2.0 * extent_km);
    for (side, expected, what) in [
        // 3.412 and 6.824 before the ground range became the spherical arc: the extent
        // these divide is the sweep's ground reach, and the arc is shorter than the
        // tangent plane's `r·cos e`, so the same pixels cover slightly less ground and
        // the scale rises.
        (types::IMAGE_SIZE, 3.4145, "a base-size ceiling"),
        (LONG_RANGE_SIDE, 6.8290, "a long-range ceiling"),
    ] {
        assert!(
            (px_per_km(side) - expected).abs() < 1e-3,
            "{what}: {:.4} px/km, not the {expected} written down",
            px_per_km(side),
        );
    }
    assert!(
        (px_per_km(types::IMAGE_SIZE) / floor - 0.7664).abs() < 1e-3,
        "the base-size arm is {:.4} of the floor's {floor:.4} px/km — 0.7664 is \
         written down, and it is the entire cost of this trade",
        px_per_km(types::IMAGE_SIZE) / floor,
    );

    // The ground the wider frame bought is carrying data, or the paragraph
    // above is arguing about an empty annulus: a gate 290 km out paints, and on
    // the floor's 230 km frame it would not be on the picture at all.
    const FAR_KM: f64 = 290.0;
    for az in [0.0, 90.0, 180.0, 270.0] {
        let at = probe_at(extent_km, types::IMAGE_SIZE, az, FAR_KM);
        assert!(
            !lean.values[at].is_nan(),
            "a gate {FAR_KM} km out at {az}° is unpainted on the base-size \
             raster, {:.0} km past the floor this render is here to be outside",
            FAR_KM - types::BASE_EXTENT_KM,
        );
    }

    const RESOLUTION_LINE: f64 = 2.0;
    for (extent, expected, why) in [
        (extent_km, 3.4145, "this Doppler cut"),
        (460.125, 2.2255, "a WSR-88D surveillance cut"),
        (types::MAX_EXTENT_KM, 2.1787, "the arithmetic cap"),
    ] {
        let side = types::raster_side_px(extent, types::IMAGE_SIZE, 0.25);
        let scale = side as f64 / (2.0 * extent);
        assert!(
            (scale - expected).abs() < 1e-3,
            "{why} on a base-size ceiling is {scale:.4} px/km, not {expected}",
        );
        assert!(
            scale > RESOLUTION_LINE,
            "{why} is {scale:.4} px/km, under the {RESOLUTION_LINE} px/km a \
             250 m gate needs to land in a pixel of its own",
        );
    }
}

#[test]
fn a_ceiling_under_the_base_size_renders_a_leaner_picture_of_the_same_ground() {
    const LEAN: usize = 1024;
    let scan = tdwr_long_range_sweep(400.2);
    let SweepRender {
        image,
        max_range_km: extent_km,
        values,
        ..
    } = render_radar_to_image_full_sized(
        &scan,
        L2_ELEVATION,
        PRODUCT,
        LAT,
        LON,
        crate::srv::MotionInputs::default(),
        None,
        None,
        &crate::nyquist::DeclaredNyquist::empty(),
        LEAN,
    )
    .expect("the fixture renders");

    assert_eq!(image.len(), LEAN * LEAN * 4);
    assert_eq!(values.len(), LEAN * LEAN);
    assert!(
        (extent_km - tdwr_ground_reach_km()).abs() < 1e-9,
        "a leaner raster covers the same ground: {extent_km}",
    );
    let at = probe_at(extent_km, LEAN, 90.0, 400.2);
    assert!(
        !values[at].is_nan(),
        "the beacon is unpainted on the lean raster",
    );
}

// ── Range folding and the declared Nyquist ───────────────────────────────

#[test]
fn a_range_folded_gate_paints_the_dedicated_purple_and_below_threshold_does_not() {
    const FOLDED_AZ: usize = 90;
    const BELOW_AZ: usize = 91;
    let azimuths: Vec<f32> = (0..360).map(|i| i as f32).collect();
    let mut gates = vec![200u8; 360];
    gates[FOLDED_AZ] = RAW_RANGE_FOLDED;
    gates[BELOW_AZ] = RAW_BELOW_THRESHOLD;
    let scan = l2_sweep(&gates, &azimuths, 1.0, true);

    let SweepRender { image, values, .. } =
        render_radar_to_image(&scan, L2_ELEVATION, types::RadarProduct::Velocity, LAT, LON)
            .expect("the fixture renders");

    let folded = probe_at(
        l2_ground_reach_km(),
        types::IMAGE_SIZE,
        FOLDED_AZ as f64,
        50.0,
    );
    assert_eq!(
        pixel_at(&image, folded),
        crate::palette::RANGE_FOLDED,
        "a range-folded gate is the dedicated purple",
    );
    assert!(
        values[folded].is_nan(),
        "the exported grid carries a plain NaN over a folded gate, not a \
         payload — nothing across the JS boundary reads one",
    );
    assert_eq!(
        values[folded].to_bits(),
        f32::NAN.to_bits(),
        "and it is the canonical NaN, not the sentinel the cell held",
    );

    let below = probe_at(
        l2_ground_reach_km(),
        types::IMAGE_SIZE,
        BELOW_AZ as f64,
        50.0,
    );
    assert_eq!(
        pixel_at(&image, below).3,
        0,
        "a below-threshold gate stays transparent",
    );
    assert!(values[below].is_nan());

    // The precondition: the rest of the sweep really did paint, or the two
    // assertions above are about a blank picture.
    let neighbour = probe_at(l2_ground_reach_km(), types::IMAGE_SIZE, 80.0, 50.0);
    assert!(
        !values[neighbour].is_nan() && pixel_at(&image, neighbour) != crate::palette::RANGE_FOLDED,
        "an ordinary radial paints its own colour",
    );
}

#[test]
fn a_sweep_with_nothing_folded_paints_no_range_folded_pixel() {
    let azimuths: Vec<f32> = (0..360).map(|i| i as f32).collect();
    let scan = l2_sweep(&vec![200u8; 360], &azimuths, 1.0, true);
    let SweepRender { image, .. } =
        render_radar_to_image(&scan, L2_ELEVATION, types::RadarProduct::Velocity, LAT, LON)
            .expect("the fixture renders");
    assert!(
        !image
            .chunks_exact(4)
            .any(|px| (px[0], px[1], px[2], px[3]) == crate::palette::RANGE_FOLDED),
        "the folded colour appeared with nothing folded",
    );
}

#[test]
fn the_render_reports_the_declared_nyquist_of_the_sweep_it_drew() {
    let declared: crate::nyquist::DeclaredNyquist =
        [(1u8, 22.4), (2u8, 31.05)].into_iter().collect();
    let scan = two_tilt_velocity_scan();

    let low = render_radar_to_image_full(
        &scan,
        0.5,
        types::RadarProduct::Velocity,
        LAT,
        LON,
        crate::srv::MotionInputs::default(),
        None,
        None,
        &declared,
    )
    .expect("the low tilt renders");
    assert_eq!(low.nyquist_ms, Some(22.4));

    let high = render_radar_to_image_full(
        &scan,
        1.5,
        types::RadarProduct::Velocity,
        LAT,
        LON,
        crate::srv::MotionInputs::default(),
        None,
        None,
        &declared,
    )
    .expect("the high tilt renders");
    assert_eq!(high.nyquist_ms, Some(31.05));

    // A volume that declared nothing for the cut reports nothing, which is the
    // Message 1 case and every fixture's: the dealiaser then estimates, as it
    // always did.
    let partial: crate::nyquist::DeclaredNyquist = [(2u8, 31.05)].into_iter().collect();
    let unnamed = render_radar_to_image_full(
        &scan,
        0.5,
        types::RadarProduct::Velocity,
        LAT,
        LON,
        crate::srv::MotionInputs::default(),
        None,
        None,
        &partial,
    )
    .expect("the low tilt renders");
    assert_eq!(
        unnamed.nyquist_ms, None,
        "an entry for another cut is not this cut's",
    );

    let volume = render_radar_to_image_full(
        &scan,
        0.5,
        types::RadarProduct::EchoTopsInterpolated,
        LAT,
        LON,
        crate::srv::MotionInputs::default(),
        None,
        None,
        &declared,
    )
    .expect("echo tops render");
    assert_eq!(volume.nyquist_ms, None);
}

fn two_tilt_velocity_scan() -> Scan {
    use nexrad_model::data::{MomentData, PulseWidth, RadialStatus, Sweep, VolumeCoveragePattern};
    let tilt = |elevation_number: u8, elevation_deg: f32| {
        let radials = (0..360)
            .map(|i| {
                Radial::new(
                    0,
                    i as u16,
                    i as f32,
                    1.0,
                    RadialStatus::IntermediateRadialData,
                    elevation_number,
                    elevation_deg,
                    Some(MomentData::from_fixed_point(
                        600,
                        0,
                        250,
                        8,
                        SCALE,
                        OFFSET,
                        vec![200; 600],
                    )),
                    Some(MomentData::from_fixed_point(
                        600,
                        0,
                        250,
                        8,
                        2.0,
                        129.0,
                        vec![200; 600],
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
    };
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
        vec![tilt(1, 0.5), tilt(2, 1.5)],
    )
}

// ── the output pool's two slots ──────────────────────────────────────────────

/// What a *used* buffer looks like.
const POISON: u8 = 0xA5;

#[test]
fn a_checked_out_texture_is_zero_at_every_length() {
    let want = 4096;
    for held in [0, 1, want / 2, want - 1, want, want + 1, want * 3] {
        super::recycle_image(vec![POISON; held]);
        let image = super::checkout_image(want);
        assert_eq!(
            image.len(),
            want,
            "a checkout asked for {want} bytes came back with {} after the slot held {held}",
            image.len()
        );
        assert!(
            image.iter().all(|&b| b == 0),
            "a checkout after the slot held {held} bytes came back with {} non-zero bytes of {want}",
            image.iter().filter(|&&b| b != 0).count()
        );
    }
    let _ = super::image_pool().take();
    let image = super::checkout_image(want);
    assert_eq!(image.len(), want);
    assert!(image.iter().all(|&b| b == 0));
}

#[test]
fn a_checked_out_value_grid_is_empty_at_every_length() {
    for held in [0usize, 1, 1024, 4096, 1 << 20] {
        super::recycle_values(vec![f32::from_bits(0xDEAD_BEEF); held]);
        let values = super::checkout_values();
        assert_eq!(
            values.len(),
            0,
            "a checkout came back holding {} values after the slot held {held}",
            values.len()
        );
    }
}

// What `recycle_image` and `recycle_values` do *to the slot* — keep the first offer,
// drop the rest, decline a buffer with no capacity — is pinned in
// `tests/render_output_slot.rs` and deliberately not here.

#[test]
fn the_rasterizer_places_a_gate_where_the_beam_module_says_it_is() {
    let mut worst_px = 0.0f64;
    let mut worst_at = (0.0, 0.0, 0.0);
    for site_lat in [27.784, LAT, 47.0411, 48.1946] {
        for extent_km in [88.8, 150.0, 230.0, 416.98, 460.11] {
            let bounds = types::ImageBounds::from_radar_site(site_lat, LON, extent_km);
            let side = types::IMAGE_SIZE;
            let proj = MercatorProjection::from_bounds(site_lat, &bounds, extent_km, side);
            let merc_span = bounds.mercator_y_max - bounds.mercator_y_min;
            for i in 0..720 {
                let az = f64::from(i) / 2.0;
                for frac in [0.01, 0.25, 0.5, 0.75, 1.0] {
                    let range_km = extent_km * frac;

                    let (sin_az, cos_az) = az.to_radians().sin_cos();
                    let (sin_d, cos_d) = (range_km / rustdar_geo::EARTH_RADIUS_KM).sin_cos();
                    let (px, py) = proj.pixel_at(sin_az, cos_az, sin_d, cos_d);

                    let (lat, lon) =
                        rustdar_geo::great_circle_destination(site_lat, LON, az, range_km);
                    let want_px =
                        (lon - bounds.min_lon) / (bounds.max_lon - bounds.min_lon) * side as f64;
                    let merc_y = rustdar_geo::lat_rad_to_mercator_y(lat.to_radians());
                    let want_py = (bounds.mercator_y_max - merc_y) / merc_span * side as f64;

                    let off = (px - want_px).abs().max((py - want_py).abs());
                    if off > worst_px {
                        worst_px = off;
                        worst_at = (site_lat, az, range_km);
                    }
                }
            }
        }
    }
    let (lat, az, range) = worst_at;
    assert!(
        worst_px < 1e-3,
        "the rasterizer and `beam` disagree by {worst_px:.3e} px, worst at \
         {lat}\u{b0}N / {az}\u{b0} / {range:.1} km",
    );
}

#[test]
fn a_cut_whose_leading_radial_is_blank_is_still_offered_to_the_loop() {
    let intact = scan_of(vec![
        settling_sweep(1, 0.68, 0.44, false),
        settling_sweep(2, 0.30, 0.84, false),
    ]);
    // The 0.4° cut with its first radial's reflectivity stripped, and nothing
    // else touched.
    let maimed = scan_of(
        intact
            .sweeps()
            .iter()
            .enumerate()
            .map(|(s, sweep)| {
                let radials: Vec<Radial> = sweep
                    .radials()
                    .iter()
                    .enumerate()
                    .map(|(i, r)| {
                        Radial::new(
                            r.collection_timestamp(),
                            r.azimuth_number(),
                            r.azimuth_angle_degrees(),
                            r.azimuth_spacing_degrees(),
                            r.radial_status(),
                            r.elevation_number(),
                            r.elevation_angle_degrees(),
                            (!(s == 0 && i == 0))
                                .then(|| r.reflectivity().cloned())
                                .flatten(),
                            r.velocity().cloned(),
                            None,
                            None,
                            None,
                            None,
                            None,
                        )
                    })
                    .collect();
                nexrad_model::data::Sweep::new(sweep.elevation_number(), radials)
            })
            .collect(),
    );
    assert!(
        maimed.sweeps()[0].radials()[0].reflectivity().is_none(),
        "the fixture must be blank in front",
    );
    assert!(
        maimed.sweeps()[0].radials()[1].reflectivity().is_some(),
        "and only in front",
    );

    assert_eq!(
        find_closest_elevation(&maimed, PRODUCT, 0.5),
        Some(0.4),
        "one blank leading radial hid the 0.4\u{b0} cut from the loop's snap",
    );
    assert_eq!(find_closest_elevation(&maimed, PRODUCT, 0.8), Some(0.8));
}

fn split_volume_with_stray_velocity(stray: Option<usize>) -> Scan {
    use nexrad_model::data::{MomentData, PulseWidth, RadialStatus, Sweep, VolumeCoveragePattern};

    let refl = |gates: usize, interval_km: f64| {
        MomentData::from_fixed_point(
            gates as u16,
            0,
            (interval_km * 1000.0) as u16,
            8,
            SCALE,
            OFFSET,
            vec![200; gates],
        )
    };
    let vel = || {
        MomentData::from_fixed_point(
            TDWR_DOPPLER_GATES as u16,
            0,
            (TDWR_DOPPLER_GATE_KM * 1000.0) as u16,
            8,
            2.0,
            129.0,
            vec![200; TDWR_DOPPLER_GATES],
        )
    };
    let half = |number: u8, gates: usize, interval_km: f64, velocity: &dyn Fn(usize) -> bool| {
        let radials = (0..360)
            .map(|i| {
                Radial::new(
                    0,
                    i as u16,
                    i as f32,
                    1.0,
                    RadialStatus::IntermediateRadialData,
                    number,
                    L2_ELEVATION,
                    Some(refl(gates, interval_km)),
                    velocity(i).then(vel),
                    None,
                    None,
                    None,
                    None,
                    None,
                )
            })
            .collect();
        Sweep::new(number, radials)
    };

    Scan::new(
        VolumeCoveragePattern::new(
            80,
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
        vec![
            half(1, TDWR_GATES, TDWR_GATE_KM, &|i| stray == Some(i)),
            half(2, TDWR_DOPPLER_GATES, TDWR_DOPPLER_GATE_KM, &|_| true),
        ],
    )
}

#[test]
fn one_stray_velocity_radial_does_not_reframe_the_reflectivity_pane() {
    let long_range_km =
        crate::beam::ground_range_km(TDWR_GATES as f64 * TDWR_GATE_KM, f64::from(L2_ELEVATION));

    for stray in [None, Some(0usize), Some(200)] {
        let scan = split_volume_with_stray_velocity(stray);
        let owner = find_sweep_owner(&scan, types::RadarProduct::Reflectivity, L2_ELEVATION)
            .expect("both halves carry reflectivity");
        assert_eq!(
            owner.elevation_number(),
            1,
            "stray velocity radial {stray:?} hid the surveillance half",
        );

        let render = render_radar_to_image(
            &scan,
            L2_ELEVATION,
            types::RadarProduct::Reflectivity,
            LAT,
            LON,
        )
        .expect("the surveillance half renders");
        assert!(
            (render.max_range_km - long_range_km).abs() < 1e-9,
            "stray velocity radial {stray:?} reframed the pane at {} km \
             instead of {long_range_km:.5} km",
            render.max_range_km,
        );
    }

    let scan = split_volume_with_stray_velocity(Some(200));
    let vel = render_radar_to_image(&scan, L2_ELEVATION, types::RadarProduct::Velocity, LAT, LON)
        .expect("the Doppler half carries velocity");
    let doppler_km = crate::beam::ground_range_km(
        TDWR_DOPPLER_GATES as f64 * TDWR_DOPPLER_GATE_KM,
        f64::from(L2_ELEVATION),
    );
    assert!((vel.max_range_km - doppler_km).abs() < 1e-9);
}

// ── The polar field against the raster it was painted beside ────────────────

#[test]
fn the_polar_field_answers_what_the_value_grid_holds() {
    let out = render_level3_radial_to_image(
        &packet(None),
        PRODUCT,
        LAT,
        LON,
        SCALE,
        OFFSET,
        None,
        types::IMAGE_SIZE,
    )
    .unwrap();
    let extent = out.max_range_km;
    let geom = out.polar.geometry().clone();
    assert_eq!(geom.radials(), N_RADIALS);
    assert_eq!(geom.gates(), N_BINS);

    // ── At the centres: exact, every time ──
    let coarse = render_level3_radial_with_gate_km(
        &packet(None),
        1.0,
        PRODUCT,
        LAT,
        LON,
        SCALE,
        OFFSET,
        None,
        LONG_RANGE_SIDE,
    )
    .unwrap();
    let cgeom = coarse.polar.geometry().clone();
    let cside = (coarse.image.len() / 4).isqrt();
    let mut centres = 0u32;
    let mut pinholes = 0u32;
    for radial in (0..cgeom.radials()).step_by(7) {
        let az = f64::from(cgeom.wedges()[radial].azimuth_deg);
        for gate in (50..cgeom.gates()).step_by(11) {
            // `gate_ground_km`, not `first + gate × interval`: the slant grid
            // is uniform and the ground grid it projects to is not, so the
            // straight line this used to be would probe a range the gate does
            // not cover.
            let km = cgeom.gate_ground_km(gate);
            if km > coarse.max_range_km {
                break;
            }
            let picked = cgeom.pick(az, km).expect("a centre is inside its own gate");
            assert_eq!(
                picked,
                super::polar::GateAt { radial, gate },
                "({az}°, {km} km) is the centre of ({radial}, {gate})"
            );
            centres += 1;
            let from_grid = coarse.values[probe_at(coarse.max_range_km, cside, az, km)];
            if from_grid.is_nan() {
                pinholes += 1;
                continue;
            }
            assert_eq!(
                coarse.polar.at(picked).unwrap().to_bits(),
                from_grid.to_bits(),
                "({az}°, {km} km): the grid and the gate must be the same number"
            );
        }
    }
    assert!(centres > 1500, "only {centres} centre probes");
    assert_eq!(
        pinholes, 0,
        "{pinholes} of {centres} cell centres are unpainted in the raster, under \
         a gate the polar field holds a value for"
    );

    // ── Away from them: one cell wide, and rare ──
    let mut probes = 0u32;
    let mut disagreed = 0u32;
    let mut adjacent = 0u32;
    let mut az = 0.13f64;
    while az < 360.0 {
        let mut km = 0.05f64;
        while km < 150.0 {
            let from_grid = out.values[probe_at(extent, types::IMAGE_SIZE, az, km)];
            let picked = geom.pick(az, km);
            let from_gate = picked.and_then(|a| out.polar.at(a));
            probes += 1;
            let agreed = match (from_grid.is_nan(), from_gate) {
                (true, None) => true,
                (false, Some(v)) => v == from_grid,
                _ => false,
            };
            if !agreed {
                disagreed += 1;
                if let Some(a) = picked
                    && !from_grid.is_nan()
                    && neighbours(&geom, a)
                        .into_iter()
                        .any(|n| out.polar.at(n) == Some(from_grid))
                {
                    adjacent += 1;
                }
            }
            km *= 1.03;
        }
        az += 0.37;
    }

    assert!(
        disagreed * 100 < probes * 3,
        "{disagreed} of {probes} probes disagree — over 3%, which is a rule that \
         has moved rather than a raster that is quantizing"
    );
    assert!(
        adjacent * 1000 >= disagreed * 990,
        "only {adjacent} of {disagreed} disagreements are an adjacent cell; the \
         rest are not quantization"
    );
}

/// The eight gates around `at`, wrapping in azimuth because a sweep closes.
fn neighbours(
    geom: &super::polar::PolarGeometry,
    at: super::polar::GateAt,
) -> Vec<super::polar::GateAt> {
    let n = geom.radials();
    let mut out = Vec::with_capacity(8);
    for dr in [-1i64, 0, 1] {
        for dg in [-1i64, 0, 1] {
            if dr == 0 && dg == 0 {
                continue;
            }
            let radial = (at.radial as i64 + dr).rem_euclid(n as i64) as usize;
            let Ok(gate) = usize::try_from(at.gate as i64 + dg) else {
                continue;
            };
            out.push(super::polar::GateAt { radial, gate });
        }
    }
    out
}

#[test]
fn consecutive_gates_tile_with_no_seam() {
    const ELEV_DEG: f64 = 19.5;
    const FIRST_KM: f64 = 2.125;
    const INTERVAL_KM: f64 = 0.25;
    const GATES: usize = 400;

    let spans: Vec<GateSpan> = gate_ground_edges(FIRST_KM, INTERVAL_KM, GATES, |slant_km| {
        crate::beam::ground_range_km(slant_km, ELEV_DEG)
    })
    .collect();
    assert_eq!(spans.len(), GATES);

    for (j, pair) in spans.windows(2).enumerate() {
        assert_eq!(
            pair[0].far_km.to_bits(),
            pair[1].near_km.to_bits(),
            "gate {j} ends at {} but gate {} starts at {} — the shared boundary \
             was evaluated twice instead of once",
            pair[0].far_km,
            j + 1,
            pair[1].near_km,
        );
    }
    for (j, span) in spans.iter().enumerate() {
        assert!(
            span.far_km > span.near_km,
            "gate {j} has no depth: {span:?}",
        );
    }

    let ground = |slant_km: f64| crate::beam::ground_range_km(slant_km, ELEV_DEG);
    let flat_depth = INTERVAL_KM * ELEV_DEG.to_radians().cos();
    let worst = (0..GATES - 1)
        .map(|j| {
            let centre = ground(FIRST_KM + j as f64 * INTERVAL_KM);
            let next = ground(FIRST_KM + (j + 1) as f64 * INTERVAL_KM);
            ((centre + flat_depth / 2.0) - (next - flat_depth / 2.0)).abs()
        })
        .fold(0.0f64, f64::max);
    assert!(
        (worst - 0.001_903).abs() < 1e-5,
        "the centre-plus-depth seam moved: {:.4} m, documented as 1.903 m",
        worst * 1000.0,
    );
}
