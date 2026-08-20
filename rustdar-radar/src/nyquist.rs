//! The Nyquist velocity each sweep **declares**, and the pairing that carries
//! it to the one consumer that needs it.
//!
//! Doppler velocity wraps at the Nyquist velocity, and
//! [`crate::sampler::VolumeSampler`] refuses to interpolate a pair of readings
//! that straddle that wrap. The number is a property of the sweep's PRF: it
//! differs from cut to cut inside one volume — KFFC's 2026-08-12 02:05 volume
//! declares 25.65 m/s on each of its low Doppler cuts and climbs to 62.94 on
//! cut 12 — so the guard needs it per sweep, not per volume.
//! **The archive states it.** Message 31's Radial Data Block carries
//! `nyquist_velocity`, in hundredths of a metre per second, on every radial;
//! `nexrad-decode` decodes it. What loses it is the model boundary:
//! `nexrad_model::data::Radial` has no field for it, so `volume::File::scan()`
//! drops it on the floor, and nothing downstream of a `Scan` can get it back.
//! [`crate::scan`] therefore walks the archive's records itself and reads the
//! number where it is still in hand, on the same pass that builds the `Scan`.

use std::collections::{BTreeMap, BTreeSet};

use nexrad_model::data::Scan;

/// Elevation number → declared Nyquist velocity, metres per second.
///
/// Keyed by the RDA's own `elevation_number` — the 1-based index of the cut in
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeclaredNyquist {
    by_elevation: BTreeMap<u8, f64>,
    /// Elevation numbers a second, **different** declaration arrived for. See
    /// [`Self::declare`] for what that means and why it is only ever recorded.
    contradicted: BTreeSet<u8>,
}

impl DeclaredNyquist {
    pub const fn empty() -> Self {
        Self {
            by_elevation: BTreeMap::new(),
            contradicted: BTreeSet::new(),
        }
    }

    /// Record `elevation_number`'s declared Nyquist velocity in m/s, **first
    /// writer wins**.
    pub fn declare(&mut self, elevation_number: u8, metres_per_second: f64) {
        if !Self::is_a_fold_limit(metres_per_second) {
            return;
        }
        let Some(&held) = self.by_elevation.get(&elevation_number) else {
            self.by_elevation
                .insert(elevation_number, metres_per_second);
            return;
        };
        if held == metres_per_second || !self.contradicted.insert(elevation_number) {
            return;
        }
        log::warn!(
            "elevation number {elevation_number} declared {held} m/s and then \
             {metres_per_second} m/s: two cuts share one key, so every reader of this volume \
             folds the later one around the earlier one's PRF"
        );
    }

    /// Whether a number is a speed a sweep could actually fold at: finite, and
    /// above zero.
    fn is_a_fold_limit(metres_per_second: f64) -> bool {
        metres_per_second.is_finite() && metres_per_second > 0.0
    }

    /// The cuts a second, different declaration arrived for — empty on every
    /// volume this archive has been observed to produce. See [`Self::declare`].
    pub fn contradicted(&self) -> impl Iterator<Item = u8> + '_ {
        self.contradicted.iter().copied()
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
    pub fn overlay(&mut self, newer: &Self) {
        for (elevation_number, ms) in newer.iter() {
            self.set(elevation_number, ms);
        }
        self.contradicted.extend(newer.contradicted.iter().copied());
    }

    /// Encode for a message port.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + self.by_elevation.len() * 9 + self.contradicted.len());
        out.extend_from_slice(&(self.by_elevation.len() as u32).to_le_bytes());
        for (elevation_number, metres_per_second) in self.iter() {
            out.push(elevation_number);
            out.extend_from_slice(&metres_per_second.to_le_bytes());
        }
        out.extend_from_slice(&(self.contradicted.len() as u32).to_le_bytes());
        out.extend(self.contradicted.iter().copied());
        out
    }

    /// The inverse of [`to_bytes`](Self::to_bytes), reading from the shared
    /// cursor so this table can sit inside a larger payload without either side
    /// having to say where it ended.
    pub(crate) fn read(r: &mut crate::wire::Reader) -> Option<Self> {
        let pairs = r.u32()?;
        let declared = r.bounded(pairs, 9)?;
        let mut table = Self::empty();
        for _ in 0..declared {
            let elevation_number = r.u8()?;
            table.by_elevation.insert(elevation_number, r.f64()?);
        }
        let flagged = r.u32()?;
        let contradicted = r.bounded(flagged, 1)?;
        for _ in 0..contradicted {
            table.contradicted.insert(r.u8()?);
        }
        Some(table)
    }

    /// [`Self::declare`]'s last-wins twin: replace whatever this table held
    /// for `elevation_number`.
    pub(crate) fn set(&mut self, elevation_number: u8, metres_per_second: f64) {
        if Self::is_a_fold_limit(metres_per_second) {
            self.by_elevation
                .insert(elevation_number, metres_per_second);
        }
    }

    /// Record what one decoded Message 31 radial declares, if it declares
    /// anything.
    pub(crate) fn declare_from_message(
        &mut self,
        radar: &nexrad_decode::messages::digital_radar_data::Message<'_>,
    ) {
        let Some(block) = radar.radial_data_block() else {
            return;
        };
        self.declare(
            radar.header().elevation_number(),
            f64::from(block.nyquist_velocity_raw()) * 0.01,
        );
    }

    /// Read every cut's declared Nyquist velocity out of a raw Level II
    /// archive file, on a walk of its own.
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
