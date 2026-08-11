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

/// A spatially coherent reflectivity field — storm cores placed in (x, y)
/// rather than a per-radial pattern, so neighbouring radials agree about
/// as much as real ones do. `silence` drops one radial without renumbering
/// the rest, which [`overlapping_radials_contend_for_pixels`] needs.
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
    let (image, _, values) =
        render_level3_radial_to_image(p, PRODUCT, LAT, LON, SCALE, OFFSET, None).unwrap();
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

/// The fixture has to actually paint, or everything below passes vacuously.
#[test]
fn fixture_covers_a_realistic_share_of_the_image() {
    let (image, values) = render(&packet(None));
    let painted = image.chunks_exact(4).filter(|px| px[3] != 0).count();
    let disc = std::f64::consts::PI * (N_BINS as f64 * 0.25 * types::PIXELS_PER_KM).powi(2);
    assert!(
        (painted as f64) > disc * 0.9 && (painted as f64) < disc * 1.1,
        "painted {painted}, expected about {disc:.0} for a {N_BINS}-gate disc"
    );
    assert!(values.iter().any(|v| !v.is_nan()));
}

/// Every value has to survive the trip through the cell exactly. The key
/// shares those 64 bits, so anything that lets it reach the low half shows
/// up here as a value the packet could not have encoded.
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

/// Pins the *direction* of the tie-break, not just its stability. Two
/// adjacent radials, the earlier one deliberately carrying the **larger**
/// value: wherever both reach a pixel the later radial must take it, purely
/// because it is later. Ranking by anything else — value, gate index, a
/// constant key — hands some of those pixels to radial 0 instead.
///
/// `both`, `only_first` and `only_second` are the value grids with both
/// radials, with the second silenced, and with the first silenced.
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
    // Radial 0 carries the larger value on purpose.
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

/// Guards the premise of the two determinism tests: radials really do land
/// on each other's pixels, so a racy rasterizer would have something to
/// race over. Silencing radial `k` hands every pixel it owned to whichever
/// lower-keyed radial also wrote there, so a pixel painted both times but
/// holding different values is one that two radials contended for.
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

/// The property this module exists to pin: ten renders of one sweep across
/// the whole rayon pool agree byte for byte.
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

/// Stability alone would let the parallel path settle on an answer of its
/// own. It has to settle on the sequential one.
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

/// Colour and value come out of one cell, so they cannot come from
/// different gates. Two separate buffers used to let them.
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
//
// These paths build their own keys and hand their own product to
// `RenderBuffers`, and none of the Level III tests above reach them.

const L2_ELEVATION: f32 = 0.5;

/// A one-sweep Level II scan of `gates.len()` radials sitting at `azimuths`,
/// each declaring `declared_spacing` as its own azimuth resolution. A radial
/// whose byte is 0 decodes as below-threshold and is skipped, which silences
/// it without renumbering the rest.
///
/// Where a radial sits and how wide it says it is are separate arguments
/// because the renderer now reads them separately, and every way they can
/// disagree is a case: sparser than declared is the sweep with radials
/// missing, denser is the lying declaration, and a `declared_spacing` of 0 is
/// a sweep that declares nothing at all.
fn l2_sweep(gates: &[u8], azimuths: &[f32], declared_spacing: f32, velocity: bool) -> Scan {
    use nexrad_model::data::{MomentData, PulseWidth, RadialStatus, Sweep, VolumeCoveragePattern};
    assert_eq!(gates.len(), azimuths.len(), "one gate byte per radial");
    let radials = gates
        .iter()
        .zip(azimuths)
        .enumerate()
        .map(|(i, (&byte, &azimuth))| {
            let moment =
                MomentData::from_fixed_point(600, 0, 250, 8, SCALE, OFFSET, vec![byte; 600]);
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

/// [`l2_sweep`] with the shape the tie-break tests were written against: 1°
/// apart from 90°, declaring the 1° they are apart by.
fn l2_scan(gates: &[u8], velocity: bool) -> Scan {
    let azimuths: Vec<f32> = (0..gates.len()).map(|i| 90.0 + i as f32).collect();
    l2_sweep(gates, &azimuths, 1.0, velocity)
}

fn render_l2(gates: &[u8], product: types::RadarProduct) -> (Vec<u8>, Vec<f32>) {
    let scan = l2_scan(gates, product != types::RadarProduct::Reflectivity);
    let (image, _, values) = render_radar_to_image(&scan, L2_ELEVATION, product, LAT, LON).unwrap();
    (image, values)
}

/// Which pixel a point `range_km` out at `az_deg` from the site lands in,
/// through the same [`MercatorProjection`] the renderer paints with.
///
/// A duplicate of the arithmetic in `render_gate` rather than a call into it,
/// because `render_gate` writes and this asks. Both walk azimuth → offset →
/// Mercator → truncated pixel, and the truncation is why a probe has to go
/// through the projection at all: a hand-computed pixel would be off by one
/// somewhere and the difference between "unpainted" and "off by one" is the
/// whole point of these tests. Probes are kept well inside 230 km so the
/// answer does not depend on the raster's extent.
fn probe(az_deg: f64, range_km: f64) -> usize {
    let bounds = types::ImageBounds::from_radar_site(LAT, LON);
    let proj = MercatorProjection::from_bounds(LAT, &bounds);
    let (sin_az, cos_az) = az_deg.to_radians().sin_cos();
    let dest_lat_rad = proj.radar_lat_rad + (range_km * cos_az) / types::EARTH_RADIUS_KM;
    let cos_correction = proj.cos_radar_lat / dest_lat_rad.cos();
    let px = (proj.center_px + range_km * sin_az * cos_correction * types::PIXELS_PER_KM) as usize;
    let py = ((proj.merc_y_top - types::lat_rad_to_mercator_y(dest_lat_rad)) * proj.merc_y_scale)
        as usize;
    py * types::IMAGE_SIZE + px
}

/// Assert what the sweep painted at a list of `(azimuth, range)` probes.
fn assert_probes(values: &[f32], painted: bool, probes: &[(f64, f64)], why: &str) {
    for &(az, range) in probes {
        let v = values[probe(az, range)];
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

/// A velocity field with enough azimuthal shear to survive the LLSD fit,
/// the range normalization, and the ±0.25 display threshold, so
/// `render_nrot_to_image` actually paints.
fn nrot_scan(n_radials: usize) -> Scan {
    nrot_sector(n_radials, 360.0 / n_radials as f32)
}

/// [`nrot_scan`] over an arc rather than the whole circle: `n_radials` spaced
/// `step_deg` apart from 0°, so `n_radials · step_deg` of sky is covered and
/// the rest of the circle is a hole.
///
/// The shear is a function of the radial *index*, not of the azimuth, so a
/// sector carries the same shear per radial that the full circle does and the
/// LLSD fit has the same fixture to work with either way.
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

/// NROT hands `RenderBuffers` its own product literal, far from where
/// `into_output` applies it. Rendering NROT through the reflectivity
/// palette would look plausible and fail nothing else.
#[test]
fn nrot_colour_comes_from_the_nrot_palette() {
    let scan = nrot_scan(360);
    let (image, _, values) = render_radar_to_image(
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

/// The NROT grid is indexed (azimuth, gate) like the others, and its key
/// has to agree.
///
/// Known gap: transposing this path's [`GateId`] survives the suite. The
/// L2 and L3 equivalents die to their `later_radial_wins` tests, which need
/// two adjacent radials carrying known, very different values — NROT has no
/// such handle, since every value is an LLSD fit over its neighbours and
/// the median filter deletes anything isolated enough to control. The
/// named fields are the mitigation: a transposition there has to be
/// written out in full rather than slipped in as argument order.
#[test]
fn nrot_render_is_deterministic() {
    let scan = nrot_scan(360);
    let once = || {
        let (image, _, values) = render_radar_to_image(
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
//
// Every probe here sits at 50 km, comfortably inside the raster whatever
// extent it is projected at, and every fixture is built so that the
// difference between a radial's declared width and the distance to its
// neighbour is the only thing under test.

/// A sweep with radials missing leaves the sky between them **empty**. Four
/// radials 90° apart declaring the 0.5° a super-res cut is: the sweep covers
/// two degrees of sky in total and 358 degrees of nothing.
///
/// This is the shape TDWR arrives in when the decoder loses radials, and the
/// failure it used to produce is not a subtle one: painting each survivor at
/// the mean distance to the next drew four 90°-wide wedges, and a wedge is
/// built from a chord rather than an arc, so the display filled with four
/// enormous triangles meeting at the site.
#[test]
fn a_sparse_sweep_leaves_holes_not_chord_triangles() {
    let scan = l2_sweep(&[200; 4], &[0.0, 90.0, 180.0, 270.0], 0.5, false);
    let (_, _, values) = render_radar_to_image(&scan, L2_ELEVATION, PRODUCT, LAT, LON).unwrap();

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

/// Collection order is where the antenna happened to start and which way it
/// happened to turn, and it is not data. A sweep handed over descending must
/// paint the same sky as the same sweep handed over ascending.
///
/// The masks are compared and the values are not, deliberately. Where two
/// wedges quantize onto one pixel the winner is the later *radial*, and
/// reversing the order reverses which of the two that is — so the values
/// legitimately differ at the seams while the painted region does not.
///
/// The old signed mean made this a hollowed-out display rather than a
/// shifted one: descending 1° radials averaged −1° a radial, and
/// `render_gate` derives its azimuth sample count from that width, so beyond
/// the range where the count first goes negative — about 26 km at 2048² —
/// the loop ran zero times and every radial stopped dead. 34 k pixels of the
/// 1.4 M this sweep covers, silently, with nothing in the log.
#[test]
fn an_out_of_order_sweep_still_paints() {
    let azimuths: Vec<f32> = (0..360).map(|i| i as f32).collect();
    // Varying bytes, so a pixel two radials contend for really does hold a
    // different value depending on which of them won it.
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
            .2
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

    // And the reason the masks are what is compared: the values are not
    // equal, and are not expected to be.
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

/// The width comes from what the radial declares, not from where the next
/// radial is. Two sweeps of radials 2° apart, one declaring the 2° it is
/// spaced by and one declaring 0.5°: the first tiles, the second leaves
/// three quarters of the sky between its wedges unpainted — which is the
/// honest answer, since 0.5° is all the sky the radar says each sample
/// covers.
#[test]
fn declared_spacing_drives_the_wedge_width() {
    let azimuths: Vec<f32> = (0..180).map(|i| i as f32 * 2.0).collect();
    let render = |declared: f32| {
        let scan = l2_sweep(&[200; 180], &azimuths, declared, false);
        render_radar_to_image(&scan, L2_ELEVATION, PRODUCT, LAT, LON)
            .unwrap()
            .2
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

/// A declaration is believed, not obeyed. A 200° arc of radials really 0.5°
/// apart, every one of them declaring 45°: the sweep's own median step is
/// what says 45° cannot be true, and the width is held to
/// [`crate::azimuth::MAX_ADJACENT_GAP_STEPS`] of it.
///
/// Without the clamp the arc would paint 22.5° past each end and the sweep
/// would cover 245° of sky it never looked at.
#[test]
fn a_lying_declaration_is_clamped_to_the_sweeps_median() {
    let azimuths: Vec<f32> = (0..=400).map(|i| i as f32 * 0.5).collect();
    let scan = l2_sweep(&vec![200; azimuths.len()], &azimuths, 45.0, false);
    let (_, _, values) = render_radar_to_image(&scan, L2_ELEVATION, PRODUCT, LAT, LON).unwrap();

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

/// The absolute cap, which the per-sweep one cannot stand in for. Two
/// radials 10° apart declaring nothing at all: their median circular gap is
/// the *larger* of the two ways round, 350°, so both the fallback and the
/// per-sweep ceiling derived from it are useless here.
/// [`super::MAX_WEDGE_DEG`] is what keeps the pair from being drawn as two
/// 350°-wide chord lenses covering the entire display.
#[test]
fn a_sweep_with_no_declaration_is_capped_at_a_sane_wedge() {
    let scan = l2_sweep(&[200, 200], &[0.0, 10.0], 0.0, false);
    let (_, _, values) = render_radar_to_image(&scan, L2_ELEVATION, PRODUCT, LAT, LON).unwrap();

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

/// The same rule on the derived grids, which have no declaration to read: a
/// 36° NROT sector must stop at its own edge.
///
/// NROT used to take its row width as `360 / rows`, which is the spacing
/// only if the grid closes the circle — a 72-row sector came out at 5° a row
/// and smeared 2.5° past both ends of the arc it was computed over.
#[test]
fn nrot_does_not_smear_past_its_sector() {
    let scan = nrot_sector(72, 0.5);
    let (_, _, values) = render_radar_to_image(
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
        assert_probes(
            &values,
            false,
            &[(37.0, range), (-1.5, range)],
            "the sector runs 0° to 35.5° and the display must end where it does",
        );
    }
}

/// How far a sweep reaches is the longest of its radials, not the first one
/// to carry the moment. A truncated opening radial used to speak for the
/// whole sweep — and the number it set is the extent the raster is projected
/// at, so a short answer is real data cut off at the edge of the image.
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

/// The width arithmetic on its own, at the boundaries the fixtures above
/// cannot reach: a NaN declaration, a negative one, and the point where the
/// two ceilings swap over.
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

/// A minimal Level III message around one digital radial packet whose
/// scale-factor halfword claims ~1 km gates — what product 163 really
/// carries on the wire, where that halfword is the scan projection
/// constant and not a gate size.
fn message_with_lying_scale_factor(product_code: i16, bins: usize) -> Level3Message {
    use nexrad_level3::model::{DataLayer, MessageHeader, SymbologyBlock};

    let packet = RadialPacket {
        first_range_bin: 0,
        num_range_bins: bins as u16,
        i_center: 0,
        j_center: 0,
        // ~1 km per gate if believed.
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

/// Product 163's packet says ~1 km per gate; the ICD says 0.25 km. The
/// display path has to prefer the PDB's override the way the
/// twin-comparison path does, or the on-screen KDP field draws 4× too
/// far out. A product without an override keeps the packet's own value.
#[test]
fn message_path_prefers_the_pdb_gate_spacing_over_the_packets() {
    const BINS: usize = 40;

    let (_, max_range, _) = render_level3_message_to_image(
        &message_with_lying_scale_factor(163, BINS),
        types::RadarProduct::SpecificDifferentialPhase,
        LAT,
        LON,
    )
    .unwrap();
    assert!(
        (max_range - BINS as f64 * 0.25).abs() < 1e-9,
        "163 must render at the ICD's 0.25 km spacing, got a max range of {max_range} km \
             from {BINS} gates"
    );

    let (_, max_range, _) = render_level3_message_to_image(
        &message_with_lying_scale_factor(94, BINS),
        PRODUCT,
        LAT,
        LON,
    )
    .unwrap();
    let packet_km = 1.0 / 0.999_f32 as f64;
    assert!(
        (max_range - BINS as f64 * packet_km).abs() < 1e-9,
        "a product with no PDB override must keep the packet's spacing, got a max range \
             of {max_range} km from {BINS} gates"
    );
}

// ── Which sweep a requested tilt reaches ─────────────────────────────────

/// A sweep whose antenna is still settling when it opens: the first
/// `SETTLING` radials ramp from `first` to `flown`, and the rest sit on
/// `flown`. The median is therefore `flown` — the tilt the sweep actually
/// flew — while the first radial reads `first`.
///
/// Every fixture in this crate before these tests gave a sweep one constant
/// elevation, which makes the median and the first radial the same number
/// and makes the difference between them invisible. That is why the switch
/// to the median broke no test: there was no test of it. This builder is
/// the one shape that can tell the two apart.
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

/// The tilt a sweep is found by is the one it flew, not the one it happened
/// to open on. The first radial here is 0.68° and the flown cut is 0.44°:
/// asking for 0.4° must reach it, and asking for 0.7° — which is where the
/// first radial sat — must not.
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

/// The KDDC VCP 215 case, which is what this change is for. Two
/// surveillance cuts — 0.44° and 0.84° — both opening well off their own
/// angle, and overlapping under the old 0.3° window. Each label must reach
/// its own cut, so neither cut is drawn twice and neither is lost.
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
    // The labels between them belong to neither cut and must draw nothing
    // rather than silently reusing a neighbour.
    assert!(at(0.6).is_none(), "0.6° is not a tilt this volume flew");
}

/// Newest-wins is load-bearing for SAILS and is *not* what changed here:
/// two sweeps of the same cut must still resolve to the later one.
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

/// The surveillance preference, unchanged: a non-Doppler product takes the
/// velocity-free half of a split cut even though the Doppler half is newer.
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

/// The window is the other half of the change: on the median it is narrow
/// enough that a neighbouring cut cannot answer for one that is missing.
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

/// The contract the whole change exists to keep: **every label the picker
/// offers reaches a sweep, and the sweep it reaches is the one the label
/// names.**
///
/// Swept across every tilt on a 0.05° grid, so the cases where a cut sits
/// exactly on the boundary of the picker's 0.1° rounding — the worst case
/// for the match window, and the ones a hand-picked fixture always misses —
/// are all covered. This is what says the window may not be narrowed to the
/// rounding itself: at 0.05° a cut landing on a boundary is half a step from
/// its own label and becomes unreachable.
#[test]
fn every_offered_label_reaches_the_cut_it_names() {
    for step in 0..=240u32 {
        let flown = step as f32 * 0.05;
        // Opening a third of a degree off, the way a real one does.
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

/// The loop's snap reads the same quantity the picker labels do, so a
/// steady selection stays on one cut across frames instead of following
/// the antenna's settling around.
#[test]
fn find_closest_elevation_snaps_to_the_flown_tilt() {
    let scan = scan_of(vec![
        settling_sweep(1, 0.68, 0.44, false),
        settling_sweep(2, 0.30, 0.84, false),
    ]);
    assert_eq!(find_closest_elevation(&scan, PRODUCT, 0.5), Some(0.4));
    assert_eq!(find_closest_elevation(&scan, PRODUCT, 0.8), Some(0.8));
}

/// The hail and HCA render paths anchor on the feedhorn, not the ground.
///
/// Both add their site height to a beam height, and `beam` measures those
/// above the antenna, so the ground under the tower is the wrong datum by a
/// whole tower — 62 ft at KTLX, 114 ft at the tallest. Neither render path
/// has a test that would see that shift in its output, so this pins the
/// lookup itself: written as the two numbers so that a switch back to
/// `Datum::SiteBase` fails here rather than passing quietly.
#[test]
fn the_render_paths_site_height_is_the_feedhorn() {
    // KTLX: 1213 ft of ground under a 62 ft tower.
    const KTLX: (f64, f64) = (35.33306, -97.2775);
    assert_eq!(
        super::render_site_height_ft(KTLX.0, KTLX.1),
        1213.0 + 62.0,
        "the feedhorn",
    );
    assert_ne!(
        super::render_site_height_ft(KTLX.0, KTLX.1),
        1213.0,
        "the ground under the tower is not the datum a beam height is above",
    );
}

/// Gates per dual-pol radial in [`dual_pol_tilt`]: 400 × 250 m from 125 m
/// reaches 100 km, far enough that a 0.5° beam climbs through either
/// candidate melting layer.
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

/// One 360-radial dual-pol tilt of uniform light rain — 30 dBZ, 1 dB ZDR,
/// 0.99 ρHV, a gently ramping ΦDP. Uniform on purpose: whatever class the
/// classifier reaches, it reaches for the whole disc, so a change of class
/// is unmistakably the environment's doing and not a gate-to-gate texture.
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

/// The premise every environmental-heights invalidation rests on: the hybrid
/// classification's picture is a **function of** the sounding's pair, so a
/// pane still holding one drawn against the old environment is showing the
/// wrong classification rather than merely an old one.
///
/// Nothing asserted this before, and the gap it left was not theoretical:
/// `RenderDispatcher::set_env_heights` dropped the hail pair's renders alone
/// while the render parameters already carried the pair here too, so a landed
/// sounding left every HCA pane on its default-melting-layer picture until the
/// volume rolled. [`RadarProduct::reads_env_heights`] is now the one statement
/// of that set; this is the measurement that puts HCA in it.
///
/// The two environments are the adaptation defaults (0 °C at 10.5 kft MSL) and
/// a winter sounding with the freezing level down at 0.8 km MSL — under, not
/// over, the beam. Uniform light rain fills the disc either way, so the class
/// the classifier reaches is the whole answer.
#[test]
fn the_hybrid_classification_changes_with_the_environmental_heights() {
    /// Dry snow: what the low freezing level puts the beam above.
    const DS: f32 = 40.0;
    /// Rain: what the default melting layer puts the beam below.
    const RA: f32 = 60.0;

    let scan = scan_of(vec![dual_pol_tilt(1, 0.5)]);
    let defaults =
        super::render_hhc_to_image(&scan, LAT, LON, None).expect("the fixture classifies");
    let sounding = super::render_hhc_to_image(&scan, LAT, LON, Some((0.8, 2.0)))
        .expect("the fixture classifies");

    let painted = |grid: &[f32]| grid.iter().filter(|v| !v.is_nan()).count();
    let all_of = |grid: &[f32], class: f32| grid.iter().filter(|v| **v == class).count();

    let cells = painted(&defaults.2);
    assert!(
        cells > 0,
        "the fixture painted nothing, so it proves nothing"
    );
    assert_eq!(
        painted(&sounding.2),
        cells,
        "both environments must classify the same disc — a difference in \
         *coverage* would confound the difference in class",
    );
    assert_eq!(
        all_of(&defaults.2, RA),
        cells,
        "with the adaptation defaults the melting layer sits above the beam \
         and the whole disc is rain",
    );
    assert_eq!(
        all_of(&sounding.2, DS),
        cells,
        "with the sounding's 0.8 km freezing level the beam climbs out of the \
         melting layer and the whole disc is dry snow",
    );
    assert_ne!(
        defaults.0, sounding.0,
        "and the rasterized pixels differ too, which is what a pane shows",
    );
}
