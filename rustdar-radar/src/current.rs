//! The current merged volume: the latest **complete** volume a site produced,
//! overlaid by every sealed sweep of the volume now being flown.
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

use nexrad_model::data::{Sweep, VolumeCoveragePattern};

use crate::nyquist::{DeclaredNyquist, Volume};
use crate::types::RadarProduct;

/// A site's current volume, resolved as borrows: the pattern that keys it and
/// the sweeps that fill it, base first, overlay after.
pub struct CurrentVolume<'a> {
    pattern: &'a VolumeCoveragePattern,
    sweeps: Vec<&'a Sweep>,
    /// How many of [`Self::sweeps`] came from the base. The overlay's are the
    base_sweeps: usize,
    /// What each served cut declared its Nyquist velocity to be, merged from
    /// the two source volumes by [`merge_declared`].
    declared_nyquist: DeclaredNyquist,
}

impl<'a> CurrentVolume<'a> {
    /// The pattern the merged sweeps are keyed by.
    pub fn pattern(&self) -> &'a VolumeCoveragePattern {
        self.pattern
    }

    /// The merged sweep list: admitted base sweeps in base order, then every
    pub fn sweeps(&self) -> &[&'a Sweep] {
        &self.sweeps
    }

    /// How many sweeps the base contributed. Zero for a volume that is all
    pub fn base_sweeps(&self) -> usize {
        self.base_sweeps
    }

    /// How many sweeps the current flight contributed.
    pub fn overlay_sweeps(&self) -> usize {
        self.sweeps.len() - self.base_sweeps
    }

    /// Each served cut's declared Nyquist velocity — the number
    pub fn declared_nyquist(&self) -> &DeclaredNyquist {
        &self.declared_nyquist
    }

    /// The collection time of the newest radial in the merged volume — the
    /// honest "data through" stamp for a caption, and a monotone identity for
    /// a rebuild key: every sealed sweep advances it.
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
    pub fn ladder_fingerprint(&self, product: RadarProduct) -> Option<u64> {
        crate::sampler::ladder_fingerprint(self.pattern, &self.sweeps, product)
    }
}

/// Resolve a site's current volume from what the app holds.
///
/// `base` is the latest **complete** volume (an archive decode or a closed
pub fn resolve<'a>(
    base: Option<Volume<'a>>,
    overlay: Option<Volume<'a>>,
) -> Option<CurrentVolume<'a>> {
    let overlay = overlay.filter(|v| !v.scan().coverage_pattern().elevation_cuts().is_empty());

    match (base, overlay) {
        (Some(base_volume), Some(overlay_volume)) => {
            let (base, overlay) = (base_volume.scan(), overlay_volume.scan());
            let base_cuts = base.coverage_pattern().elevation_cuts();
            let overlay_cuts = overlay.coverage_pattern().elevation_cuts();
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
