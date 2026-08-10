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

/// One sweep with real, distinct collection timestamps — the fingerprint
/// hashes them, so a fixture stamping every radial `0` would make two
/// different volumes' sweeps indistinguishable and the fingerprint tests
/// vacuous.
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

/// A base-volume sweep: eight radials, so the ladder's `Debug` line —
/// which prints radial counts — can tell a base sweep from an overlay one
/// inside the same rung.
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

/// Whether the cut at this 1-based number is a Doppler half in [`TABLE`].
fn is_doppler(number: u8) -> bool {
    matches!(number, 2 | 4 | 7)
}

/// A complete volume over [`TABLE`], all eight cuts, collected at `t0`.
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

/// An in-flight volume over [`TABLE`] whose cuts `1..=sealed` have sealed,
/// collected one minute after `t0`. Twelve radials against the base's
/// eight, so a ladder description names which volume a rung came from.
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

/// The first radial's collection stamp of the sweep a ladder chose for
/// `slot` at rung `key` — which volume's sweep won, in one number.
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

    // Cuts 1 and 2 sealed in the overlay, so the base's are out; the
    // base's other six fill the ladder, and the overlay's two follow.
    assert_eq!(current.base_sweeps(), 6);
    assert_eq!(current.overlay_sweeps(), 2);
    let numbers: Vec<u8> = current
        .sweeps()
        .iter()
        .map(|s| s.elevation_number())
        .collect();
    assert_eq!(numbers, vec![3, 4, 5, 6, 7, 8, 1, 2]);
    // Base first, overlay after — the order every newest-wins rule reads.
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

/// The merged ladder must take the overlay's fresh surveillance sweep for
/// reflectivity even though the *base* holds a SAILS repeat of the same
/// cut — the repeat was newer within the base, but the overlay's sweep is
/// newer than the whole base.
#[test]
fn the_ladder_prefers_the_overlay_sweep_over_the_base_sails_repeat() {
    let base = base_volume(0);
    let overlay = overlay_volume(0, 1); // the 0.5° surveillance half only
    let current =
        resolve(Some((&base).into()), Some((&overlay).into())).expect("both volumes exist");

    // Reflectivity at 0.5°: the overlay's surveillance sweep, not the
    // base's SAILS surveillance repeat (cut 6) and not any Doppler half.
    let refl = chosen_stamp(&current, MomentSlot::Reflectivity, 0.5);
    assert_eq!(refl, 61_000, "the overlay's cut-1 sweep wins the rung");

    // Velocity at 0.5°: the overlay has sealed no Doppler half yet, so
    // the newest velocity is the base's SAILS Doppler repeat (cut 7) —
    // base data honestly standing in until the overlay's arrives.
    let vel = chosen_stamp(&current, MomentSlot::Velocity, 0.5);
    assert_eq!(vel, 7_000, "the base's cut-7 sweep still carries velocity");

    // Once the overlay's Doppler half seals, it takes the rung over.
    let overlay2 = overlay_volume(0, 2);
    let current2 =
        resolve(Some((&base).into()), Some((&overlay2).into())).expect("both volumes exist");
    let vel2 = chosen_stamp(&current2, MomentSlot::Velocity, 0.5);
    assert_eq!(vel2, 62_000, "the overlay's cut-2 sweep takes velocity");
}

/// On a VCP change nothing the base flew keys truthfully onto the new
/// pattern, so the merge is the overlay alone — honest truncation until
/// the new volume fills, never a ladder stitched from two geometries.
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

/// The adaptive base tilt moves the lowest cuts between volumes of the
/// *same* VCP. Only the moved cuts drop; the rest of the base still fills
/// the ladder.
#[test]
fn an_adaptive_tilt_move_drops_only_the_moved_cuts() {
    let base = base_volume(0);
    // Same VCP number, same table — except the base tilt moved to 0.4°,
    // which moves its Doppler half and both SAILS revisits with it.
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
    // Base cuts 1, 2, 6 and 7 — the 0.5° family under the old table — no
    // longer describe cuts the new table declares at those indexes; 3, 4,
    // 5 and 8 still do. The overlay's own sweep follows.
    assert_eq!(numbers, vec![3, 4, 5, 8, 1]);
    assert_eq!(current.base_sweeps(), 4);
}

/// A volume joined mid-flight has no pattern until its start chunk lands.
/// Keying its sweeps by the base's table would be a guess about a flight
/// whose plan is unknown, so it contributes nothing.
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

/// The waste the old count-based key caused, pinned from the other side:
/// a split cut's Doppler half carries a short-range reflectivity copy, so
/// its seal used to move the reflectivity key and force a re-cut that
/// produced a byte-identical picture. The fingerprint must not move —
/// and must still move for the moment the seal *does* change.
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

/// The key must be a property of the data, not of the allocation: the
/// assembler rebuilds its snapshot `Arc` on every seal, so a key that
/// moved with the rebuild would re-cut every pane once per seal even
/// when its own rungs were untouched.
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

/// The declared table is part of the picture — the caption's ceiling is
/// drawn from it — so two states whose chosen sweeps agree but whose
/// patterns do not must not share a key.
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

/// The merged ladder survives the worker port identically. A payload
/// extracted from the merge's parts, sent through its own bytes and
/// reconstructed, builds the very ladder a sampler builds over a
/// test-side materialisation of the same merge — compared over the
/// sampler's `Debug` line, whose radial counts are what tell a base
/// sweep from an overlay sweep inside one rung (8 against 12 here).
#[test]
fn a_merged_payload_ports_the_ladder_it_resolved() {
    let base = base_volume(0);
    let overlay = overlay_volume(0, 2);
    let current =
        resolve(Some((&base).into()), Some((&overlay).into())).expect("both volumes exist");

    // Materialised the expensive way — the way production never does —
    // purely so the sampler can read the merge directly for comparison.
    let materialized = Scan::new(
        current.pattern().clone(),
        current.sweeps().iter().map(|s| (*s).clone()).collect(),
    );

    for product in [RadarProduct::Reflectivity, RadarProduct::Velocity] {
        let direct = crate::sampler::VolumeSampler::new(&materialized, product)
            .expect("the merged ladder builds");
        // precondition: the merged rung this test is about really is the
        // overlay's — a 12-radial sweep — or the comparison proves nothing
        // about the merge.
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
//   cargo test -p rustdar-radar --release --lib -- --ignored --nocapture current::tests::live_

/// Measure, on a real volume, every cost the merged-substrate design
/// weighs: the resolve itself, the fingerprint, the per-consumer
/// extractions and renders, the voxel resample, a section cut, and the
/// full-`Scan` clone a materialised merge would have paid per sealed
/// sweep. Numbers, not assertions — the assertions are only that each
/// stage runs at all.
#[cfg(not(target_arch = "wasm32"))]
#[ignore = "hits the live nexrad archive bucket"]
#[tokio::test]
async fn live_substrate_costs_are_measured() {
    use nexrad_model::data::DataMoment;
    use std::time::Instant;

    let site = "KTLX";
    let radar = crate::sites::get_radar_site(site).expect("a real site");
    let now = chrono::Utc::now().naive_utc();
    let crate::scan::DecodedScan {
        scan,
        declared_nyquist,
    } = crate::scan::get_scan(site, now).await.expect("a volume");
    println!("declared Nyquist velocities: {:?}", declared_nyquist);

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

    // The section/voxel payload: what the frame thread pays per re-cut.
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

    // Whole-volume plan products: extraction + render, per recompute.
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

    // The voxel resample at the desktop shape — the cost the worker move
    // takes off the frame thread.
    let request = crate::voxel::VoxelRequest {
        centre: (radar.lat, radar.lon),
        half_width_km: 80.0,
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

    // A section cut, end to end — the worker-side render per re-cut.
    let request = crate::xsect::SectionRequest {
        start: (radar.lat - 0.5, radar.lon - 0.5),
        end: (radar.lat + 0.5, radar.lon + 0.5),
        top_km_msl: None,
        product: RadarProduct::Reflectivity,
    };
    let t = Instant::now();
    let section = crate::xsect::render_section(&scan, &request, radar.lat, radar.lon, None);
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
