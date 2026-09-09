//! **A decoded volume with its gate arrays released and every scalar kept.**
//!
//! # Why this type exists
//!
//! A decoded Level II volume is 33.7–82.7 MiB, and **95.9 % of that is gate
//! buffers** (`crate::scan_size`). Some of the things that read a volume never
//! look at a gate: `crate::sampler::ladder_fingerprint` reads cut angles,
//! elevation numbers, radial counts, the first radial's collection time and
//! whether a moment slot is *present* on it; `crate::current::CurrentVolume::newest_data_time`
//! reads collection times. Both run on the frame thread — the fingerprint per
//! cross-section pane, the stamp once a frame per site — which is why the
//! volume they read has to stay resident and why that residency is most of
//! what the census publishes as `still scans`.
//!
//! A skeleton serves those readers at the structure's cost instead of the
//! arrays'.
//!
//! # Why it is a distinct TYPE and not a flag
//!
//! Because the failure mode of getting this wrong is silent. A gate reader
//! handed a gate-released `Scan` does not error: `raw_values()` returns an
//! empty slice, `values()` yields nothing, and the section cut or the render
//! produces a **blank picture from data that looked present**. Nothing in the
//! decode path treats an empty buffer as invalid, because nothing needed to.
//!
//! So the skeleton is a newtype with a private field, and the only way to a
//! `Scan` from it is [`VolumeSkeleton::as_scan_without_gates`] — named so that
//! a caller reaching for gates through it has to write the words down. Storing
//! one in a field a gate reader reads is then a type error rather than a
//! blank pane.
//!
//! # The silent readers, enumerated
//!
//! Four places consume an empty gate buffer as legitimate data rather than
//! refusing it, which is why the type and not a flag:
//!
//! * `crate::render`'s Level II raster fill iterates the moment and paints
//!   nothing, then returns `Some(..)`: **a fully transparent picture reported
//!   as a successful render.**
//! * `crate::render::plane::sweep_code_plane` does refuse
//!   (`PlaneUnavailable::NothingPainted`), but the caller logs it at `info!`
//!   and falls through to the raster above — its own doc says "`None` is not
//!   a failure, it is the raster".
//! * `crate::velocity::grid` sizes itself from the SCALAR `gate_count`, which
//!   a skeleton preserves, and returns `Some` with an all-`NaN` grid that
//!   NROT, SRV, the wind profile and the dealiaser consume as a real blank
//!   sweep.
//! * `crate::sampler::estimate_fold_limit` returns `None`, silently disarming
//!   a rung's velocity-fold guard.
//!
//! None of these is reachable from a skeleton today, because nothing stores
//! one yet. They are the acceptance criteria for the change that does.
//!
//! # What is preserved, exactly
//!
//! Every scalar of every moment: `gate_count`, `first_gate_range`,
//! `gate_interval`, `data_word_size`, `scale`, `offset` — and the moment's
//! PRESENCE, which is what `resolve_ladder`'s `carries` asks. `gate_count` in
//! particular is **hashed** by `ladder_fingerprint`, so a skeleton that
//! dropped it would silently move a re-cut key; that is why the release is a
//! vendored method on the block rather than a rebuild through the public
//! accessors, which read fixed-point `u16` fields back as `f64` kilometres.
//!
//! Every scalar of every radial and sweep is preserved by construction: the
//! radials are rebuilt field for field and the sweeps keep their elevation
//! numbers and radial order.
//!
//! # What it costs
//!
//! The containers, exactly sized. A decoded volume's radial vectors carry
//! ~42 % spare capacity because the decoder grows them radial by radial; a
//! skeleton is built with `collect` into a vector sized from an
//! `ExactSizeIterator`, so it carries none.

use nexrad_model::data::{Radial, Scan, Sweep};

/// A volume's structure with its gate arrays released.
///
/// Construct with [`VolumeSkeleton::of`]. See the module note for why the
/// inner `Scan` is private.
#[derive(Debug, Clone)]
pub struct VolumeSkeleton(Scan);

impl VolumeSkeleton {
    /// **Release every gate buffer in `volume`, keeping its structure.**
    ///
    /// O(radials × moments) and allocates the structure once. Nothing decodes
    /// and no gate is read — the buffers are dropped, not copied.
    pub fn of(volume: &Scan) -> Self {
        let sweeps = volume
            .sweeps()
            .iter()
            .map(|sweep| {
                let radials = sweep
                    .radials()
                    .iter()
                    .map(strip_radial)
                    .collect::<Vec<Radial>>();
                Sweep::new(sweep.elevation_number(), radials)
            })
            .collect::<Vec<Sweep>>();
        // **The site rides along, and forgetting it is a WRONG READOUT rather
        // than a blank one.** `Scan::new` sets `site: None`, and
        // `crate::types::ScanInfo::from_scan` reads `data.site()` to decide
        // whether a radar's position is `SitePositionSource::Volume` or falls
        // back to the station table. A skeleton built with `Scan::new` would
        // silently demote every volume-stated position to the table row — no
        // error, no log, a different number on the readout. The first version
        // of this function did exactly that.
        Self(match volume.site() {
            Some(site) => Scan::with_site(site.clone(), volume.coverage_pattern().clone(), sweeps),
            None => Scan::new(volume.coverage_pattern().clone(), sweeps),
        })
    }

    /// **The structure, as a `Scan`, with every gate buffer empty.**
    ///
    /// Deliberately verbose. Every reader that takes this is asserting it
    /// consults scalars only; one that reaches a gate through it gets an
    /// empty slice and no error, which is the whole hazard the newtype exists
    /// to make visible at the call site.
    pub fn as_scan_without_gates(&self) -> &Scan {
        &self.0
    }

    /// What this skeleton costs the allocator, by the same walk that prices a
    /// whole volume (`crate::scan_size::scan_bytes`), so the two figures are
    /// comparable without a caveat.
    pub fn bytes(&self) -> usize {
        crate::scan_size::scan_bytes(&self.0)
    }
}

/// One radial rebuilt field for field with its moments' buffers released.
///
/// Written out rather than derived: `Radial::new` takes the seven moment slots
/// positionally, so a slot swapped here would put velocity gates under
/// reflectivity's scalars. The order below is `Radial::new`'s own.
fn strip_radial(radial: &Radial) -> Radial {
    Radial::new(
        radial.collection_timestamp(),
        radial.azimuth_number(),
        radial.azimuth_angle_degrees(),
        radial.azimuth_spacing_degrees(),
        radial.radial_status(),
        radial.elevation_number(),
        radial.elevation_angle_degrees(),
        radial.reflectivity().map(|m| m.without_values()),
        radial.velocity().map(|m| m.without_values()),
        radial.spectrum_width().map(|m| m.without_values()),
        radial
            .differential_reflectivity()
            .map(|m| m.without_values()),
        radial.differential_phase().map(|m| m.without_values()),
        radial.correlation_coefficient().map(|m| m.without_values()),
        radial.clutter_filter_power().map(|m| m.without_values()),
    )
}

#[cfg(test)]
mod tests;
