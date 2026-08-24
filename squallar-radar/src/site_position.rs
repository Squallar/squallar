//! Where a radar says it is, in a form that cannot go non-finite.

use crate::sites::{RadarSite, SiteHeights};

/// Micro-degrees in one degree. Not a geodesy constant — it converts an angle
/// to an integer count of angles, never to a ground distance.
const MICRO_DEGREES_PER_DEGREE: f64 = 1_000_000.0;

/// Metres in one international foot, exactly.
const METRES_PER_FOOT: f64 = 0.3048;

/// How far a volume's own position may sit from the fetched catalogue's before
/// the volume stops being believed, in kilometres.
///
/// What holds this constant is the *gap*, not the maximum: nothing in the
/// archive falls between 0.19 km and 4,180 km, so every threshold in that band
/// accepts and refuses the same volumes.
///
/// Measured off-tree over 170 volumes from 77 sites, both instruments: the
/// largest volume-against-station-record disagreement is 186.7 m (`PAIH`),
/// median 2.5 m for WSR-88D and 0.2 m for TDWR, and the largest real movement
/// is `KTLX`'s 43 m re-survey. So one kilometre is 5.4× the largest
/// disagreement and three orders of magnitude below the nearest constructed
/// corruption `nexrad_decode`'s `position_tests` builds (4,180 km).
///
/// A radar that genuinely *relocates* by more than this — `RKSG` moved from
/// Osan to Camp Humphreys, about 15 km — is refused until the catalogue
/// catches up. The refusal is logged at error level: a silent refusal is the
/// same class of defect as the silent acceptance it replaces.
pub const CATALOGUE_DISAGREEMENT_LIMIT_KM: f64 = 1.0;

/// Where the position on a [`crate::types::ScanInfo`]'s site came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SitePositionSource {
    /// The volume in hand said so, in its own Volume Data Block. Self-
    /// correcting: no network, no table, no cache.
    Volume,
    /// A volume said so in an earlier session, and it was remembered.
    Learned,
    /// [`crate::sites::radars()`], the compiled-in snapshot.
    Table,
    /// Nothing knows where this site is — an identifier the table does not
    /// carry whose data carries no position either. The coordinates on the
    /// site are **not an answer**; see [`crate::sites::UNKNOWN_SITE_NAME`].
    Unknown,
}

/// A radar's position and heights, as its own volume reports them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SitePosition {
    /// Latitude, micro-degrees north.
    pub lat_udeg: i32,
    /// Longitude, micro-degrees east.
    pub lon_udeg: i32,
    /// `site_height` from the Volume Data Block: the ground under the tower,
    /// whole metres MSL. The archive truncates this field, so it asserts a
    /// truth somewhere in `[m, m+1)`.
    pub site_height_m: i32,
    /// `tower_height` from the Volume Data Block: the tower above that ground,
    /// whole metres. Equal to `site_height_m` on a TDWR, which is how the two
    /// instruments are told apart — see [`SitePosition::heights_over`].
    pub tower_height_m: i32,
}

impl SitePosition {
    /// The position a decoded volume states, or `None` if it states none.
    ///
    /// The only float-to-integer boundary in the type, and where three things
    /// are refused rather than encoded: non-finite coordinates, coordinates
    /// off the planet, and exactly (0, 0) — a zero-filled Volume Data Block
    /// rather than a radar in the Gulf of Guinea, which a learned position
    /// would persist and outlive the volume that produced it.
    pub fn from_volume(site: &nexrad_model::meta::Site) -> Option<Self> {
        let lat = f64::from(site.latitude());
        let lon = f64::from(site.longitude());
        if !lat.is_finite() || !lon.is_finite() {
            log::warn!("volume for {} states a non-finite position", site);
            return None;
        }
        if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
            log::warn!("volume for {} states a position off the planet", site);
            return None;
        }
        if lat == 0.0 && lon == 0.0 {
            log::warn!("volume for {} states (0, 0); its block is zeroed", site);
            return None;
        }
        Some(Self {
            // `round` before `as` inside `micro_from_degrees`: the saturating
            // cast would otherwise be the only thing standing between a bad
            // float and the integer.
            lat_udeg: micro_from_degrees(lat),
            lon_udeg: micro_from_degrees(lon),
            site_height_m: i32::from(site.height_meters()),
            tower_height_m: i32::from(site.tower_height_meters()),
        })
    }

    /// Latitude in degrees.
    pub fn lat(&self) -> f64 {
        degrees_from_micro(self.lat_udeg)
    }

    /// Longitude in degrees.
    pub fn lon(&self) -> f64 {
        degrees_from_micro(self.lon_udeg)
    }

    /// This position's heights, keeping `table`'s finer figures wherever the
    /// volume cannot contradict them.
    ///
    /// The volume's height fields are whole metres and
    /// [`crate::sites::radars()`]' are feet, so a figure stands unless it
    /// disagrees with its volume by a metre or more — the resolution of the
    /// volume's own field. Every TDWR volume reports `tower_height`
    /// byte-identical to `site_height` and no WSR-88D volume does, so a volume
    /// whose two heights are equal reports one feedhorn figure with no
    /// separable tower ([`SiteHeights::FeedhornOnly`]); the base is unknown.
    pub fn heights_over(&self, table: Option<SiteHeights>) -> SiteHeights {
        if self.tower_height_m == self.site_height_m {
            return SiteHeights::FeedhornOnly {
                feedhorn_ft: match table {
                    Some(SiteHeights::FeedhornOnly { feedhorn_ft }) => {
                        adjudicate(feedhorn_ft, self.site_height_m)
                    }
                    _ => feet_from_metres(self.site_height_m),
                },
            };
        }
        match table {
            Some(SiteHeights::BaseAndTower { base_ft, tower_ft }) => SiteHeights::BaseAndTower {
                base_ft: adjudicate(base_ft, self.site_height_m),
                tower_ft: adjudicate(tower_ft, self.tower_height_m),
            },
            _ => SiteHeights::BaseAndTower {
                base_ft: feet_from_metres(self.site_height_m),
                tower_ft: feet_from_metres(self.tower_height_m),
            },
        }
    }

    /// The site row `table` would have been, moved onto this position.
    /// `table` keeps only its name: everything else here is measured.
    pub fn applied_to(&self, table: Option<&'static RadarSite>) -> RadarSite {
        self.applied_to_named(
            table.map_or(crate::sites::UNKNOWN_SITE_NAME, |row| row.name),
            table.and_then(|row| row.heights),
        )
    }

    /// The row a radar of this `name` becomes at this position.
    pub fn applied_to_named(
        &self,
        name: &'static str,
        table_heights: Option<SiteHeights>,
    ) -> RadarSite {
        RadarSite {
            name,
            network: crate::sites::RadarNetwork::of_id(name),
            lat: self.lat(),
            lon: self.lon(),
            heights: Some(self.heights_over(table_heights)),
        }
    }
}

/// Degrees from a micro-degree count.
pub(crate) fn degrees_from_micro(udeg: i32) -> f64 {
    f64::from(udeg) / MICRO_DEGREES_PER_DEGREE
}

/// A micro-degree count from degrees, rounded — the inverse of
/// [`degrees_from_micro`], and the same conversion
/// [`SitePosition::from_volume`] does, kept in one place.
pub(crate) fn micro_from_degrees(degrees: f64) -> i32 {
    (degrees * MICRO_DEGREES_PER_DEGREE).round() as i32
}

/// Feet MSL from a whole-metre figure the archive truncated. `pub(crate)` for
/// `SiteFix::applied`, which converts a catalogue's feedhorn metres the same
/// way.
pub(crate) fn feet_from_metres(m: i32) -> i32 {
    (f64::from(m) / METRES_PER_FOOT).round() as i32
}

/// `table_ft` if the volume's metre cannot contradict it, otherwise the
/// volume's. See [`SitePosition::heights_over`] for why the threshold is a
/// metre.
fn adjudicate(table_ft: i32, volume_m: i32) -> i32 {
    let disagreement_m = (f64::from(table_ft) * METRES_PER_FOOT - f64::from(volume_m)).abs();
    if disagreement_m < 1.0 {
        table_ft
    } else {
        feet_from_metres(volume_m)
    }
}

#[cfg(test)]
mod tests;
