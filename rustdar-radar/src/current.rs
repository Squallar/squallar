//! The current merged volume: the latest **complete** volume a site produced,
//! overlaid by every sealed sweep of the volume now being flown.
//!
//! # Why this exists
//!
//! A volume takes 4–7 minutes to fly, and the live chunk feed delivers it a
//! sealed sweep at a time. Anything that reads a *whole* volume — a
//! cross-section, the 3D resample — therefore used to choose between two bad
//! answers: cut from the growing live volume, whose ladder starts one rung
//! tall after every roll, or cut from the last archive volume, which is
//! complete but ages while fresher sweeps sit in hand. The app nearly always
//! holds both, and together they are one honest volume: the complete base
//! fills every rung the current flight has not reached, and each sealed sweep
//! replaces its rung the moment it lands.
//!
//! # What "merge" means here — and what it never does
//!
//! [`resolve`] produces no new data. It returns the base's pattern or the
//! overlay's, and a list of *borrowed* sweeps in an order the existing
//! newest-wins rules already understand: admitted base sweeps first, overlay
//! sweeps after, so `render::find_sweep`'s `.rev()` and the sampler's
//! newest-first rung choice both prefer the sealed live sweep over the base's
//! copy of the same cut with **no new selection rule anywhere**. Sweeps are
//! not rebuilt, radials are not touched, and the split-cut discriminator —
//! which lives in each radial's own velocity field — survives untouched;
//! rebuilding radials is how `carried_velocity` was broken once before.
//!
//! # The admission rule, and the honesty line it walks
//!
//! A sweep is keyed onto the tilt ladder through
//! `pattern.elevation_cuts()[sweep.elevation_number() - 1]`, so a base sweep
//! inside a merged volume is keyed by the **overlay's** table. That is only
//! truthful where the overlay's table says, at that index, exactly what the
//! base's table said — the angle is the one thing the ladder reads. So:
//!
//! * base sweep `k` is admitted iff both tables hold index `k-1` **and**
//!   declare bit-identical angles there, and the overlay has not already
//!   sealed its own sweep `k` (which supersedes the base's outright — same
//!   cut, same role in its split pair, strictly newer);
//! * on a VCP change the indexes stop agreeing and the base drops, rung by
//!   rung or wholesale — the merged ladder then shows honest truncation until
//!   the new pattern fills, rather than a ladder stitched from two patterns'
//!   geometry;
//! * an overlay with **no pattern yet** (joined mid-flight, start chunk still
//!   missing) contributes nothing: keying its sweeps by the base's table
//!   would be a guess about a flight whose plan has not arrived. The merged
//!   volume is then the base alone, and it heals at the next volume start.
//!
//! The comparison is on the declared angles, not the VCP number. Two volumes
//! flying "the same" VCP can declare different tables — the adaptive base
//! tilt moves the lowest cuts, SAILS inserts renumber everything after them —
//! and two different VCP numbers could in principle declare equal prefixes.
//! The angles are what the ladder keys on, the caption ceiling is drawn from,
//! and the below-horizon wrap correction reads; where they agree exactly, the
//! merged keying is exact, and where they differ at all, admission would put
//! a sweep on a rung its own volume never declared.

use nexrad_model::data::{Scan, Sweep, VolumeCoveragePattern};

use crate::types::RadarProduct;

/// A site's current volume, resolved as borrows: the pattern that keys it and
/// the sweeps that fill it, base first, overlay after.
///
/// This is a *view*, rebuilt cheaply wherever it is needed, rather than a
/// materialised `Scan`: `Sweep` is not shared, so a merged `Scan` would deep-
/// copy every gate byte of both volumes on every sealed sweep — tens of
/// megabytes on the one thread the browser has. Consumers that need a `Scan`
/// get one through [`crate::render_input::RenderInput::extract_volume_parts`],
/// which copies exactly the moment it ships and nothing else.
pub struct CurrentVolume<'a> {
    pattern: &'a VolumeCoveragePattern,
    sweeps: Vec<&'a Sweep>,
    /// How many of [`Self::sweeps`] came from the base. The overlay's are the
    /// rest; the split is what a caption needs to say how much of the picture
    /// is the current flight's.
    base_sweeps: usize,
}

impl<'a> CurrentVolume<'a> {
    /// The pattern the merged sweeps are keyed by.
    pub fn pattern(&self) -> &'a VolumeCoveragePattern {
        self.pattern
    }

    /// The merged sweep list: admitted base sweeps in base order, then every
    /// keyable overlay sweep in overlay order — so a later sweep is always
    /// the newer statement of its cut.
    pub fn sweeps(&self) -> &[&'a Sweep] {
        &self.sweeps
    }

    /// How many sweeps the base contributed. Zero for a volume that is all
    /// overlay (no base yet), `sweeps().len()` for one that is all base.
    pub fn base_sweeps(&self) -> usize {
        self.base_sweeps
    }

    /// How many sweeps the current flight contributed.
    pub fn overlay_sweeps(&self) -> usize {
        self.sweeps.len() - self.base_sweeps
    }

    /// The collection time of the newest radial in the merged volume — the
    /// honest "data through" stamp for a caption, and a monotone identity for
    /// a rebuild key: every sealed sweep advances it.
    ///
    /// Off the radials' own epoch-millisecond stamps rather than
    /// `Sweep::time_range`, which sits behind a chrono feature this build does
    /// not enable. `None` only when no radial anywhere carries a positive
    /// timestamp, which no real volume produces.
    pub fn newest_data_time(&self) -> Option<chrono::NaiveDateTime> {
        self.sweeps
            .iter()
            .flat_map(|sweep| sweep.radials())
            .map(nexrad_model::data::Radial::collection_timestamp)
            .filter(|&ms| ms > 0)
            .max()
            .and_then(chrono::DateTime::from_timestamp_millis)
            .map(|dt| dt.naive_utc())
    }

    /// The re-cut key for `product` over this merged volume — see
    /// [`crate::sampler::ladder_fingerprint`]. Delegates rather than restates:
    /// the choice hashed here is the choice the sampler will make.
    pub fn ladder_fingerprint(&self, product: RadarProduct) -> Option<u64> {
        crate::sampler::ladder_fingerprint(self.pattern, &self.sweeps, product)
    }
}

/// Resolve a site's current volume from what the app holds.
///
/// `base` is the latest **complete** volume (an archive decode or a closed
/// chunk assembly — never a partial); `overlay` is the in-flight assembler's
/// snapshot, which by construction carries only sealed sweeps. `None` when
/// neither exists: the site has no volume at all yet.
///
/// The admission rule is the module doc's; this is its one implementation.
pub fn resolve<'a>(base: Option<&'a Scan>, overlay: Option<&'a Scan>) -> Option<CurrentVolume<'a>> {
    // An overlay whose pattern has no cuts cannot key its own sweeps — the
    // mid-flight-join state. It contributes nothing rather than borrowing the
    // base's table for a flight whose plan is unknown.
    let overlay = overlay.filter(|scan| !scan.coverage_pattern().elevation_cuts().is_empty());

    match (base, overlay) {
        (Some(base), Some(overlay)) => {
            let base_cuts = base.coverage_pattern().elevation_cuts();
            let overlay_cuts = overlay.coverage_pattern().elevation_cuts();
            // Elevation numbers the overlay has sealed: those cuts are
            // superseded in the base — same cut of the same declared pattern
            // index, strictly newer.
            let overlay_numbers: Vec<u8> = overlay
                .sweeps()
                .iter()
                .map(Sweep::elevation_number)
                .collect();
            let admits = |sweep: &Sweep| -> bool {
                let Some(index) = usize::from(sweep.elevation_number()).checked_sub(1) else {
                    return false;
                };
                let (Some(base_cut), Some(overlay_cut)) =
                    (base_cuts.get(index), overlay_cuts.get(index))
                else {
                    return false;
                };
                base_cut.elevation_angle_degrees() == overlay_cut.elevation_angle_degrees()
                    && !overlay_numbers.contains(&sweep.elevation_number())
            };
            let mut sweeps: Vec<&Sweep> = base.sweeps().iter().filter(|s| admits(s)).collect();
            let base_sweeps = sweeps.len();
            // Defensive symmetry: an overlay sweep its own table cannot key
            // poisons every ladder built over the merge, where the base alone
            // was fine. Real volumes never produce one; dropping it keeps a
            // corrupt cut from costing the whole picture.
            sweeps.extend(
                overlay
                    .sweeps()
                    .iter()
                    .filter(|s| keyable(overlay_cuts.len(), s)),
            );
            Some(CurrentVolume {
                pattern: overlay.coverage_pattern(),
                sweeps,
                base_sweeps,
            })
        }
        (Some(base), None) => Some(CurrentVolume {
            pattern: base.coverage_pattern(),
            sweeps: base.sweeps().iter().collect(),
            base_sweeps: base.sweeps().len(),
        }),
        // No base yet: the overlay stands alone, exactly as the growing live
        // volume always has. Its ladder is short and the captions say so.
        (None, Some(overlay)) => Some(CurrentVolume {
            pattern: overlay.coverage_pattern(),
            sweeps: overlay.sweeps().iter().collect(),
            base_sweeps: 0,
        }),
        (None, None) => None,
    }
}

/// Whether `sweep`'s elevation number indexes a table of `cut_count` cuts.
fn keyable(cut_count: usize, sweep: &Sweep) -> bool {
    usize::from(sweep.elevation_number())
        .checked_sub(1)
        .is_some_and(|i| i < cut_count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sampler::{LadderChoice, resolve_ladder};
    use crate::types::{MomentSlot, RadarProduct};
    use nexrad_model::data::{
        ChannelConfiguration, ElevationCut, MomentData, PulseWidth, Radial, RadialStatus,
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
        let current = resolve(Some(&base), Some(&overlay)).expect("both volumes exist");

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
        let current = resolve(Some(&base), Some(&overlay)).expect("both volumes exist");

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
        let current2 = resolve(Some(&base), Some(&overlay2)).expect("both volumes exist");
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
        let current = resolve(Some(&base), Some(&overlay)).expect("both volumes exist");
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
        let current = resolve(Some(&base), Some(&overlay)).expect("both volumes exist");
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
        let current = resolve(Some(&base), Some(&overlay)).expect("the base exists");
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

        let base_only = resolve(Some(&base), None).expect("base alone resolves");
        assert_eq!(base_only.base_sweeps(), 8);
        assert_eq!(base_only.overlay_sweeps(), 0);

        let overlay_only = resolve(None, Some(&overlay)).expect("overlay alone resolves");
        assert_eq!(overlay_only.base_sweeps(), 0);
        assert_eq!(overlay_only.overlay_sweeps(), 2);

        assert!(resolve(None, None).is_none());
    }

    #[test]
    fn the_newest_data_time_is_the_overlay_seal_not_the_base() {
        let base = base_volume(0);
        let overlay = overlay_volume(0, 2);
        let current = resolve(Some(&base), Some(&overlay)).expect("both volumes exist");
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
        let before = resolve(Some(&base), Some(&one_sealed)).expect("resolves");
        let after = resolve(Some(&base), Some(&two_sealed)).expect("resolves");

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
        let before = resolve(Some(&base), Some(&two_sealed)).expect("resolves");
        let after = resolve(Some(&base), Some(&three_sealed)).expect("resolves");
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
        let a = resolve(Some(&base), Some(&overlay_a)).expect("resolves");
        let b = resolve(Some(&base), Some(&overlay_b)).expect("resolves");
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
        let a = resolve(None, Some(&flown)).expect("resolves");
        let b = resolve(None, Some(&taller)).expect("resolves");
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
        let current = resolve(Some(&base), Some(&overlay)).expect("both volumes exist");

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
        let scan = crate::scan::get_scan(site, now).await.expect("a volume");

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
        let current = resolve(Some(&scan), Some(&scan)).expect("resolves");
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
        let current = resolve(None, Some(&no_pattern));
        assert!(
            current.is_none(),
            "an overlay with no pattern and no base resolves to nothing"
        );

        let base = base_volume(0);
        let current = resolve(Some(&base), None).expect("resolves");
        assert!(
            current
                .ladder_fingerprint(RadarProduct::VerticallyIntegratedLiquid)
                .is_none(),
            "a Level III product has no ladder and no key"
        );
    }
}
