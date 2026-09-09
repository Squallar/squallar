use super::*;
use crate::sampler::{LadderChoice, resolve_ladder};
use crate::types::{MomentSlot, RadarProduct};
use nexrad_model::data::{
    ChannelConfiguration, ElevationCut, MomentData, PulseWidth, Radial, RadialStatus, Scan,
    WaveformType,
};

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

fn vcp(number: u16, cut_angles: &[f64]) -> VolumeCoveragePattern {
    VolumeCoveragePattern::new(
        number,
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

fn moment() -> MomentData {
    MomentData::from_fixed_point(4, 2125, 250, 8, 2.0, 66.0, vec![100, 110, 120, 130])
}

fn sweep_of(
    elevation_number: u8,
    elevation_deg: f32,
    collected_ms: i64,
    n_radials: u16,
    refl: bool,
    vel: bool,
) -> Sweep {
    let spacing = 360.0 / f32::from(n_radials);
    let radials = (0..n_radials)
        .map(|i| {
            Radial::new(
                collected_ms + i64::from(i),
                i + 1,
                f32::from(i) * spacing,
                spacing,
                RadialStatus::IntermediateRadialData,
                elevation_number,
                elevation_deg,
                refl.then(moment),
                vel.then(moment),
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

fn sweep(
    elevation_number: u8,
    elevation_deg: f32,
    collected_ms: i64,
    refl: bool,
    vel: bool,
) -> Sweep {
    sweep_of(elevation_number, elevation_deg, collected_ms, 8, refl, vel)
}

/// The split-cut table the fixtures fly: surveillance and Doppler halves
/// at 0.5° and 0.9°, a single 1.3° cut, a SAILS repeat of the 0.5° pair,
/// and a 1.8° top.
const TABLE: [f64; 8] = [0.5, 0.5, 0.9, 0.9, 1.3, 0.5, 0.5, 1.8];

fn is_doppler(number: u8) -> bool {
    matches!(number, 2 | 4 | 7)
}

fn base_volume(t0: i64) -> Scan {
    let sweeps = (1..=8u8)
        .map(|n| {
            sweep(
                n,
                TABLE[usize::from(n) - 1] as f32,
                t0 + i64::from(n) * 1000,
                true,
                is_doppler(n),
            )
        })
        .collect();
    Scan::new(vcp(212, &TABLE), sweeps)
}

fn overlay_volume(t0: i64, sealed: u8) -> Scan {
    let sweeps = (1..=sealed)
        .map(|n| {
            sweep_of(
                n,
                TABLE[usize::from(n) - 1] as f32,
                t0 + 60_000 + i64::from(n) * 1000,
                12,
                true,
                is_doppler(n),
            )
        })
        .collect();
    Scan::new(vcp(212, &TABLE), sweeps)
}

fn chosen_stamp(current: &CurrentVolume<'_>, slot: MomentSlot, key: f64) -> i64 {
    let choices = resolve_ladder(current.pattern().elevation_cuts(), current.sweeps(), slot)
        .expect("the fixture's ladder resolves");
    let LadderChoice { chosen, .. } = choices
        .into_iter()
        .find(|c| c.key == key)
        .expect("the rung exists");
    current.sweeps()[chosen].radials()[0].collection_timestamp()
}

#[test]
fn an_overlay_sweep_supersedes_the_base_sweep_of_its_cut() {
    let base = base_volume(0);
    let overlay = overlay_volume(0, 2);
    let current =
        resolve(Some((&base).into()), Some((&overlay).into())).expect("both volumes exist");

    assert_eq!(current.base_sweeps(), 6);
    assert_eq!(current.overlay_sweeps(), 2);
    let numbers: Vec<u8> = current
        .sweeps()
        .iter()
        .map(|s| s.elevation_number())
        .collect();
    assert_eq!(numbers, vec![3, 4, 5, 6, 7, 8, 1, 2]);
    let stamps: Vec<i64> = current
        .sweeps()
        .iter()
        .map(|s| s.radials()[0].collection_timestamp())
        .collect();
    assert!(
        stamps[..6].iter().all(|&t| t < 60_000),
        "the first six sweeps are the base's"
    );
    assert!(
        stamps[6..].iter().all(|&t| t > 60_000),
        "the last two are the overlay's"
    );
}

#[test]
fn the_ladder_prefers_the_overlay_sweep_over_the_base_sails_repeat() {
    let base = base_volume(0);
    let overlay = overlay_volume(0, 1); // the 0.5° surveillance half only
    let current =
        resolve(Some((&base).into()), Some((&overlay).into())).expect("both volumes exist");

    let refl = chosen_stamp(&current, MomentSlot::Reflectivity, 0.5);
    assert_eq!(refl, 61_000, "the overlay's cut-1 sweep wins the rung");

    let vel = chosen_stamp(&current, MomentSlot::Velocity, 0.5);
    assert_eq!(vel, 7_000, "the base's cut-7 sweep still carries velocity");

    let overlay2 = overlay_volume(0, 2);
    let current2 =
        resolve(Some((&base).into()), Some((&overlay2).into())).expect("both volumes exist");
    let vel2 = chosen_stamp(&current2, MomentSlot::Velocity, 0.5);
    assert_eq!(vel2, 62_000, "the overlay's cut-2 sweep takes velocity");
}

#[test]
fn a_vcp_change_drops_the_base_rather_than_mixing_two_geometries() {
    let base = base_volume(0);
    let overlay = Scan::new(
        vcp(35, &[0.9, 1.3, 1.8]),
        vec![
            sweep(1, 0.9, 60_000, true, false),
            sweep(2, 1.3, 61_000, true, false),
        ],
    );
    let current =
        resolve(Some((&base).into()), Some((&overlay).into())).expect("both volumes exist");
    assert_eq!(current.base_sweeps(), 0, "no base sweep keys onto VCP 35");
    assert_eq!(current.sweeps().len(), 2);
    assert_eq!(
        current.pattern().pattern_number().number(),
        35,
        "the current flight's pattern is the authority"
    );
}

#[test]
fn an_adaptive_tilt_move_drops_only_the_moved_cuts() {
    let base = base_volume(0);
    // Same VCP number, same table — except the base tilt moved to 0.4°, which
    // moves its Doppler half and both SAILS revisits with it.
    let mut moved = TABLE;
    moved[0] = 0.4;
    moved[1] = 0.4;
    moved[5] = 0.4;
    moved[6] = 0.4;
    let overlay = Scan::new(vcp(212, &moved), vec![sweep(1, 0.4, 60_000, true, false)]);
    let current =
        resolve(Some((&base).into()), Some((&overlay).into())).expect("both volumes exist");
    let numbers: Vec<u8> = current
        .sweeps()
        .iter()
        .map(|s| s.elevation_number())
        .collect();
    // Base cuts 1, 2, 6 and 7 — the 0.5° family under the old table — no longer
    // describe cuts the new table declares at those indexes; 3, 4, 5 and 8 do.
    assert_eq!(numbers, vec![3, 4, 5, 8, 1]);
    assert_eq!(current.base_sweeps(), 4);
}

#[test]
fn an_overlay_without_its_pattern_contributes_nothing() {
    let base = base_volume(0);
    let overlay = Scan::new(vcp(0, &[]), vec![sweep(1, 0.5, 60_000, true, false)]);
    let current = resolve(Some((&base).into()), Some((&overlay).into())).expect("the base exists");
    assert_eq!(current.base_sweeps(), 8);
    assert_eq!(current.overlay_sweeps(), 0);
    assert_eq!(
        current.pattern().elevation_cuts().len(),
        8,
        "the pattern is the base's, not the placeholder"
    );
}

#[test]
fn resolve_covers_every_absence() {
    let base = base_volume(0);
    let overlay = overlay_volume(0, 2);

    let base_only = resolve(Some((&base).into()), None).expect("base alone resolves");
    assert_eq!(base_only.base_sweeps(), 8);
    assert_eq!(base_only.overlay_sweeps(), 0);

    let overlay_only = resolve(None, Some((&overlay).into())).expect("overlay alone resolves");
    assert_eq!(overlay_only.base_sweeps(), 0);
    assert_eq!(overlay_only.overlay_sweeps(), 2);

    assert!(resolve(None, None).is_none());
}

#[test]
fn the_newest_data_time_is_the_overlay_seal_not_the_base() {
    let base = base_volume(0);
    let overlay = overlay_volume(0, 2);
    let current =
        resolve(Some((&base).into()), Some((&overlay).into())).expect("both volumes exist");
    let newest = current.newest_data_time().expect("radials carry stamps");
    // The overlay's cut-2 sweep's last radial: 60_000 + 2000 + 11 ms.
    assert_eq!(
        newest,
        chrono::DateTime::from_timestamp_millis(62_011)
            .expect("a real stamp")
            .naive_utc()
    );
}

// ── The re-cut key ──────────────────────────────────────────────────────

#[test]
fn a_doppler_half_seal_leaves_the_reflectivity_fingerprint_alone() {
    let base = base_volume(0);
    let one_sealed = overlay_volume(0, 1);
    let two_sealed = overlay_volume(0, 2);
    let before = resolve(Some((&base).into()), Some((&one_sealed).into())).expect("resolves");
    let after = resolve(Some((&base).into()), Some((&two_sealed).into())).expect("resolves");

    let refl_before = before.ladder_fingerprint(RadarProduct::Reflectivity);
    let refl_after = after.ladder_fingerprint(RadarProduct::Reflectivity);
    assert!(refl_before.is_some());
    assert_eq!(
        refl_before, refl_after,
        "the Doppler half changes no reflectivity rung, so no re-cut"
    );

    let vel_before = before.ladder_fingerprint(RadarProduct::Velocity);
    let vel_after = after.ladder_fingerprint(RadarProduct::Velocity);
    assert!(vel_before.is_some());
    assert_ne!(
        vel_before, vel_after,
        "the same seal is a real change for velocity and must re-cut"
    );
}

#[test]
fn a_surveillance_seal_moves_the_reflectivity_fingerprint() {
    let base = base_volume(0);
    let two_sealed = overlay_volume(0, 2);
    let three_sealed = overlay_volume(0, 3);
    let before = resolve(Some((&base).into()), Some((&two_sealed).into())).expect("resolves");
    let after = resolve(Some((&base).into()), Some((&three_sealed).into())).expect("resolves");
    assert_ne!(
        before.ladder_fingerprint(RadarProduct::Reflectivity),
        after.ladder_fingerprint(RadarProduct::Reflectivity),
        "cut 3 is a surveillance half: its seal replaces the 0.9° rung"
    );
}

#[test]
fn the_fingerprint_is_stable_across_a_snapshot_rebuild() {
    let base = base_volume(0);
    let overlay_a = overlay_volume(0, 2);
    let overlay_b = overlay_volume(0, 2);
    let a = resolve(Some((&base).into()), Some((&overlay_a).into())).expect("resolves");
    let b = resolve(Some((&base).into()), Some((&overlay_b).into())).expect("resolves");
    assert_eq!(
        a.ladder_fingerprint(RadarProduct::Reflectivity),
        b.ladder_fingerprint(RadarProduct::Reflectivity)
    );
}

#[test]
fn a_pattern_change_moves_the_fingerprint_even_with_the_same_sweeps() {
    let sweeps = vec![sweep(1, 0.5, 1_000, true, false)];
    let flown = Scan::new(vcp(212, &[0.5, 1.8]), sweeps.clone());
    let taller = Scan::new(vcp(212, &[0.5, 1.8, 6.4]), sweeps);
    let a = resolve(None, Some((&flown).into())).expect("resolves");
    let b = resolve(None, Some((&taller).into())).expect("resolves");
    assert_ne!(
        a.ladder_fingerprint(RadarProduct::Reflectivity),
        b.ladder_fingerprint(RadarProduct::Reflectivity),
        "the declared ceiling changed; the caption must re-draw"
    );
}

#[test]
fn a_merged_payload_ports_the_ladder_it_resolved() {
    let base = base_volume(0);
    let overlay = overlay_volume(0, 2);
    let current =
        resolve(Some((&base).into()), Some((&overlay).into())).expect("both volumes exist");

    let materialized = Scan::new(
        current.pattern().clone(),
        current.sweeps().iter().map(|s| (*s).clone()).collect(),
    );

    for product in [RadarProduct::Reflectivity, RadarProduct::Velocity] {
        let direct = crate::sampler::VolumeSampler::new(&materialized, product)
            .expect("the merged ladder builds");
        assert!(
            format!("{direct:?}").contains(" 12x"),
            "precondition: no overlay rung in the direct ladder: {direct:?}"
        );

        let input = crate::render_input::RenderInput::extract_volume_parts(
            current.pattern(),
            current.sweeps(),
            product,
            35.33,
            -97.27,
            None,
        )
        .expect("the merge carries the moment");
        let decoded = crate::render_input::RenderInput::from_bytes(&input.to_bytes())
            .expect("the payload round-trips");
        let reconstructed = decoded.to_scan();
        let ported = crate::sampler::VolumeSampler::new(&reconstructed, product)
            .expect("the reconstructed ladder builds");

        assert_eq!(
            format!("{ported:?}"),
            format!("{direct:?}"),
            "{product:?}: the worker's merged ladder is not the app's",
        );
    }
}

// -- live ---------------------------------------------------------------
//
// Run with:
//   cargo test -p squallar-radar --release --lib -- --ignored --nocapture current::tests::live_

#[cfg(not(target_arch = "wasm32"))]
#[ignore = "hits the live nexrad archive bucket"]
#[tokio::test]
async fn live_substrate_costs_are_measured() {
    use nexrad_model::data::DataMoment;
    use std::time::Instant;

    let site = "KTLX";
    let now = chrono::Utc::now().naive_utc();
    let crate::scan::DecodedScan {
        scan,
        declared_nyquist,
    } = crate::scan::decode_bytes(crate::scan::fetch_scan(site, now).await.expect("a volume"))
        .expect("the volume decodes");
    println!("declared Nyquist velocities: {:?}", declared_nyquist);

    let position = scan
        .site()
        .and_then(crate::site_position::SitePosition::from_volume)
        .unwrap_or_else(|| {
            panic!(
                "the volume for {site} states no position, and this instrument \
                 has no other way to place it"
            )
        });
    let radar = crate::sites::resolve([(site, crate::sites::SiteFix::Learned(position))])
        .get(site)
        .expect("the row the resolve above just placed");

    let gate_bytes: usize = scan
        .sweeps()
        .iter()
        .flat_map(|s| s.radials())
        .map(|r| {
            [
                r.reflectivity(),
                r.velocity(),
                r.spectrum_width(),
                r.differential_reflectivity(),
                r.differential_phase(),
                r.correlation_coefficient(),
            ]
            .into_iter()
            .flatten()
            .map(|m| m.raw_values().len())
            .sum::<usize>()
        })
        .sum();
    println!(
        "volume: {} sweeps, {:.1} MB of gate bytes",
        scan.sweeps().len(),
        gate_bytes as f64 / 1e6
    );

    let t = Instant::now();
    let cloned = scan.clone();
    println!(
        "full Scan clone (the per-seal cost a materialised merge would pay): {:?}",
        t.elapsed()
    );
    drop(cloned);

    let t = Instant::now();
    let current = resolve(Some((&scan).into()), Some((&scan).into())).expect("resolves");
    println!(
        "current::resolve over two full volumes: {:?} ({} + {} sweeps)",
        t.elapsed(),
        current.base_sweeps(),
        current.overlay_sweeps()
    );
    let t = Instant::now();
    let newest = current.newest_data_time();
    println!("newest_data_time: {:?} -> {newest:?}", t.elapsed());
    let t = Instant::now();
    let fp = current.ladder_fingerprint(RadarProduct::Reflectivity);
    println!("ladder_fingerprint(REF): {:?} -> {fp:?}", t.elapsed());

    let t = Instant::now();
    let volume_input = crate::render_input::RenderInput::extract_volume(
        &scan,
        RadarProduct::Reflectivity,
        radar.lat,
        radar.lon,
    )
    .expect("reflectivity everywhere");
    let extract_ms = t.elapsed();
    let t = Instant::now();
    let bytes = volume_input.to_bytes();
    println!(
        "extract_volume(REF): {extract_ms:?}, payload {:.1} MB, to_bytes {:?}",
        bytes.len() as f64 / 1e6,
        t.elapsed()
    );

    for product in [
        RadarProduct::EchoTopsInterpolated,
        RadarProduct::NormalizedRotation,
        RadarProduct::StormRelativeVelocity,
        RadarProduct::HydrometeorClassification,
    ] {
        let t = Instant::now();
        let Some(input) = crate::render_input::RenderInput::extract(
            &scan, 0.5, product, radar.lat, radar.lon, None, None,
        ) else {
            println!("{product:?}: no payload (moment absent)");
            continue;
        };
        let extract_ms = t.elapsed();
        let payload_mb = input.to_bytes().len() as f64 / 1e6;
        let t = Instant::now();
        let rendered = crate::render::render_from(&input).is_some();
        println!(
            "{product:?}: extract {extract_ms:?}, payload {payload_mb:.1} MB, \
                 render {:?} (drew: {rendered})",
            t.elapsed()
        );
    }

    let request = crate::voxel::VoxelRequest {
        centre: (radar.lat, radar.lon),
        half_extent_km: Some(crate::voxel::HalfExtentKm::square(80.0)),
        base_km_msl: crate::voxel::DEFAULT_BASE_KM_MSL,
        top_km_msl: crate::voxel::DEFAULT_TOP_KM_MSL,
        product: RadarProduct::Reflectivity,
        shape: crate::voxel::DESKTOP_SHAPE,
        values_wanted: false,
    };
    let t = Instant::now();
    let grid = crate::voxel::build_voxels(&scan, &request, radar.lat, radar.lon);
    println!(
        "build_voxels(desktop shape): {:?} (built: {})",
        t.elapsed(),
        grid.is_some()
    );

    let request = crate::xsect::SectionRequest {
        start: (radar.lat - 0.5, radar.lon - 0.5),
        end: (radar.lat + 0.5, radar.lon + 0.5),
        top_km_msl: None,
        product: RadarProduct::Reflectivity,
    };
    let t = Instant::now();
    let section = crate::xsect::render_section(
        &scan,
        &request,
        radar.lat,
        radar.lon,
        crate::srv::MotionInputs::default(),
    );
    println!(
        "render_section: {:?} (cut: {})",
        t.elapsed(),
        section.is_some()
    );
}

#[test]
fn the_fingerprint_refuses_what_the_sampler_refuses() {
    let no_pattern = Scan::new(vcp(0, &[]), vec![sweep(1, 0.5, 1_000, true, false)]);
    let current = resolve(None, Some((&no_pattern).into()));
    assert!(
        current.is_none(),
        "an overlay with no pattern and no base resolves to nothing"
    );

    let base = base_volume(0);
    let current = resolve(Some((&base).into()), None).expect("resolves");
    assert!(
        current
            .ladder_fingerprint(RadarProduct::VerticallyIntegratedLiquid)
            .is_none(),
        "a Level III product has no ladder and no key"
    );
}

/// **The two frame-thread readers are functions of the LIVE overlay, not of
/// the base alone — so memoising them where the base is installed would be
/// stale within one sealed sweep.**
///
/// This refutes a route, and it is worth a test rather than a note because
/// the route is the obvious one. `App::current_ladder_fingerprint` runs per
/// section pane per frame and `App::current_volume_stamp` once a frame per
/// site; neither reads a gate byte, so "memoise them at install and the base
/// becomes withdrawable" reads like a clean win. It is not available: both go
/// through [`resolve`], which MERGES the base with the site's current flight
/// sweep by sweep, and the live chunk feed advances that flight every ~30 s.
/// A value cached where the base is installed would answer for a volume the
/// site has already grown past.
///
/// **The base does not move here at all** — same fixture, same allocation,
/// same everything — so a difference in either answer can only have come from
/// the overlay. That is the whole design of the test: an arrangement that
/// re-made the base between the two readings could not tell a stale memo from
/// a legitimately changed one.
///
/// What it does NOT refute, and the reason the enumeration was worth doing:
/// neither reader touches a moment's gate buffer. `resolve_ladder` reads cut
/// angles, elevation numbers, radial counts, the first radial's collection
/// time and whether a slot is PRESENT on it; `newest_data_time` reads
/// collection times. Gate bytes are 95.9 % of a decoded volume
/// (`scan_size`), so the route these readers actually leave open is releasing
/// the gate buffers and keeping the radial skeleton — a representation
/// change, not a memo.
#[test]
fn the_fingerprint_and_the_stamp_move_with_the_overlay_while_the_base_stands() {
    let base = base_volume(0);
    let product = RadarProduct::Velocity;

    // **Sweep 4 is the Doppler half of the 0.9 rung**, and that is why the
    // readings are taken across IT and not across any sealed sweep. The
    // fingerprint is a RE-CUT key: it is deliberately insensitive to a flight
    // advancing in a way that does not change which sweep each rung is cut
    // from, and sealing the 0.9 surveillance half (sweep 3) is exactly such
    // an advance — measured, on the first version of this test, which went
    // green on `assert_ne!` because the key correctly did not move. Sealing
    // the Doppler half moves the 0.9 velocity choice off the base and onto
    // the flight, which is a real change to what a section pane would cut.
    let three_sealed = overlay_volume(0, 3);
    let four_sealed = overlay_volume(0, 4);

    let earlier = resolve(Some((&base).into()), Some((&three_sealed).into()))
        .expect("base and overlay both exist");
    let later = resolve(Some((&base).into()), Some((&four_sealed).into()))
        .expect("base and overlay both exist");

    assert_eq!(
        earlier.overlay_sweeps(),
        3,
        "fixture: the earlier reading is not the three-sweep flight",
    );
    assert_eq!(
        later.overlay_sweeps(),
        4,
        "fixture: the flight did not advance, so there is no change here for \
         either reader to see",
    );

    let earlier_print = earlier
        .ladder_fingerprint(product)
        .expect("the fixture's ladder resolves");
    let later_print = later
        .ladder_fingerprint(product)
        .expect("the fixture's ladder resolves");
    assert_ne!(
        earlier_print, later_print,
        "the re-cut key did not move when the flight sealed a sweep, so a \
         fingerprint memoised at base install would still be correct — and \
         the section pane would be re-cutting against a key that cannot see \
         its own data arriving",
    );

    let earlier_stamp = earlier
        .newest_data_time()
        .expect("the fixture carries clocked radials");
    let later_stamp = later
        .newest_data_time()
        .expect("the fixture carries clocked radials");
    assert!(
        later_stamp > earlier_stamp,
        "the data-through stamp did not advance with the sealed sweep \
         ({earlier_stamp} -> {later_stamp}); memoising it at base install \
         would freeze every site's displayed time at its last archive volume",
    );
}

/// **Neither frame-thread reader touches a gate byte** — so the route the memo
/// refutation leaves open is releasing the gate buffers and keeping the radial
/// skeleton.
///
/// The redirect, gated rather than argued. `resolve_ladder` reads cut angles,
/// elevation numbers, radial counts, the first radial's collection time and
/// whether a moment slot is PRESENT on it; `newest_data_time` reads collection
/// times. None of that is a gate. Gate bytes are 95.9 % of a decoded volume
/// (`crate::scan_size`), and one is 33.7-82.7 MiB, so a representation that
/// kept the skeleton and released the buffers would leave both of these
/// readers answering exactly as they do now.
///
/// **The fixture is the claim.** The two volumes differ in one respect and
/// one only: every moment in the second carries an EMPTY value buffer where
/// the first carries four gates. Same cuts, same sweeps, same elevation
/// numbers, same radial counts, same collection times, and the moments are
/// still PRESENT — which is what `carries` asks. If either reader consulted a
/// gate, these two would answer differently.
///
/// It is a statement about these two readers and **not** about the volume: the
/// section cut and the 3D resample read the gates and would be destroyed by
/// the same change. They are dispatched rather than per-frame, which is why
/// they are the ones that can pay a decode.
///
/// TAMPER: hash any gate value into `ladder_fingerprint` and this fails.
#[test]
fn the_hot_readers_answer_the_same_without_a_single_gate_byte() {
    fn gateless_sweep(elevation_number: u8, elevation_deg: f32, collected_ms: i64) -> Sweep {
        // **The one difference: the BUFFER is empty while the declared gate
        // count stays 4**, which is exactly the shape a released buffer has.
        // Dropping the count to 0 as well fails this test, and correctly:
        // `ladder_fingerprint` hashes `moment.gate_count()`, which is a `u16`
        // field and not a gate. So a skeleton must carry the declared count
        // forward — that is an acceptance criterion for the route, found here
        // rather than after it had been built.
        let empty = || MomentData::from_fixed_point(4, 2125, 250, 8, 2.0, 66.0, Vec::new());
        let radials = (0..8u16)
            .map(|i| {
                Radial::new(
                    collected_ms + i64::from(i),
                    i + 1,
                    f32::from(i) * 45.0,
                    45.0,
                    RadialStatus::IntermediateRadialData,
                    elevation_number,
                    elevation_deg,
                    Some(empty()),
                    is_doppler(elevation_number).then(empty),
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

    let with_gates = base_volume(0);
    let without_gates = Scan::new(
        vcp(212, &TABLE),
        (1..=8u8)
            .map(|n| gateless_sweep(n, TABLE[usize::from(n) - 1] as f32, i64::from(n) * 1000))
            .collect(),
    );

    // The fixture's own premise: the gates really are gone, and really were
    // there. Without this the test could pass on two identical volumes.
    let gates_of = |scan: &Scan| -> usize {
        use nexrad_model::data::DataMoment;
        scan.sweeps()
            .iter()
            .flat_map(|s| s.radials())
            .filter_map(nexrad_model::data::Radial::reflectivity)
            .map(|m| m.raw_values().len())
            .sum()
    };
    assert!(
        gates_of(&with_gates) > 0,
        "fixture: the control carries no gates either, so this compares two \
         gateless volumes and proves nothing",
    );
    assert_eq!(
        gates_of(&without_gates),
        0,
        "fixture: the gateless volume still carries gate bytes",
    );

    for product in [RadarProduct::Velocity, RadarProduct::Reflectivity] {
        let full = resolve(Some((&with_gates).into()), None).expect("a base alone resolves");
        let bare = resolve(Some((&without_gates).into()), None).expect("a base alone resolves");
        assert_eq!(
            full.ladder_fingerprint(product),
            bare.ladder_fingerprint(product),
            "the re-cut key for {product:?} changed when the gate buffers \
             were released, so the fingerprint is reading gates and the \
             skeleton route is refuted for it",
        );
    }

    let full = resolve(Some((&with_gates).into()), None).expect("a base alone resolves");
    let bare = resolve(Some((&without_gates).into()), None).expect("a base alone resolves");
    assert_eq!(
        full.newest_data_time(),
        bare.newest_data_time(),
        "the data-through stamp changed when the gate buffers were released, \
         so it is reading gates",
    );
    assert!(
        full.newest_data_time().is_some(),
        "fixture: neither volume carries a clocked radial, so the equality \
         above is None == None",
    );
}

// ── What the merge has stopped wanting ──────────────────────────────────

/// **`superseded_base_sweeps` names exactly the base sweeps the merge leaves
/// out, at every seal depth there is.**
///
/// The anti-drift gate, and the reason the two share `admits_base_sweep`
/// rather than each spelling the rule. An instrument that answered this
/// question with a second copy of the admission test would, the first time
/// one of them changed, report rungs the merge is still serving as bytes a
/// release could take — a hole in the radar sized in megabytes and reported
/// as a saving.
///
/// Asserted against `resolve`'s own `base_sweeps()` and not against a table
/// of expected numbers: a pinned table is a third spelling of the same rule
/// and would drift with neither.
#[test]
fn the_superseded_sweeps_are_the_ones_the_merge_leaves_out() {
    let base = base_volume(0);
    for sealed in 0..=8u8 {
        let overlay = overlay_volume(0, sealed);
        let merged =
            resolve(Some((&base).into()), Some((&overlay).into())).expect("both volumes exist");
        let superseded = superseded_base_sweeps(&base, Some(&overlay));
        assert_eq!(
            merged.base_sweeps() + superseded.len(),
            base.sweeps().len(),
            "at {sealed} sealed cut(s) the merge took {} base sweep(s) and \
             {} were called superseded, which does not account for the \
             base's {}",
            merged.base_sweeps(),
            superseded.len(),
            base.sweeps().len(),
        );
        // Every index is a real sweep of the base, and none repeats — the
        // property a caller pricing them against a per-sweep list relies on.
        let mut seen = superseded.clone();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), superseded.len(), "an index was named twice");
        assert!(
            superseded.iter().all(|&i| i < base.sweeps().len()),
            "an index names no sweep of the base",
        );
    }
}

/// **With no live flight nothing is superseded**, which is the reading that
/// keeps a boot — where the overlay has not arrived — from being reported as
/// a whole base's worth of freeable bytes.
#[test]
fn a_base_with_no_overlay_has_superseded_nothing() {
    let base = base_volume(0);
    assert!(superseded_base_sweeps(&base, None).is_empty());
    // An overlay with no cut table is the same case: `resolve` discards it
    // before the merge, so every base sweep is still served.
    let patternless = Scan::new(vcp(212, &[]), vec![sweep(1, 0.5, 0, true, false)]);
    assert!(superseded_base_sweeps(&base, Some(&patternless)).is_empty());
    assert_eq!(
        resolve(Some((&base).into()), Some((&patternless).into()))
            .expect("resolves")
            .base_sweeps(),
        base.sweeps().len(),
        "fixture: the patternless overlay was merged after all, so the \
         assertion above is not about the case it names",
    );
}
