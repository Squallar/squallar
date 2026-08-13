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

/// The fixture has to actually paint, or everything below passes vacuously.
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

/// [`l2_sweep`] with the shape the tie-break tests were written against: 1°
/// apart from 90°, declaring the 1° they are apart by.
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

/// Which pixel a point `range_km` out at `az_deg` from the site lands in on a
/// raster projected at `extent_km`, through the same [`MercatorProjection`]
/// the renderer paints with.
///
/// Through `MercatorProjection::pixel_at`, the placement itself, rather than
/// through a restatement of it: `render_gate` writes and this asks, but both
/// have to be asking about the same pixel. The truncation is why a probe goes
/// through the projection at all — a hand-computed pixel would be off by one
/// somewhere, and the difference between "unpainted" and "off by one" is the
/// whole point of these tests.
///
/// This *was* a duplicate, and the duplication is what let the placement stay
/// wrong: it walked the same equirectangular offsets `render_gate` walked, so
/// every probe in this file agreed with the renderer about a position neither
/// of them shared with [`crate::beam::site_bearing_range_km`] — which is the
/// function the hover readout, the cross-section and [`painted_ranges_km`] all
/// ask the same question backwards with.
///
/// The extent and the side are both arguments because both are now properties
/// of the render being probed rather than of the display: a fixture reaching
/// 150 km is drawn on a 150 km frame at [`types::IMAGE_SIZE`], a TDWR-shaped
/// one on a 417 km frame, and the same TDWR through a `_sized` entry on a
/// 4096-pixel one. A probe that assumed any of those would be asking about the
/// wrong picture. Callers pass what the render they are probing handed back.
fn probe_at(extent_km: f64, side_px: usize, az_deg: f64, range_km: f64) -> usize {
    let bounds = types::ImageBounds::from_radar_site(LAT, LON, extent_km);
    let proj = MercatorProjection::from_bounds(LAT, &bounds, extent_km, side_px);
    let (sin_az, cos_az) = az_deg.to_radians().sin_cos();
    let (sin_d, cos_d) = (range_km / types::EARTH_RADIUS_KM).sin_cos();
    let (px, py) = proj.pixel_at(sin_az, cos_az, sin_d, cos_d);
    py as usize * side_px + px as usize
}

/// Gates in [`l2_sweep`]'s moments, and the gate they are — the shape of a
/// WSR-88D cut, and the geometry every probe in this file that does not say
/// otherwise is answered against.
const L2_GATES: usize = 600;
/// See [`L2_GATES`]. 600 of these reach 150 km of beam.
const L2_GATE_KM: f64 = 0.25;

/// The ground [`l2_sweep`] covers, km, and so the extent every render of it is
/// projected at.
///
/// Derived from the fixture's own gates rather than written down, because that
/// is the property under test everywhere it is used: these renders are framed
/// by what their data reaches, so a probe that assumed any other number would
/// be asking about a different picture. It used to be `types::BASE_EXTENT_KM` —
/// a 150 km fixture really was drawn on a 230 km frame — and that is exactly
/// the substitution this file now has none of.
fn l2_ground_reach_km() -> f64 {
    L2_GATES as f64 * L2_GATE_KM * f64::from(L2_ELEVATION).to_radians().cos()
}

/// The raw gate codes Level II reserves below the data range. Every other
/// fixture in this file writes a value byte; these two are the states the
/// plan view has to tell apart from each other and from an unpainted pixel.
const RAW_BELOW_THRESHOLD: u8 = 0;
const RAW_RANGE_FOLDED: u8 = 1;

/// The RGBA a pixel holds.
fn pixel_at(image: &[u8], idx: usize) -> (u8, u8, u8, u8) {
    let px = &image[idx * 4..idx * 4 + 4];
    (px[0], px[1], px[2], px[3])
}

/// Assert what the sweep painted at a list of `(azimuth, range)` probes, on
/// the frame [`l2_sweep`]'s own reach gives it.
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

/// The absolute cap, which the per-sweep one cannot stand in for. Two
/// radials 10° apart declaring nothing at all: their median circular gap is
/// the *larger* of the two ways round, 350°, so both the fallback and the
/// per-sweep ceiling derived from it are useless here.
/// [`super::MAX_WEDGE_DEG`] is what keeps the pair from being drawn as two
/// 350°-wide chord lenses covering the entire display.
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
/// The ground the cut covers, km: its 417 km of beam laid down at the
/// fixture's own 0.5°.
///
/// Not 417. A frame is sized by the ground its sweep covers, and at 0.5° that
/// is 16 m short of the beam's length — a fortieth of a pixel, and the whole
/// reason the correction is landable on the WSR-88D fleet. The tests below
/// pin the ground figure rather than the round one so that a future change
/// putting slant ranges back on the glass fails here instead of only at 60°.
fn tdwr_ground_reach_km() -> f64 {
    417.0 * f64::from(L2_ELEVATION).to_radians().cos()
}

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

/// The same cut with **every** gate above threshold, so the painted disc runs
/// out to the frame's own edge at every bearing.
///
/// The band fixture above cannot ask where the edge is — it paints a ring at
/// 400.2 km inside a 417 km frame, so 15.88 km of margin absorbs anything an
/// edge test could catch, which is the same vacuity the 230 km floor used to
/// give every fixture in this file. A disc is what
/// `nothing_is_painted_outside_the_extent_a_render_declares` needs and the
/// only thing it is used for.
fn tdwr_filled_long_range_sweep() -> Scan {
    tdwr_sweep_from_gates(vec![200; TDWR_GATES])
}

/// The 360 radials both TDWR fixtures are made of, over whatever gates they
/// hand in: 1390 gates of 300 m from the antenna at the 1.0° a TDWR declares.
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

/// Every painted pixel's ground range from the site, km, read back out of the
/// bounds the render declares — the inverse of the trip `render_gate` makes,
/// so it answers in the same kilometres the gates were indexed by.
///
/// `site_lat` because the trip is not latitude-independent and the caller that
/// matters renders the same sweep at several: it is
/// [`crate::beam::site_bearing_range_km`] on the way back and
/// [`crate::beam::great_circle_destination`] on the way out, and reading a
/// render taken at one latitude through bounds built at another measures the
/// mismatch instead of the render.
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
            crate::beam::site_bearing_range_km(site_lat, LON, lat, lon).1
        })
        .collect()
}

/// [`painted_ranges_km_at`] at [`LAT`], the site every fixture here is flown by
/// unless it says otherwise.
fn painted_ranges_km(values: &[f32], extent_km: f64) -> Vec<f64> {
    painted_ranges_km_at(values, extent_km, LAT)
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

    // The beacon, at four bearings, on the frame this render declared. Not on
    // a 230 km frame: a probe there would be asking about a picture 1.81×
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

/// Gates in a TDWR's Doppler moments, and the gate they are.
///
/// Measured, not chosen: 592 gates of 150 m from the antenna, reaching
/// 88.800 km of beam, decoded from the lowest cut of the 2026-08-11 00Z volume
/// at **TOKC, TDAL, TPIT and TATL** — four TDWR sites across four regions,
/// identical on every one of them to the last bit. Beside them on the same
/// volume sits [`TDWR_GATES`]' 1390-gate reflectivity reaching 417 km, which is
/// 4.7 times as far.
const TDWR_DOPPLER_GATES: usize = 592;
/// See [`TDWR_DOPPLER_GATES`]. 592 of these reach 88.8 km of beam.
const TDWR_DOPPLER_GATE_KM: f64 = 0.15;

/// A TDWR volume as the four sites above actually fly it: one cut carrying a
/// 1390-gate reflectivity **and** a 592-gate velocity, so the two moments of
/// one sweep reach 417 km and 88.8 km respectively.
///
/// Both moments are filled rather than beaconed, because what these tests read
/// off the picture is where each product's data *stops*.
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

/// **The reported defect.** A TDWR velocity pane is framed at the 88.8 km its
/// Doppler moment reaches, not at 230 km, and the reflectivity beside it on the
/// same volume is framed at its own 417 km.
///
/// A pane's range ring is drawn at the render's `max_range_km`
/// (`rustdar_egui::ui_map_pane`'s `render_radar_range_ring`), so this number
/// *is* the ring. Drawn at 230 km around 88.8 km of data it claimed 6.7 times
/// the coverage the radar had, and on a velocity pane — where the ring is read
/// as the edge of the Doppler coverage — that is the reading it destroys.
///
/// The two products are asserted against each other and not only against their
/// own numbers, because a per-product reach is exactly what a single volume-wide
/// extent cannot express: one volume, one sweep, two moments, two frames.
///
/// Run at all four sites' real coordinates rather than one. The extent in
/// kilometres is a property of the moment and must not move between them; where
/// the site *is* changes the bounds those kilometres become, and asserting both
/// halves at four latitudes from 26 °N to 40 °N is what separates
/// "the extent is the data's" from "the extent happens to be right at one site".
#[test]
fn a_tdwr_doppler_sweep_is_projected_at_its_own_reach_not_the_base_extent() {
    let cos_e = f64::from(L2_ELEVATION).to_radians().cos();
    let doppler_ground_km = TDWR_DOPPLER_GATES as f64 * TDWR_DOPPLER_GATE_KM * cos_e;

    // The four measured sites, with the coordinates the site table holds.
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

        // The defect, stated as the assertion that would have failed.
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

        // Per-product reach on one volume: the same sweep, two frames.
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

        // What the ring the user sees is drawn at, at this site's latitude:
        // the bounds the frontend hands back to `ImageBounds::from_radar_site`.
        let bounds = types::ImageBounds::from_radar_site(lat, lon, vel.max_range_km);
        let ring_km = (bounds.max_lat - lat) * types::KM_PER_DEGREE_LAT;
        assert!(
            (ring_km - doppler_ground_km).abs() < 1e-6,
            "{name}: the ring stands {ring_km:.4} km out around \
             {doppler_ground_km:.4} km of data",
        );

        // And the echo fills that frame, so the ring bounds the picture rather
        // than floating outside it. Due east the projection's cos correction
        // is exactly 1.
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

/// A sweep whose leading radial carries no moment is still found, and is still
/// framed by its own reach.
///
/// `find_sweep_owner` asked `radials.first()` whether the sweep carried the
/// product, so one blank radial spoke for all 360: the Doppler cut vanished
/// from the search and a velocity request fell through to whatever else
/// matched. On this volume that is the reflectivity half of the same cut, so
/// the pane came back framed at 417 km — the range ring 328 km out around
/// 88.8 km of velocity, which is the reported defect reappearing through a
/// different door.
///
/// It is the same first-radial assumption the wind-profile fits carried, found
/// on the extent path while tracing where the frame's reach comes from.
#[test]
fn a_sweep_whose_leading_radial_is_blank_is_still_found_and_still_framed_by_its_own_reach() {
    use nexrad_model::data::Sweep;

    let full = tdwr_doppler_volume();
    let sweep = &full.sweeps()[0];

    // The same volume with the first radial's velocity stripped and every
    // other radial untouched.
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

    let cos_e = f64::from(L2_ELEVATION).to_radians().cos();
    let doppler_ground_km = TDWR_DOPPLER_GATES as f64 * TDWR_DOPPLER_GATE_KM * cos_e;
    assert!(
        (vel.max_range_km - doppler_ground_km).abs() < 1e-9,
        "one blank leading radial reframed the pane at {} km instead of \
         {doppler_ground_km:.5} km",
        vel.max_range_km,
    );
}

// ── Where a tilt's gates are drawn ───────────────────────────────────────
//
// A gate is measured out along the beam and belongs on the ground under it,
// and those are the same number only at zero elevation. The renderer used to
// paint the slant range: harmless on a WSR-88D, whose highest cut is 19.5°
// and whose worst error is 5.7 %, and not harmless at all on a TDWR, whose
// VCP 80 climbs to 60° where the slant range is *twice* the ground range.
//
// The fixtures below are single-tilt volumes carrying a cut table, because
// the point of the first test is that the plan view and the 3D sampler put
// the same echo in the same place — and the sampler builds its ladder by
// indexing that table with each sweep's `elevation_number`.

/// A single-tilt volume with one reflectivity band a known **slant** range
/// out and every other gate below threshold.
///
/// A band rather than a filled disc for the same reason
/// [`tdwr_long_range_sweep`] uses one: a disc paints its outermost pixels at
/// the reach whatever happened inside it, and every question here is about
/// *where* one return lands.
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

/// The inner and outer ground radii of a painted ring, km, and the range
/// halfway between them.
fn ring_bounds_km(values: &[f32], extent_km: f64) -> (f64, f64, f64) {
    let ranges = painted_ranges_km(values, extent_km);
    assert!(!ranges.is_empty(), "the fixture painted nothing at all");
    let near = ranges.iter().copied().fold(f64::INFINITY, f64::min);
    let far = ranges.iter().copied().fold(0.0f64, f64::max);
    (near, far, (near + far) / 2.0)
}

/// **The plan view and the 3D sampler put the same gate over the same
/// ground.** A 45° tilt's return at 20 km slant belongs at 14.142 km, and
/// both renderers now say so.
///
/// This is the property the whole correction exists for, and the one the
/// display could not previously have: the sampler has always applied `cos e`
/// (`sampler::sample_rung` converts a ground range to a slant one before
/// reading a gate), sections and voxels are drawn from it, and the plan view
/// drew the same echo 5.86 km further out. A user comparing a section against
/// the map above it was comparing two different geometries, and 45° is chosen
/// because `cos 45° = 1/√2` makes the disagreement impossible to mistake for
/// rounding.
///
/// Both sides are *measured*, not asserted against the same constant: the 2D
/// ring is read back out of its own pixels through the bounds the render
/// declared, and the 3D band is found by walking ground ranges past the
/// sampler until it stops answering.
#[test]
fn a_45_degree_sweep_lands_at_the_same_ground_range_in_2d_and_3d() {
    const ELEV: f32 = 45.0;
    const SLANT_KM: f64 = 20.0;
    const GATE_KM: f64 = 0.25;
    // 20 · cos 45° — the ground under the gate, and what both must agree on.
    let ground_km = SLANT_KM * f64::from(ELEV).to_radians().cos();

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
    // One gate of tolerance. The two find the band's edges differently — the
    // raster paints whole gate cells and the sampler interpolates between
    // gate centres — so their outermost answers legitimately differ by up to
    // a gate, while their centres cannot.
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

/// A TDWR's steep tilts halve in radius, which is the headline: `cos 60°` is
/// exactly 0.5, so a return 24 km out along a VCP 80 hazardous cut is drawn
/// 12 km from the site.
///
/// 592 gates of 150 m is TPIT's Doppler geometry, and 60° is the top of its
/// VCP 80 ladder. Before this, every one of those tilts painted its echoes at
/// up to twice their true distance — a storm over the airport drawn out at
/// the edge of the terminal area.
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
    // The whole picture, not just the band's centre: nothing is drawn out at
    // the slant range any more.
    assert!(
        far < SLANT_KM * 0.6,
        "the outermost painted pixel stands {far:.2} km out, which is most of \
         the way to the {SLANT_KM} km slant range this tilt used to be drawn at",
    );
}

/// The reach a render reports is a **ground** reach, so a frame is sized by
/// the ground it covers rather than by the beam's length.
///
/// TPIT's long-range surveillance cut is where this is largest: 1390 gates of
/// 300 m reach 417 km, and at the 0.2637° that cut actually flies they cover
/// 416.996 km of ground. Small on purpose — the point is that the number is
/// the ground one, not that it is far off. It is visible at every extent now
/// rather than only past 230 km, because no floor absorbs a short sweep's
/// correction any more: the same cut's 88.8 km Doppler moment reports 88.797.
#[test]
fn a_frame_is_sized_by_the_ground_its_sweep_covers() {
    const ELEV: f32 = 0.2637;
    let scan = tilted_beacon_sweep(ELEV, TDWR_GATE_KM, TDWR_GATES, 400.2, TDWR_BEACON_GATES);
    let SweepRender {
        max_range_km: extent_km,
        ..
    } = render_radar_to_image(&scan, ELEV, PRODUCT, LAT, LON).unwrap();

    let expected = 417.0 * f64::from(ELEV).to_radians().cos();
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

/// A 0.5° beacon at 200 km moves less than a pixel — the near-invariance
/// that makes this landable on the WSR-88D fleet.
///
/// `1 − cos 0.5°` is 3.8e-5, so 200 km of range moves 7.6 m. At the 4.45 px/km
/// a 230 km frame gives that is 0.034 of a pixel: every low tilt, which is nearly every
/// tilt anyone looks at, is where it has always been. The shift only becomes
/// visible where the geometry makes it real — 0.9 px at 2.4°, 18 px at 19.5°,
/// and half the radius at a TDWR's 60°.
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

/// A sweep whose data stops short is drawn at the range it reaches, and its
/// echo fills the frame rather than sitting in the middle of one.
///
/// The reported defect at fixture scale. `l2_sweep`'s moments are 600 gates
/// of 250 m — 150 km — and the picture used to come out 230 km wide with
/// the echo filling 150 km of it and the outer 80 km permanently blank. The
/// range ring was drawn around the 230, which is the lie: it claimed 53%
/// more coverage than the sweep had. The radius is recovered from the pixels
/// rather than assumed — the painted disc's outermost column is measured and
/// converted back at the render's own px/km — so this fails if the frame and
/// the picture inside it ever stop agreeing.
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

    // And it fills the frame: the disc's extreme columns are the raster's
    // own, give or take the half-pixel the edge column is quantized to. On
    // the old frame the echo stopped at column 1381 of 2048 and the 667
    // columns past it were unpaintable.
    assert!(
        west <= 1 && east >= side - 2,
        "the echo runs columns {west}..{east} of {side}; a sweep drawn at its \
         own reach must reach its own edge",
    );
}

/// The number a render reports bounds the picture it hands over: nothing is
/// painted further from the site than the extent, at any bearing, on two frame
/// sizes and at three latitudes.
///
/// The property every consumer of `max_range_km` depends on and none of them
/// can check. The frontend places the texture between the bounds this extent
/// builds, and a hover divides a pointer position by them to pick a pixel
/// back out — so a gate painted past the extent would be a return the display
/// draws in one place and reads from another, with nothing anywhere to notice.
///
/// # The bound is one pixel, and it took two changes to get there
///
/// It was one pixel before either of them and it was **vacuous at both
/// fixtures**: the frame used to be 230 km around 150 km of echo and 417 km
/// around a beacon at 400.2, so 80 km and 17 km of empty margin absorbed
/// anything this could have caught.
///
/// Projecting a raster at its sweep's own reach removed the margin, and what
/// the assertion then measured was a disagreement between two models of the
/// ground — `render_gate` placing a gate equirectangularly, `dy` km north read
/// off as a latitude offset and `dx` km east as a longitude one, against
/// [`painted_ranges_km_at`] reading the pixel back with
/// [`crate::beam::site_bearing_range_km`], a great-circle distance. Those agree
/// on the cardinals and diverge on the diagonals, always outward, growing with
/// range and with latitude to 1.44 % of the extent at 47 °N over 417 km. The
/// bound here was widened to that measured disagreement and said so.
///
/// It is not widened now. `render_gate` asks
/// [`crate::beam::great_circle_destination`] where a gate is — the exact
/// inverse of the function reading it back — so the two models are one model
/// and the residual is the pixel grid alone. What is left over is a *negative*
/// excess at every fixture and latitude below, i.e. the outermost painted pixel
/// centre sits inside the extent, which is what a truncating cast onto a pixel
/// grid does.
///
/// # Why three latitudes and why a filled disc
///
/// The error this replaced was **latitude-dependent** — 2.71 km at 26 °N
/// against 6.01 km at 47 °N over the same 417 km — so a test at one site would
/// have watched it shrink rather than go. The three below are the fixture's own
/// site and the two highest latitudes the WSR-88D fleet reaches, KMSX (47.04 °N)
/// and KATX (48.19 °N); a latitude-shaped regression cannot hide from the pair
/// at the top.
///
/// Both fixtures are **filled discs**, because a disc's outermost painted
/// pixels are at the reach whatever happened inside it. That is exactly the
/// property [`tdwr_long_range_sweep`]'s band is built *not* to have, which is
/// why the band is not used here and [`tdwr_filled_long_range_sweep`] exists.
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
            // One pixel: a pixel's *centre* is what is measured and a gate
            // reaching exactly the extent claims the pixel it starts in.
            let slop = 1.0 / (types::IMAGE_SIZE as f64 / (2.0 * extent_km));
            assert!(
                furthest <= extent_km + slop,
                "{why} at {site_lat}\u{b0}N: a pixel {furthest:.3} km out on a \
                 raster declaring {extent_km:.3} km, past the one-pixel \
                 ({slop:.3} km) bound by {:.3} km",
                furthest - extent_km - slop,
            );
            // And the disc really does reach the edge, so the line above is
            // measuring the frame and not an empty margin.
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

/// The same rule on the derived grids, which have no declaration to read: a
/// 36° NROT sector must stop at its own edge.
///
/// NROT used to take its row width as `360 / rows`, which is the spacing
/// only if the grid closes the circle — a 72-row sector came out at 5° a row
/// and smeared 2.5° past both ends of the arc it was computed over.
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

    // On the grid's own frame. NROT is a derived product with its own reach,
    // which is not `l2_sweep`'s, and a probe on the wrong frame would land in
    // an arbitrary pixel and pass by being unpainted for the wrong reason.
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
/// What a render reports is the extent it was projected at, which is now the
/// reach itself, so each arm's right and wrong answers are four distinct
/// numbers. 1200 gates at the ICD's 0.25 km is 300 km; believing the packet
/// instead would ask for 1201 km and be held at [`types::MAX_EXTENT_KM`]. 460
/// gates at the packet's own ~1.001 km is 460.5 km; a spurious override would
/// collapse it to 115 km. The arms used to need enough gates to clear a 230 km
/// floor before any of that was visible; they no longer do, and the gate counts
/// are kept as they are because they are the real products' own.
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
    // The radars this renders against; there are none until a test asks.
    crate::sites::fixture::install();
    // KTLX: 1214 ft of ground under a 62 ft tower.
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
    // The radars this renders against; there are none until a test asks.
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

    let base = render_radar_to_image_full(
        &scan,
        L2_ELEVATION,
        PRODUCT,
        LAT,
        LON,
        None,
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
        None,
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
        None,
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

/// What the second number buys, measured: a long-range raster is never
/// meaningfully coarser than the floor, where a base-size one would be less
/// than half as fine.
///
/// The floor is 2048 px over 460 km — 4.4522 px/km, or 1.11 pixels across a
/// 250 m gate — and that figure is what every visual judgement this display
/// has ever been checked against was made at. Stretching the *base* raster
/// over a surveillance cut gives 2.2255 px/km, 0.56 pixels a gate, so gates
/// start sharing pixels rather than owning them; 4096 px gives 4.4510 and
/// 1.11, which is the floor's own scale to a third of a thousandth.
///
/// The ratio is not flat, and pretending it were would be the wrong claim: the
/// side steps once at the floor while the extent is continuous, so a Doppler
/// cut comes out **finer** than the floor at 6.82 px/km. Finer is free — the
/// raster costs the same pixels wherever they land — and what matters is only
/// that it is never much coarser.
///
/// The rows are the reaches a gate count really produces, first gate included,
/// rather than the round numbers this table used to carry: a Doppler cut is
/// 2.125 + 1192 × 0.25 = 300.125 and not 298, a surveillance cut 460.125 and
/// not 458. The distinction is not cosmetic. At 458 the long-range raster
/// scores 4.4716 px/km and clears the floor outright; at the 460.125 a radar
/// actually flies it scores 4.4510 and sits **0.027% under** it, so the claim
/// this test makes has to be "never meaningfully coarser" with a stated bar,
/// and `>=` was only ever passing because the input was 2.125 km short.
///
/// | reach   | side | px/km  | vs floor | base raster would be |
/// |---------|-----:|-------:|---------:|---------------------:|
/// | 300.125 | 4096 | 6.8238 |  +53.3 % |               3.4119 |
/// | 417     | 4096 | 4.9113 |  +10.3 % |               2.4556 |
/// | 460.125 | 4096 | 4.4510 |   −0.0 % |               2.2255 |
/// | 470     | 4096 | 4.3574 |   −2.1 % |               2.1787 |
///
/// The last row is [`types::MAX_EXTENT_KM`], which is a guard on arithmetic
/// rather than a reach any radar has — the widest real sweep is the 460.125 km
/// row — so it is where the picture is furthest under the floor, and it is
/// stated rather than excluded.
#[test]
fn the_long_range_raster_keeps_the_floors_km_per_pixel() {
    /// How far under the floor a long-range raster may land before "keeps the
    /// floor's km per pixel" stops being an honest description. The widest
    /// real sweep is 0.027% under; the arithmetic cap, which no radar reaches,
    /// is 2.13% under and is asserted separately below.
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

    // The widest real sweep, in the unit that decides whether a gate is drawn
    // or shared: pixels across one 250 m super-resolution gate. The base
    // raster gives it half a pixel; the long-range one gives it its own.
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

/// The Doppler half of a WSR-88D split cut, at super-resolution: 1192 gates of
/// 250 m from a first gate 2.125 km out.
///
/// The three numbers are the RDA's, not a guess at them, and they are what
/// puts this sweep outside [`types::BASE_EXTENT_KM`]'s floor: 2.125 + 1192 ×
/// 0.25 is 300.125 km. Measured identical on eight sites across three coverage
/// patterns — KCBW, KESX, KICT, KMPX, KUDX, KCRP, KFTG, KDMX; VCP 35, 212 and
/// 215 — where it is the geometry of every velocity tilt from the lowest up to
/// about 3°, and the RDA states the intent itself: those elevation cuts carry
/// `super_resolution_doppler_to_300km`.
const DOPPLER_GATES: u16 = 1192;
/// See [`DOPPLER_GATES`].
const DOPPLER_GATE_M: u16 = 250;
/// See [`DOPPLER_GATES`].
const DOPPLER_FIRST_GATE_M: u16 = 2125;

/// The ground a Doppler cut covers, km: its 300.125 km of beam laid down at
/// the fixture's own elevation, for the reason [`tdwr_ground_reach_km`] gives.
fn doppler_ground_reach_km() -> f64 {
    // Widened before multiplying: 1192 × 250 is 298 000, and the three
    // constants are the `u16` fields `MomentData` stores them in.
    let slant_km = (f64::from(DOPPLER_FIRST_GATE_M)
        + f64::from(DOPPLER_GATES) * f64::from(DOPPLER_GATE_M))
        / 1000.0;
    slant_km * f64::from(L2_ELEVATION).to_radians().cos()
}

/// A filled Doppler cut: every gate of every radial carries a velocity.
///
/// Filled rather than the thin ring [`tdwr_long_range_sweep`] paints, because
/// what is read off this fixture is the scale of the whole picture rather than
/// where one far return lands, and a filled disc puts a gate under every probe.
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

/// A ceiling at the base size draws a Doppler cut's extra ground and pays for
/// it in scale. This is where that costing is written down.
///
/// It is the case the floor's guarantee does *not* cover, and it is not a
/// corner: a Doppler cut reaches 300.125 km, so every velocity tilt below about
/// 3° is in it. A browser is always in it — its class pins the raster ceiling
/// to the base size, whatever the browser reports — and so is a GLES 3.0
/// handheld whose `AppState::raster_side_ceiling_px` comes back at the base
/// size because that is all it said it could hold.
///
/// **The two ceilings declare the same extent**, and that is the first
/// assertion because it is the decision the rest follows from: how much ground
/// a picture shows is a fact about the data, and only how finely it is sampled
/// is a fact about the device. A raster whose extent moved with the machine
/// would put the same volume's echo in two places, and a loop frame — which
/// takes a leaner ceiling on purpose — would crop the still frame it replaces
/// rather than merely softening it.
///
/// **What it costs**: 3.412 px/km against the long-range arm's 6.824, which is
/// 23.4% under the floor's 4.4522, and 0.853 pixels across a 250 m gate where
/// the floor gives 1.113.
///
/// **What it buys**, and why the trade goes this way rather than holding these
/// devices at the floor's extent: over 192 Doppler sweeps of the eight sites
/// [`DOPPLER_GATES`] names, the band from 230 km out to 300 km holds 448 690
/// gates carrying a velocity — 1.68% of that band's own bins, the rest being
/// below threshold, but 3.4% of all the velocity those sweeps hold. Sparse and
/// real. Holding the floor would trade a picture that is uniformly softer for
/// one missing its outer third.
///
/// **What keeps that affordable** is the last block: two pixels per kilometre
/// is where a 250 m gate stops landing in a pixel at all
/// (`a_quarter_kilometre_gate_still_gets_its_own_pixel_at_the_base_extent`), and the
/// base-size arm clears it at every extent this display can be handed — down to
/// 2.1787 px/km at [`types::MAX_EXTENT_KM`], which is 8.9% of margin and the
/// reason that cap cannot quietly rise.
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
            None,
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
        (types::IMAGE_SIZE, 3.412, "a base-size ceiling"),
        (LONG_RANGE_SIDE, 6.824, "a long-range ceiling"),
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

    // The margin the trade runs on, at every extent a base-size ceiling can be
    // handed: the widest sweep a radar flies, and the arithmetic guard past it.
    const RESOLUTION_LINE: f64 = 2.0;
    for (extent, expected, why) in [
        (extent_km, 3.412, "this Doppler cut"),
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
        None,
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

/// A range-folded gate is a reading, and the plan view says so.
///
/// The sweep is 360 radials of ordinary velocity with two of them replaced:
/// one whose gates are all `RAW_RANGE_FOLDED` and its neighbour whose gates
/// are all `RAW_BELOW_THRESHOLD`. Both used to leave the same hole. They are
/// two different statements — one says the echo came from beyond the
/// unambiguous range, the other says nothing came back at all — and the
/// cross-section painter has always drawn the difference.
///
/// The purple is [`crate::palette::RANGE_FOLDED`] exactly, which is what a
/// colour no product scale can produce is for:
/// `the_range_folded_colour_is_unreachable_through_any_products_scale` pins
/// that, so a pixel this colour can only have come from this arm.
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

/// The purple is confined to the gates that declared themselves folded: a
/// sweep with none of them holds no pixel of that colour anywhere.
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

/// Two tilts, two PRFs, and the render reports the one belonging to the sweep
/// it drew.
///
/// The lookup is by the RDA's `elevation_number` off the chosen `Sweep`, which
/// is why this path resolves through `find_sweep_owner`: a table keyed by cut
/// has to be read with the cut's own number, and asking the raster which tilt
/// it drew is the only way to know which entry that is.
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
        None,
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
        None,
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
        None,
        None,
        None,
        &partial,
    )
    .expect("the low tilt renders");
    assert_eq!(
        unnamed.nyquist_ms, None,
        "an entry for another cut is not this cut's",
    );

    // A volume product has no one sweep behind it to have declared anything.
    let volume = render_radar_to_image_full(
        &scan,
        0.5,
        types::RadarProduct::EchoTopsInterpolated,
        LAT,
        LON,
        None,
        None,
        None,
        &declared,
    )
    .expect("echo tops render");
    assert_eq!(volume.nyquist_ms, None);
}

/// Two velocity tilts carrying the RDA's cut numbers 1 and 2 — the shape
/// [`the_render_reports_the_declared_nyquist_of_the_sweep_it_drew`] needs and
/// [`l2_sweep`]'s single sweep cannot be.
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

/// What a *used* buffer looks like. Not zero, and not a byte any correct render
/// could leave behind either, so one of them surviving into a checkout is
/// unambiguous.
const POISON: u8 = 0xA5;

/// `checkout_image` hands out a buffer indistinguishable from a fresh
/// allocation, at every length relative to the one the slot was holding.
///
/// # Why this is a unit test and not only an end-to-end one
///
/// `tests/render_output_pool.rs` renders through the public entry and *does*
/// have teeth on the texture — [`RenderBuffers::into_output`]'s colouring pass
/// has no `else` arm, so an unpainted pixel keeps whatever the buffer arrived
/// holding, and a blank render following a painted one shows the difference.
/// But it can only reach the lengths a raster side actually takes, and it says
/// nothing at all about the value grid, which `extend` overwrites element for
/// element and where an end-to-end assertion is therefore vacuous — the trap
/// this campaign has been caught by three times. This poisons the slot directly
/// at seven lengths around the one being asked for, which is the only way to
/// fail a checkout that is right for the sides production happens to use and
/// wrong for the arithmetic.
#[test]
fn a_checked_out_texture_is_zero_at_every_length() {
    let want = 4096;
    // Shorter than, equal to and longer than the request, plus both degenerate
    // ends. A buffer *longer* than the request is what a grow-only fit gets
    // wrong; a shorter one is what a fit that trusts the slot's own length gets
    // wrong.
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
    // And with nothing in the slot, which is every render before the first one
    // that gives a buffer back — and every render in a browser.
    let _ = super::image_pool().take();
    let image = super::checkout_image(want);
    assert_eq!(image.len(), want);
    assert!(image.iter().all(|&b| b == 0));
}

/// `checkout_values` hands out an **empty** grid whatever the slot was holding,
/// so a longer grid from a previous render cannot survive past the end of this
/// one's.
///
/// The grid is filled by `extend`, which writes every element it produces, so
/// unlike the texture there is no seeded state here to leak. The failure this
/// pins is the other one: a checkout that kept the slot's length would leave
/// the render *appending* to it, and every grid after the first would come out
/// longer than the raster it describes — which
/// `crate::render_input::tests`'s shape assertions would then read as a
/// different raster side.
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

// What `recycle_image` and `recycle_values` do *to the slot* — keep the first
// offer, drop the rest, decline a buffer with no capacity — is pinned in
// `tests/render_output_slot.rs` and deliberately not here. A test of that shape
// has to assert the slot's exact contents, and this binary's twenty-odd other
// rendering tests take and fill both slots on other threads while it runs. It
// was never observed failing — forty-five clean runs of the whole binary all
// passed — and it fails anyway: with one thread checking out and recycling
// beside it, the assertion read the wrong answer 2,017 times in 200,000, which
// puts an ordinary run somewhere around one in a thousand. That is the flake
// nobody sees for a year and then cannot reproduce. The two tests
// above are the shape that survives here, and it is not an accident — both
// assert only what a checkout must satisfy *whatever* the slot held, so no
// interleaving can make them fail. See that file for how a separate process
// gets the exact-contents claim back without the race.

/// The rasterizer's own placement is [`crate::beam::great_circle_destination`]
/// carried into pixels — the same point, arrived at by two routes.
///
/// `MercatorProjection::pixel_at` does not *call* `great_circle_destination`:
/// it inlines the arithmetic with the site's sine and cosine hoisted onto the
/// projection and the destination's latitude left as a sine, because it runs
/// tens of millions of times a frame and an `asin` there would be undone by a
/// `tan` immediately afterwards. That is a performance spelling of a shared
/// model, and this is what keeps it one: the gate's geography goes through
/// `beam`, then through [`types::ImageBounds`] the way the frontend places the
/// texture and the way `painted_ranges_km_at` reads a pixel back, and lands on
/// the column and row the rasterizer computed.
///
/// Sub-thousandth-of-a-pixel, not sub-pixel: the point is that the two are the
/// same arithmetic in a different order, so anything a rounding difference
/// cannot explain is a second model.
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
                    let (sin_d, cos_d) = (range_km / types::EARTH_RADIUS_KM).sin_cos();
                    let (px, py) = proj.pixel_at(sin_az, cos_az, sin_d, cos_d);

                    // The same gate, placed by `beam` and framed by the bounds.
                    let (lat, lon) =
                        crate::beam::great_circle_destination(site_lat, LON, az, range_km);
                    let want_px =
                        (lon - bounds.min_lon) / (bounds.max_lon - bounds.min_lon) * side as f64;
                    let merc_y = types::lat_rad_to_mercator_y(lat.to_radians());
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

/// A cut whose leading radial lost the product's moment is still offered to
/// the loop's snap.
///
/// [`find_closest_elevation`] is what `rustdar_frontend`'s loop dispatch asks
/// which elevation each historical scan actually holds. Asked of the leading
/// radial alone, one blank radial took the whole cut out of that answer, so a
/// steady selection snapped to a *neighbouring* tilt — or, on a volume with no
/// neighbour, to nothing — while every other radial of the cut carried the
/// moment. It is the same first-radial assumption
/// [`find_sweep_owner`] and [`crate::velocity::tilts`] dropped.
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

/// A split TDWR-shaped volume: a surveillance half carrying only reflectivity
/// out to 417 km and a Doppler half carrying both out to 88.8 km, at one
/// elevation, the Doppler half last. `stray` gives the surveillance half's
/// *n*-th radial a velocity moment it has no business carrying.
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

/// One stray velocity radial does not turn a surveillance cut into a Doppler
/// one, and so does not reframe the reflectivity pane.
///
/// The surveillance preference asks whether a sweep is the split cut's Doppler
/// half. Asked as `any`, one radial out of 360 answered yes for the
/// surveillance half, the preference then found no surveillance cut at all, and
/// the fallback handed reflectivity the newest sweep carrying it — the Doppler
/// half, 88.8 km wide. That is the same 328 km reframing, in the same
/// direction, as the blank-leading-radial defect the `any` was introduced to
/// fix, so the question is the sweep's majority.
///
/// The blank radial is run at both ends: in front, where a first-radial test
/// would have been fooled, and in the middle, where an `any` test is.
#[test]
fn one_stray_velocity_radial_does_not_reframe_the_reflectivity_pane() {
    let cos_e = f64::from(L2_ELEVATION).to_radians().cos();
    let long_range_km = TDWR_GATES as f64 * TDWR_GATE_KM * cos_e;

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

    // And the Doppler half is still recognised as one: a velocity request
    // reaches it, framed at its own much shorter reach.
    let scan = split_volume_with_stray_velocity(Some(200));
    let vel = render_radar_to_image(&scan, L2_ELEVATION, types::RadarProduct::Velocity, LAT, LON)
        .expect("the Doppler half carries velocity");
    let doppler_km = TDWR_DOPPLER_GATES as f64 * TDWR_DOPPLER_GATE_KM * cos_e;
    assert!((vel.max_range_km - doppler_km).abs() < 1e-9);
}

// ── The polar field against the raster it was painted beside ────────────────

/// The readout's gate **is** the raster's gate, everywhere the raster is not
/// quantizing — and where it is, the disagreement is one cell wide.
///
/// This is the test that keeps [`polar`]'s inverse and
/// [`MercatorProjection::render_gate`]'s forward paint describing one picture.
/// Nothing else can: the module's own tests run on geometry written down by
/// hand, and a rule that had drifted half a gate out would still pass every one
/// of them.
///
/// # Two claims, because there are two questions
///
/// **At a cell's centre the two agree exactly, with no tolerance.** A point at
/// a wedge's own azimuth and a gate's own range sits deep inside that gate's
/// footprint, so the pixel under it was claimed by that gate and no other, and
/// the number the grid holds there is that gate's to the bit. A rule off by
/// half a gate in range, or half a wedge in azimuth, moves the answer to a
/// neighbour at *every one* of these probes; this is what would catch it.
///
/// **Away from the centres they disagree on 2.70% of points, and that is the
/// raster's quantization rather than a second rule.** Measured over 263,683
/// probes on a 1° × 0.25 km sweep drawn at 2048 px (6.83 px/km), stepping
/// azimuth by 0.37° and range geometrically by 3%: 7,114 disagreements, and in
/// 7,071 of them — **99.4%** — the number the grid holds is one of the eight
/// gates immediately around the one the point is in. The differences are one
/// data level: median 0.500 dBZ, p90 0.500, p99 2.000, worst 4.500.
///
/// That is exactly what a truncating rasterizer does. `render_gate` walks
/// sample points and drops them onto a pixel grid nothing aligns them to, so a
/// pixel straddling a cell boundary goes to whichever claimant `write_key`
/// ranks highest rather than to whichever covers most of it. The reader cannot
/// see which; the cursor's own gate is the honest answer, and it is the one
/// this returns.
///
/// The bound is what makes the number an assertion rather than a note. A rule
/// half a gate out disagrees on roughly half the probes, not a fortieth.
///
/// # A raster defect this found, which is not fixed here
///
/// The centre half tolerates a small number of cell centres where the raster is
/// **unpainted under a gate that was painted** — 4 of 2,028 (0.20%) on the
/// coarse render. They are single-pixel pinholes: a 5 x 5 neighbourhood around
/// one reads `##.##` with the hole in the middle, inside a solid echo.
///
/// The cause is `render_gate`'s sample lattice. Its two step counts are
/// `ceil(len_px) + 2` per axis, which makes each step under a pixel — 0.622 px
/// radially and 0.910 px tangentially at (308.5 deg, 347 km) — but those axes
/// are radial and tangential, not the raster's, and a lattice whose *covering
/// radius* exceeds half a pixel can miss a pixel square entirely when it lies
/// diagonally across it. `hypot(0.622, 0.910) / 2` is **0.551 px**, over the
/// 0.5 a unit square needs, and all four holes measured 0.549-0.560.
///
/// So a hover over one of those pixels reads nothing today, in the middle of an
/// echo. The polar field has no pinholes — it is the measurements, not a
/// resampling of them — so this change removes the symptom from the readout
/// without touching the cause. The cause is a sample-density change in the
/// rasterizer's innermost loop: raising the steps to `ceil(len_px * 1.5) + 2`
/// puts the covering radius at 0.37 px, and costs 2.25x the samples in the loop
/// `POOLED_CELLS` measures at 233 ms of a browser frame, on top of moving every
/// picture the display has ever drawn. That is its own change with its own
/// measurement, and it is deliberately not made here.
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
    //
    // On a *second* render of the same field, at 1 km gates rather than
    // 0.25 km. The fine render cannot answer this question at all: the raster
    // is 2.0 texels per gate by construction ([`types::TEXELS_PER_SAMPLE`]), so
    // a gate is two texels deep and its centre falls on the seam between them —
    // the worst probe point there is, and one where the neighbouring gate's
    // claim on the pixel is as good as this gate's. Widen the gate and the cell
    // becomes 4.36 texels deep at the display's calibrated scale, with a
    // genuine interior for a point to be inside of.
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
        // From 50 km out. Inside that a 1° wedge is under four texels wide and
        // the same seam problem returns in the other axis.
        for gate in (50..cgeom.gates()).step_by(11) {
            let km = cgeom.first_gate_km() + gate as f64 * cgeom.gate_interval_km();
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
    assert!(
        pinholes * 200 <= centres,
        "{pinholes} of {centres} cell centres are unpainted in the raster — over \
         0.5%, which is more than the rasterizer's known pinholes"
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
