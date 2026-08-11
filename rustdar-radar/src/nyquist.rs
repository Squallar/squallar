//! The Nyquist velocity each sweep **declares**, and the pairing that carries
//! it to the one consumer that needs it.
//!
//! # Why this module exists at all
//!
//! Doppler velocity wraps at the Nyquist velocity, and
//! [`crate::sampler::VolumeSampler`] refuses to interpolate a pair of readings
//! that straddle that wrap. The number is a property of the sweep's PRF: it
//! differs from cut to cut inside one volume — measured 22.5–31 m/s on the low
//! cuts against up to 35.5 on the high cuts of the same volume — so the guard
//! needs it per sweep, not per volume.
//!
//! **The archive states it.** Message 31's Radial Data Block carries
//! `nyquist_velocity`, in hundredths of a metre per second, on every radial;
//! `nexrad-decode` decodes it. What loses it is the model boundary:
//! `nexrad_model::data::Radial` has no field for it, so `volume::File::scan()`
//! drops it on the floor, and nothing downstream of a `Scan` can get it back.
//! [`crate::scan`] therefore walks the archive's records itself and reads the
//! number where it is still in hand, on the same pass that builds the `Scan`.
//!
//! Before this module the sampler *estimated* the limit instead, off the
//! largest speed a sweep observed (`estimate_fold_limit`).
//! That estimate is exact for a sweep that folded and an **under**estimate for
//! one that did not, and the sampler uses the number as a classification
//! boundary, so an underestimate widens the fold hypothesis and manufactures
//! false positives. The declared number has neither failure mode. The estimate
//! stays as the fallback, because it is still the only answer available where
//! the declaration is not:
//!
//! * a volume decoded entirely from **Message 1** (`digital_radar_data_legacy`)
//!   — the legacy message has no Nyquist field of any kind, so there is nothing
//!   to read, and this is an absence rather than an error;
//! * a `Scan` that reached the sampler by some route that never carried a
//!   table — every test fixture, and any future caller that holds only model
//!   types.
//!
//! # What is *not* in here
//!
//! No tolerance, no reconciliation between the declared and the estimated
//! number, and no warning when they disagree. The declared value simply wins
//! where it exists. Comparing the two would be a measurement, and this is the
//! plumbing.

use std::collections::BTreeMap;

use nexrad_model::data::Scan;

/// Elevation number → declared Nyquist velocity, metres per second.
///
/// Keyed by the RDA's own `elevation_number` — the 1-based index of the cut in
/// the VCP — because that is the key the sampler already resolves a rung by,
/// and because it survives every hop this value takes: the chunk feed, the
/// merged current volume and the worker's wire payload all carry it, while a
/// sweep's position in a `Vec` does not.
///
/// Empty is the honest "no volume said" and the only failure mode: every
/// reader falls back to `estimate_fold_limit` for a cut this
/// has no entry for, so a partial table degrades cut by cut rather than
/// wholesale.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeclaredNyquist {
    by_elevation: BTreeMap<u8, f64>,
}

impl DeclaredNyquist {
    /// A table that declares nothing. `const` so it can back the `static` that
    /// [`Volume`]'s `From<&Scan>` hands out.
    pub const fn empty() -> Self {
        Self {
            by_elevation: BTreeMap::new(),
        }
    }

    /// Record `elevation_number`'s declared Nyquist velocity in m/s, **first
    /// writer wins**.
    ///
    /// First-wins rather than last-wins because within one sweep the number is
    /// constant by construction — every radial of a cut is collected at the
    /// same PRF — so the first radial to arrive is as good a statement as the
    /// last, and first-wins makes the table independent of how many radials a
    /// caller happens to walk. A non-finite value is refused rather than
    /// stored: it would reach the guard as a comparison that is false in both
    /// directions, which is a silently disabled guard rather than an absent
    /// one.
    pub fn declare(&mut self, elevation_number: u8, metres_per_second: f64) {
        if metres_per_second.is_finite() {
            self.by_elevation
                .entry(elevation_number)
                .or_insert(metres_per_second);
        }
    }

    /// What cut `elevation_number` declared, m/s, or `None` when this table
    /// does not name it.
    pub fn get(&self, elevation_number: u8) -> Option<f64> {
        self.by_elevation.get(&elevation_number).copied()
    }

    /// Nothing was declared anywhere in this volume.
    pub fn is_empty(&self) -> bool {
        self.by_elevation.is_empty()
    }

    /// How many cuts declared a value.
    pub fn len(&self) -> usize {
        self.by_elevation.len()
    }

    /// Every `(elevation_number, m/s)` pair, ascending by elevation number.
    pub fn iter(&self) -> impl Iterator<Item = (u8, f64)> + '_ {
        self.by_elevation.iter().map(|(k, v)| (*k, *v))
    }

    /// Overlay `newer` onto this table: every cut `newer` names takes its
    /// value, and cuts it does not name keep theirs.
    ///
    /// The merge [`crate::current::resolve`] needs, and in its direction. A
    /// merged volume serves each cut from the *newest* sweep that sealed it —
    /// the in-flight overlay's where it has one, the complete base's
    /// otherwise — so the declared number has to follow the same precedence or
    /// a rung would be guarded by the PRF of the sweep it did not take. Two
    /// volumes flying the same VCP normally declare the same numbers, so this
    /// is usually a no-op; it stops being one across a VCP change or an
    /// adaptive-PRF reselect, which is exactly when it matters.
    pub fn overlay(&mut self, newer: &Self) {
        for (elevation_number, ms) in newer.iter() {
            self.set(elevation_number, ms);
        }
    }

    /// [`Self::declare`]'s last-wins twin: replace whatever this table held
    /// for `elevation_number`.
    ///
    /// `pub(crate)` because the only caller that legitimately overwrites is
    /// [`crate::current::resolve`]'s merge, where a later sweep is by
    /// construction the newer statement of its cut. Everywhere else the
    /// first-wins rule is what keeps a table from depending on how far a walk
    /// happened to get, so this is not part of the public surface.
    pub(crate) fn set(&mut self, elevation_number: u8, metres_per_second: f64) {
        if metres_per_second.is_finite() {
            self.by_elevation
                .insert(elevation_number, metres_per_second);
        }
    }

    /// Record what one decoded Message 31 radial declares, if it declares
    /// anything.
    ///
    /// **The one place this crate reads the field, and the one place it states
    /// the unit.** Three walks over Level II bytes reach a Message 31 —
    /// [`crate::scan`]'s archive decode, [`crate::chunks`]'s real-time chunk
    /// decode, and [`Self::from_archive`]. Two of them spelled the read out for
    /// themselves before this; the archive decode is the walk this change adds,
    /// and it would have been a third copy. "Read `nyquist_velocity_raw`,
    /// multiply by 0.01, first writer wins" written out three times is three
    /// chances for one of them to drift, and the drift would be silent: the
    /// guard would simply be a little wrong on whichever path diverged. They
    /// all call this instead.
    ///
    /// A radial with no Radial Data Block leaves its cut unnamed rather than
    /// declaring a zero — an absence the guard estimates for, not a fold limit
    /// of nothing.
    pub(crate) fn declare_from_message(
        &mut self,
        radar: &nexrad_decode::messages::digital_radar_data::Message<'_>,
    ) {
        let Some(block) = radar.radial_data_block() else {
            return;
        };
        // The raw word is hundredths of a metre per second. Taken raw rather
        // than through `nyquist_velocity()` so this crate's one statement of
        // the unit is the `* 0.01` here, in a module whose whole subject is the
        // number.
        self.declare(
            radar.header().elevation_number(),
            f64::from(block.nyquist_velocity_raw()) * 0.01,
        );
    }

    /// Read every cut's declared Nyquist velocity out of a raw Level II
    /// archive file, on a walk of its own.
    ///
    /// **Not what the archive path uses.** [`crate::scan`] folds this read into
    /// the same walk that builds the `Scan`, because a separate pass here costs
    /// a second bzip2 decompress and a second Message 31 parse of the whole
    /// volume — measured at 98% of `volume::File::scan()`'s own cost, so
    /// running both very nearly doubled every archive decode.
    ///
    /// It stays because its *traversal* is independent: the live test in
    /// [`crate::scan`] pins the folded table against this one, and a table built
    /// by a walk that does nothing else is what makes that a real check on the
    /// single-pass restructure rather than a tautology. Note the limit of that
    /// check — the reading itself is shared through
    /// [`Self::declare_from_message`], so a wrong field or a wrong unit would be
    /// wrong identically on both sides and this would still agree. Use it for
    /// the traversal check, and for a caller who wants the numbers without
    /// paying for a `Scan`.
    ///
    /// Every failure is an absence, never an error: an unreadable record, a
    /// record that will not decompress, a Message 1 volume (no Nyquist field
    /// exists in the legacy message) and a Message 31 radial with no Radial
    /// Data Block all leave their cut unnamed, and the guard estimates for it.
    /// Returning a `Result` here would make a volume that renders perfectly
    /// well fail on a field only one product's interpolation reads.
    pub fn from_archive(file: &nexrad_data::volume::File) -> Self {
        use nexrad_decode::messages::MessageContents;
        let mut out = Self::empty();
        let Ok(records) = file.records() else {
            return out;
        };
        for record in records {
            let record = if record.compressed() {
                match record.decompress() {
                    Ok(r) => r,
                    Err(_) => continue,
                }
            } else {
                record
            };
            let Ok(messages) = record.messages() else {
                continue;
            };
            for message in messages {
                if let MessageContents::DigitalRadarData(radar) = message.contents() {
                    out.declare_from_message(radar);
                }
            }
        }
        out
    }
}

impl FromIterator<(u8, f64)> for DeclaredNyquist {
    fn from_iter<I: IntoIterator<Item = (u8, f64)>>(iter: I) -> Self {
        let mut out = Self::empty();
        for (elevation_number, ms) in iter {
            out.declare(elevation_number, ms);
        }
        out
    }
}

/// The table [`Volume::from`] hands a caller who passed a bare `Scan`: a
/// volume nothing declared for, which every reader treats as "estimate".
static NOTHING_DECLARED: DeclaredNyquist = DeclaredNyquist::empty();

/// A borrowed volume: a `Scan`, and the per-sweep numbers the model type drops.
///
/// # Why a pair rather than a parameter
///
/// [`crate::sampler::VolumeSampler::new`], [`crate::xsect::render_section`] and
/// [`crate::voxel::build_voxels_with_motion`] all take "the volume", and all
/// three now need one thing about it that a `Scan` cannot hold. Adding a
/// parameter to each would mean every caller that *has* no table — every
/// fixture, every test, every path where the volume never came from an archive
/// — writing an empty one out loud at the call site, and it would put the
/// declared table's absence in three signatures instead of one type.
///
/// So they take `impl Into<Volume>` instead. `&Scan` converts, yielding a
/// volume that declares nothing; a caller that *does* hold a table passes
/// [`Volume::new`]. The conversion is what keeps "no declared table" from
/// being a special case anybody has to spell.
#[derive(Clone, Copy)]
pub struct Volume<'a> {
    scan: &'a Scan,
    declared_nyquist: &'a DeclaredNyquist,
}

impl<'a> Volume<'a> {
    /// Pair a scan with the table its archive declared.
    pub fn new(scan: &'a Scan, declared_nyquist: &'a DeclaredNyquist) -> Self {
        Self {
            scan,
            declared_nyquist,
        }
    }

    /// The volume's sweeps and coverage pattern.
    pub fn scan(&self) -> &'a Scan {
        self.scan
    }

    /// What each cut declared, possibly nothing.
    pub fn declared_nyquist(&self) -> &'a DeclaredNyquist {
        self.declared_nyquist
    }
}

impl<'a> From<&'a Scan> for Volume<'a> {
    fn from(scan: &'a Scan) -> Self {
        Self {
            scan,
            declared_nyquist: &NOTHING_DECLARED,
        }
    }
}

#[cfg(test)]
mod tests;
