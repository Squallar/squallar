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

/// The fixture has to actually paint, or everything below passes vacuously.
#[test]
fn fixture_covers_a_realistic_share_of_the_image() {
    let (image, values) = render(&packet(None));
    let painted = image.chunks_exact(4).filter(|px| px[3] != 0).count();
    // 600 gates of 0.25 km reach 150 km, inside the floor, so this fixture is
    // drawn on the 230 km frame every short product gets.
    let px_per_km = types::IMAGE_SIZE as f64 / (2.0 * types::BASE_EXTENT_KM);
    let disc = std::f64::consts::PI * (N_BINS as f64 * 0.25 * px_per_km).powi(2);
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

/// Which pixel a point `range_km` out at `az_deg` from the site lands in on a
/// raster projected at `extent_km`, through the same [`MercatorProjection`]
/// the renderer paints with.
///
/// A duplicate of the arithmetic in `render_gate` rather than a call into it,
/// because `render_gate` writes and this asks. Both walk azimuth → offset →
/// Mercator → truncated pixel, and the truncation is why a probe has to go
/// through the projection at all: a hand-computed pixel would be off by one
/// somewhere and the difference between "unpainted" and "off by one" is the
/// whole point of these tests.
///
/// The extent and the side are both arguments because both are now properties
/// of the render being probed rather than of the display: a fixture reaching
/// 150 km is drawn on the 230 km floor at [`types::IMAGE_SIZE`], a TDWR-shaped
/// one on a 417 km frame, and the same TDWR through a `_sized` entry on a
/// 4096-pixel one. A probe that assumed any of those would be asking about the
/// wrong picture. Callers pass what the render they are probing handed back.
fn probe_at(extent_km: f64, side_px: usize, az_deg: f64, range_km: f64) -> usize {
    let bounds = types::ImageBounds::from_radar_site(LAT, LON, extent_km);
    let proj = MercatorProjection::from_bounds(LAT, &bounds, extent_km, side_px);
    let (sin_az, cos_az) = az_deg.to_radians().sin_cos();
    let dest_lat_rad = proj.radar_lat_rad + (range_km * cos_az) / types::EARTH_RADIUS_KM;
    let cos_correction = proj.cos_radar_lat / dest_lat_rad.cos();
    let px = (proj.center_px + range_km * sin_az * cos_correction * proj.px_per_km) as usize;
    let py = ((proj.merc_y_top - types::lat_rad_to_mercator_y(dest_lat_rad)) * proj.merc_y_scale)
        as usize;
    py * side_px + px
}

/// [`probe_at`] on the floor, which is where every fixture in this file that
/// does not say otherwise is drawn — their moments reach 150 km.
fn probe(az_deg: f64, range_km: f64) -> usize {
    probe_at(types::BASE_EXTENT_KM, types::IMAGE_SIZE, az_deg, range_km)
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

// ── How far a render reaches, and how wide it is drawn ───────────────────
//
// A raster used to be ±230 km whatever it held, and the gate loops stopped
// there. It is now projected at `types::plan_view_extent_km` of the sweep's
// own reach, so what a render reports is the half-width of the *picture* and
// the pictures below are three different sizes.

/// Gates in TPIT's long-range surveillance cut.
const TDWR_GATES: usize = 1390;
/// Its gate, km. 1390 of them reach 417.
const TDWR_GATE_KM: f64 = 0.3;
/// Gates either side of the beacon's own, so the ring is five gates thick —
/// 1.5 km, three or four pixels at the 2.46 px/km a 417 km frame gives a
/// 2048-pixel image, which is enough to survive azimuth quantization at every
/// bearing rather than only at the four cardinals.
const TDWR_BEACON_GATES: usize = 2;

/// A TDWR long-range reflectivity cut, as TPIT actually flies it: 1390 gates
/// of 300 m from the antenna, 360 radials at the 1.0° a TDWR declares.
///
/// Every gate is below threshold except a band `beacon_km` out, so the render
/// is a thin ring at a known range rather than a filled disc — a filled one
/// would put its outermost pixels at the reach whatever happened in between,
/// and the point here is *where* a single far return lands.
fn tdwr_long_range_sweep(beacon_km: f64) -> Scan {
    use nexrad_model::data::{MomentData, PulseWidth, RadialStatus, Sweep, VolumeCoveragePattern};

    let beacon_gate = (beacon_km / TDWR_GATE_KM).round() as usize;
    let gates: Vec<u8> = (0..TDWR_GATES)
        .map(|g| {
            if g.abs_diff(beacon_gate) <= TDWR_BEACON_GATES {
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

/// Every painted pixel's ground range from the site, km, read back out of the
/// bounds the render declares — the inverse of the trip `render_gate` makes,
/// so it answers in the same kilometres the gates were indexed by.
fn painted_ranges_km(values: &[f32], extent_km: f64) -> Vec<f64> {
    let bounds = types::ImageBounds::from_radar_site(LAT, LON, extent_km);
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
            crate::beam::site_bearing_range_km(LAT, LON, lat, lon).1
        })
        .collect()
}

/// A TDWR's long-range reflectivity is drawn to the 417 km it reaches, and a
/// return 400 km out lands where the render's own projection says it should.
///
/// 1390 gates of 300 m is the cut TPIT flies on its lowest tilt, and it is the
/// case the fixed 230 km frame threw away outright: everything past gate 767
/// fell out of the loop, so nearly half the sweep's range — the whole outer
/// two thirds of its area — was decoded, held in memory and never drawn.
#[test]
fn a_tdwr_long_range_sweep_is_projected_at_its_own_reach() {
    const BEACON_KM: f64 = 400.2; // gate 1334's centre
    let scan = tdwr_long_range_sweep(BEACON_KM);
    let (_, extent_km, values) =
        render_radar_to_image(&scan, L2_ELEVATION, PRODUCT, LAT, LON).unwrap();

    assert!(
        (extent_km - 417.0).abs() < 1e-9,
        "1390 gates of 0.3 km reach 417 km; the render declares {extent_km} km",
    );

    // The beacon, at four bearings, on the frame this render declared. Not on
    // the floor's frame: a probe there would be asking about a picture 1.81×
    // smaller, and 400 km east of the site is not on it at all.
    for az in [0.0, 90.0, 180.0, 270.0] {
        let at = probe_at(extent_km, types::IMAGE_SIZE, az, BEACON_KM);
        assert!(
            !values[at].is_nan(),
            "the beacon 400 km out at {az}° is unpainted at the pixel this \
             render's own projection puts it in",
        );
    }

    // And the picture really is wider than the wall was, measured off the
    // pixels: the eastmost painted column stands at the beacon's outer edge,
    // 170 km past where every gate used to stop. Due east the projection's
    // `cos φ₀ / cos φ` factor is exactly 1, so a column is the range in pixels
    // and nothing else.
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
    // The band's own far edge: its outermost gate's centre plus half a gate.
    // Two pixels of tolerance for `render_gate`'s sample padding and the
    // truncating cast that turns a position into a column — 0.81 km at this
    // extent, which is under three of the 0.3 km gates being drawn.
    let band_edge_km =
        ((BEACON_KM / TDWR_GATE_KM).round() + TDWR_BEACON_GATES as f64 + 0.5) * TDWR_GATE_KM;
    assert!(
        (painted_km - band_edge_km).abs() < 2.0 / px_per_km,
        "the outermost painted column stands {painted_km:.2} km out against a \
         beacon band ending at {band_edge_km:.2} km",
    );
}

/// A sweep whose data stops inside the floor is drawn on the floor's frame,
/// at the scale it has always been drawn at.
///
/// This is the guarantee the floor exists for. `l2_sweep`'s moments are 600
/// gates of 250 m — 150 km, the shape of a WSR-88D Doppler cut — so the
/// picture must come out 230 km wide with the echo filling 150 km of it, and
/// the radius is recovered from the pixels rather than assumed: the painted
/// disc's outermost column is measured and converted back at
/// `IMAGE_SIZE / 460`, the km-per-pixel this display had before there was an
/// extent at all.
#[test]
fn a_sweep_inside_the_floor_is_drawn_at_the_floor() {
    let azimuths: Vec<f32> = (0..360).map(|i| i as f32).collect();
    let scan = l2_sweep(&[200; 360], &azimuths, 1.0, false);
    let (_, extent_km, values) =
        render_radar_to_image(&scan, L2_ELEVATION, PRODUCT, LAT, LON).unwrap();

    assert_eq!(
        extent_km,
        types::BASE_EXTENT_KM,
        "a 150 km sweep must be drawn on the 230 km frame it always was",
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

    let px_per_km = side as f64 / (2.0 * types::BASE_EXTENT_KM);
    let radius_km = (east - west) as f64 / 2.0 / px_per_km;
    assert!(
        (radius_km - 150.0).abs() < 1.0,
        "600 gates of 0.25 km must fill 150 km of the frame; the disc measures \
         {radius_km:.2} km across columns {west}..{east} at {px_per_km:.4} px/km",
    );
}

/// The number a render reports bounds the picture it hands over: nothing is
/// painted further from the site than the extent, on a frame that is full and
/// on a frame that is not.
///
/// The property every consumer of `max_range_km` depends on and none of them
/// can check. The frontend places the texture between the bounds this extent
/// builds, and a hover divides a pointer position by them to pick a pixel
/// back out — so a gate painted past the extent would be a return the display
/// draws in one place and reads from another, with nothing anywhere to notice.
#[test]
fn nothing_is_painted_outside_the_extent_a_render_declares() {
    let azimuths: Vec<f32> = (0..360).map(|i| i as f32).collect();
    for (scan, why) in [
        (
            l2_sweep(&[200; 360], &azimuths, 1.0, false),
            "150 km of echo",
        ),
        (tdwr_long_range_sweep(400.2), "a 417 km TDWR cut"),
    ] {
        let (_, extent_km, values) =
            render_radar_to_image(&scan, L2_ELEVATION, PRODUCT, LAT, LON).unwrap();
        let ranges = painted_ranges_km(&values, extent_km);
        assert!(!ranges.is_empty(), "{why} painted nothing");

        let furthest = ranges.iter().copied().fold(0.0f64, f64::max);
        // One pixel of slop: a pixel's *centre* is what is measured and a gate
        // reaching exactly the extent claims the pixel it starts in.
        let slop = 1.0 / (types::IMAGE_SIZE as f64 / (2.0 * extent_km));
        assert!(
            furthest <= extent_km + slop,
            "{why}: a pixel {furthest:.2} km out on a raster declaring \
             {extent_km:.2} km",
        );
    }
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
///
/// Both arms are given enough gates to reach past
/// [`types::BASE_EXTENT_KM`], because what a render reports is the extent it
/// was projected at and below the floor that is 230 km whatever the spacing —
/// which is exactly the observation this test would lose. 1200 gates at the
/// ICD's 0.25 km is 300 km; believing the packet instead would ask for
/// 1201 km and be held at the cap. 460 gates at the packet's own ~1.001 km is
/// 460.5 km; a spurious override would collapse it to 115 km and be held at
/// the floor. So each arm's wrong answer is a different number from its right
/// one on both sides.
#[test]
fn message_path_prefers_the_pdb_gate_spacing_over_the_packets() {
    const ICD_BINS: usize = 1200;
    const PACKET_BINS: usize = 460;

    let (_, extent_km, _) = render_level3_message_to_image(
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

    let (_, extent_km, _) = render_level3_message_to_image(
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

    // The fixture reaches 100 km, well inside the floor, so both renders take
    // the base raster and neither is asking anything of the size cascade.
    let scan = scan_of(vec![dual_pol_tilt(1, 0.5)]);
    let defaults = super::render_hhc_to_image(&scan, LAT, LON, None, types::IMAGE_SIZE)
        .expect("the fixture classifies");
    let sounding = super::render_hhc_to_image(&scan, LAT, LON, Some((0.8, 2.0)), types::IMAGE_SIZE)
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

// ── The adaptive raster size ─────────────────────────────────────────────────

/// The side a static desktop or mobile pane offers, and the only 4096 in this
/// file. Named rather than repeated so the rows below cannot drift apart.
const LONG_RANGE_SIDE: usize = 4096;

/// A sweep stopping inside the floor is drawn **byte for byte** the same way
/// whether or not a long-range raster was on offer.
///
/// This is the guarantee the whole size cascade rests on. Nearly every render
/// this display makes stops inside 230 km — every Doppler cut, every derived
/// 1° × 1 km grid, every Level III product fetched here — and a change to the
/// raster's size that moved any of them would move the entire fleet of
/// pictures that were correct yesterday. Bit-identical rather than
/// approximately equal, because "close" is not the claim: the claim is that
/// nothing already on screen changed at all.
///
/// Both buffers are compared, not just the image: a value grid that shifted
/// while the colours did not would be a hover reading the wrong gate, which no
/// visual check would ever show.
#[test]
fn a_render_inside_the_floor_ignores_the_long_range_ceiling_entirely() {
    let azimuths: Vec<f32> = (0..360).map(|i| i as f32).collect();
    let scan = l2_sweep(&[200; 360], &azimuths, 1.0, false);

    let base = render_radar_to_image_full(&scan, L2_ELEVATION, PRODUCT, LAT, LON, None, None)
        .expect("the fixture renders");
    let offered = render_radar_to_image_full_sized(
        &scan,
        L2_ELEVATION,
        PRODUCT,
        LAT,
        LON,
        None,
        None,
        LONG_RANGE_SIDE,
    )
    .expect("the fixture renders at the long-range ceiling too");

    assert_eq!(
        base.1, offered.1,
        "a 150 km sweep must project at the floor under either ceiling",
    );
    assert_eq!(
        base.0.len(),
        types::IMAGE_SIZE * types::IMAGE_SIZE * 4,
        "the floor's raster is the base size",
    );
    assert_eq!(base.0, offered.0, "the image moved under an unused ceiling");
    // `NaN` is most of a value grid, and `NaN != NaN`, so the bits are what is
    // compared — a grid full of quiet NaNs would otherwise never be equal to
    // itself and this assertion would be vacuous.
    let bits = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
    assert_eq!(
        bits(&base.2),
        bits(&offered.2),
        "the value grid moved under an unused ceiling",
    );
}

/// A TDWR's long-range cut takes the long-range raster, and a return 400 km
/// out lands within a pixel of where the render's own projection puts it.
///
/// The size and the extent are separate decisions and this is where they meet:
/// 1390 gates of 300 m reach 417 km, so the picture covers 1.81× the ground
/// the floor does — and on 4096 px rather than 2048 it covers it at very
/// nearly the same resolution, which is the whole point of the second number.
#[test]
fn a_tdwr_long_range_sweep_takes_the_long_range_raster() {
    const BEACON_KM: f64 = 400.2; // gate 1334's centre
    let scan = tdwr_long_range_sweep(BEACON_KM);
    let (image, extent_km, values) = render_radar_to_image_full_sized(
        &scan,
        L2_ELEVATION,
        PRODUCT,
        LAT,
        LON,
        None,
        None,
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
        (extent_km - 417.0).abs() < 1e-9,
        "the extent is the sweep's own reach, not the raster's size: {extent_km}",
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

/// What the second number buys, measured: a long-range raster is never
/// meaningfully coarser than the floor, where a base-size one would be less
/// than half as fine.
///
/// The floor is 2048 px over 460 km — 4.4522 px/km, or 1.11 pixels across a
/// 250 m gate — and that figure is what every visual judgement this display
/// has ever been checked against was made at. Stretching the *base* raster
/// over a 458 km surveillance cut gives 2.2358 px/km, 0.56 pixels a gate, so
/// gates start sharing pixels rather than owning them; 4096 px gives 4.4716
/// and 1.12, which is the floor's own scale to within half a percent.
///
/// The ratio is not flat, and pretending it were would be the wrong claim: the
/// side steps once at the floor while the extent is continuous, so a 298 km
/// Doppler cut comes out **finer** than the floor at 6.87 px/km. Finer is
/// free — the raster costs the same pixels wherever they land — and what
/// matters is only that it is never much coarser.
///
/// | reach | side | px/km  | vs floor | base raster would be |
/// |-------|-----:|-------:|---------:|---------------------:|
/// | 298   | 4096 | 6.8725 |  +54.4 % |               3.4362 |
/// | 417   | 4096 | 4.9113 |  +10.3 % |               2.4556 |
/// | 458   | 4096 | 4.4716 |   +0.4 % |               2.2358 |
/// | 470   | 4096 | 4.3574 |   −2.1 % |               2.1787 |
///
/// The last row is [`types::MAX_EXTENT_KM`], which is a guard on arithmetic
/// rather than a reach any radar has — the widest real sweep is the 458 km
/// row — so it is the one place the picture is (slightly) coarser than the
/// floor, and it is stated rather than excluded.
#[test]
fn the_long_range_raster_keeps_the_floors_km_per_pixel() {
    let floor = types::IMAGE_SIZE as f64 / (2.0 * types::BASE_EXTENT_KM);
    for (extent_km, why) in [
        (298.0, "a WSR-88D Doppler cut at 1192 gates"),
        (417.0, "a TDWR long-range reflectivity cut"),
        (458.0, "a WSR-88D surveillance cut"),
    ] {
        let side = types::raster_side_px(extent_km, LONG_RANGE_SIDE);
        let px_per_km = side as f64 / (2.0 * extent_km);
        assert!(
            px_per_km >= floor,
            "{why}: {extent_km} km on {side} px is {px_per_km:.4} px/km, under \
             the floor's {floor:.4}",
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

    // The widest real sweep, in the unit that decides whether a gate is drawn
    // or shared: pixels across one 250 m super-resolution gate. The base
    // raster gives it half a pixel; the long-range one gives it its own.
    const SURVEILLANCE_KM: f64 = 458.0;
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

    // The cap: the one extent where the long-range raster *is* coarser, by the
    // 2.1% the table names and no more.
    let at_cap = types::raster_side_px(types::MAX_EXTENT_KM, LONG_RANGE_SIDE) as f64
        / (2.0 * types::MAX_EXTENT_KM);
    assert!(
        (at_cap / floor - 0.9787).abs() < 1e-3,
        "at the {} km cap the raster is {at_cap:.4} px/km, {:.4} of the \
         floor — the table above says 0.9787",
        types::MAX_EXTENT_KM,
        at_cap / floor,
    );
}

/// A ceiling *under* the base size is honoured, which is how the browser's
/// loop frames stay the size its texture budget was written for.
///
/// The same sweep at the same extent, drawn onto a quarter of the pixels: the
/// picture is coarser and the geometry is not, so the extent it declares is
/// the extent it would have declared at any size.
#[test]
fn a_ceiling_under_the_base_size_renders_a_leaner_picture_of_the_same_ground() {
    const LEAN: usize = 1024;
    let scan = tdwr_long_range_sweep(400.2);
    let (image, extent_km, values) =
        render_radar_to_image_full_sized(&scan, L2_ELEVATION, PRODUCT, LAT, LON, None, None, LEAN)
            .expect("the fixture renders");

    assert_eq!(image.len(), LEAN * LEAN * 4);
    assert_eq!(values.len(), LEAN * LEAN);
    assert!(
        (extent_km - 417.0).abs() < 1e-9,
        "a leaner raster covers the same ground: {extent_km}",
    );
    let at = probe_at(extent_km, LEAN, 90.0, 400.2);
    assert!(
        !values[at].is_nan(),
        "the beacon is unpainted on the lean raster",
    );
}
