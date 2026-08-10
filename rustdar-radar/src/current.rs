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

use nexrad_model::data::{Sweep, VolumeCoveragePattern};

use crate::nyquist::{DeclaredNyquist, Volume};
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
    /// What each served cut declared its Nyquist velocity to be, merged from
    /// the two source volumes by [`merge_declared`].
    ///
    /// Owned rather than borrowed, unlike everything else here: it is a
    /// handful of `f64`s, and there is no single existing table to point at —
    /// the merged volume's is the *composition* of two.
    declared_nyquist: DeclaredNyquist,
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

    /// Each served cut's declared Nyquist velocity — the number
    /// [`crate::sampler::VolumeSampler`]'s velocity fold guard prefers to its
    /// own estimate, and the one a `Scan` cannot carry.
    ///
    /// Pass it to [`crate::render_input::RenderInput::with_declared_nyquist`]
    /// alongside the payload extracted from [`Self::pattern`] and
    /// [`Self::sweeps`], so the worker guards on the same limits this thread
    /// would. Empty when neither source volume declared anything, which every
    /// reader treats as "estimate".
    pub fn declared_nyquist(&self) -> &DeclaredNyquist {
        &self.declared_nyquist
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
pub fn resolve<'a>(
    base: Option<Volume<'a>>,
    overlay: Option<Volume<'a>>,
) -> Option<CurrentVolume<'a>> {
    // An overlay whose pattern has no cuts cannot key its own sweeps — the
    // mid-flight-join state. It contributes nothing rather than borrowing the
    // base's table for a flight whose plan is unknown.
    let overlay = overlay.filter(|v| !v.scan().coverage_pattern().elevation_cuts().is_empty());

    match (base, overlay) {
        (Some(base_volume), Some(overlay_volume)) => {
            let (base, overlay) = (base_volume.scan(), overlay_volume.scan());
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
            let declared_nyquist = merge_declared(
                &sweeps,
                base_sweeps,
                base_volume.declared_nyquist(),
                overlay_volume.declared_nyquist(),
            );
            Some(CurrentVolume {
                pattern: overlay.coverage_pattern(),
                sweeps,
                base_sweeps,
                declared_nyquist,
            })
        }
        (Some(base), None) => {
            let sweeps: Vec<&Sweep> = base.scan().sweeps().iter().collect();
            let base_sweeps = sweeps.len();
            let declared_nyquist = merge_declared(
                &sweeps,
                base_sweeps,
                base.declared_nyquist(),
                &DeclaredNyquist::empty(),
            );
            Some(CurrentVolume {
                pattern: base.scan().coverage_pattern(),
                sweeps,
                base_sweeps,
                declared_nyquist,
            })
        }
        // No base yet: the overlay stands alone, exactly as the growing live
        // volume always has. Its ladder is short and the captions say so.
        (None, Some(overlay)) => {
            let sweeps: Vec<&Sweep> = overlay.scan().sweeps().iter().collect();
            let declared_nyquist = merge_declared(
                &sweeps,
                0,
                &DeclaredNyquist::empty(),
                overlay.declared_nyquist(),
            );
            Some(CurrentVolume {
                pattern: overlay.scan().coverage_pattern(),
                sweeps,
                base_sweeps: 0,
                declared_nyquist,
            })
        }
        (None, None) => None,
    }
}

/// The merged volume's declared Nyquist table, built from the **sweeps it
/// actually serves** rather than by overlaying the two volumes' whole tables.
///
/// The distinction is the point. Both source tables can name cuts their
/// volume's sweeps do not appear on — the live assembler declares a cut from
/// its first radial, chunks before it seals — and overlaying them wholesale
/// would let the in-flight volume's number key a rung the *base's* sweep is
/// serving. Where the two volumes fly the same PRF that is a no-op; where an
/// adaptive reselect or a VCP change moved it, it is the guard reading one
/// sweep's fold limit off another sweep's waveform. So each sweep contributes
/// its own volume's declaration and nothing else does.
///
/// `sweeps[..base_sweeps]` are the admitted base sweeps and the rest are the
/// overlay's, which is the order [`resolve`] builds and [`CurrentVolume`]
/// documents.
fn merge_declared(
    sweeps: &[&Sweep],
    base_sweeps: usize,
    base: &DeclaredNyquist,
    overlay: &DeclaredNyquist,
) -> DeclaredNyquist {
    let mut out = DeclaredNyquist::empty();
    for (index, sweep) in sweeps.iter().enumerate() {
        let source = if index < base_sweeps { base } else { overlay };
        if let Some(ms) = source.get(sweep.elevation_number()) {
            // Later sweeps supersede earlier ones on the same cut — the
            // newest-wins rule the whole merge is ordered by — so this is
            // `set` rather than the first-wins `declare`.
            out.set(sweep.elevation_number(), ms);
        }
    }
    out
}

/// Whether `sweep`'s elevation number indexes a table of `cut_count` cuts.
fn keyable(cut_count: usize, sweep: &Sweep) -> bool {
    usize::from(sweep.elevation_number())
        .checked_sub(1)
        .is_some_and(|i| i < cut_count)
}

#[cfg(test)]
mod tests;
