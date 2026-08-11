//! Where a radar says it is, in a form that cannot go non-finite.
//!
//! # Why this exists
//!
//! [`crate::sites::RADARS`] is a snapshot of the network on the day the binary
//! shipped. It is correct today — every row was read out of that site's own
//! Level II volume — and it starts rotting the moment it is compiled. A radar
//! that is re-surveyed, relocated, or commissioned after the build is a row
//! this binary can never learn about.
//!
//! Meanwhile every volume the application downloads *states its own position*.
//! `crate::scan::decoded` has always read `latitude_raw`, `longitude_raw`,
//! `site_height_raw` and `tower_height_raw` out of the first Message 31's
//! Volume Data Block and handed them to `nexrad_model::data::Scan::with_site`
//! — and nothing in the workspace ever called `Scan::site()`. The truth was
//! decoded into memory once per volume and thrown away in favour of the table.
//!
//! This type is what carries it instead.
//!
//! # Why integers
//!
//! `serde_json` writes a non-finite float as `null`, and `null` then fails to
//! deserialize on the *next* load — so one bad `f64` in a persisted record
//! destroys the whole record, and the failure surfaces a run later than the
//! bug. That is the same failure class `VolumeAlphaConfig.alpha: Vec<u8>` was
//! moved to integers to close, and it is closed the same way here: an `i32`
//! has no non-finite values, so there is nothing to filter and nothing to
//! remember to filter.
//!
//! The conversion happens once, at [`SitePosition::from_volume`], which is the
//! only place a float ever becomes a `SitePosition`. Everything downstream of
//! it is integers.
//!
//! # Why micro-degrees
//!
//! A micro-degree is ~0.11 m of latitude. The largest position step any site
//! in the archive has ever made is `KTLX`'s 43 m re-survey between 2013 and
//! 2016 — three orders of magnitude coarser — and the source figure is an
//! `f32`, whose spacing at CONUS longitudes is ~0.8 m. So micro-degrees are
//! finer than both the thing being measured and the thing measuring it, and
//! nothing is lost by rounding to them.
//!
//! Rounding *at the source* is also what makes a learned position and the
//! volume it was learned from bit-identical. A pane that renders from a volume
//! this session and from the cache the next session lands on the same pixel,
//! because both paths divide the same integer.

use crate::sites::{RadarSite, SiteHeights};

/// Micro-degrees in one degree.
///
/// Not a geodesy constant — it converts an angle to an integer count of
/// angles, and never to a ground distance. See
/// [`crate::types::KM_PER_DEGREE_LAT`] for the one thing in this workspace
/// that is allowed to do the latter.
const MICRO_DEGREES_PER_DEGREE: f64 = 1_000_000.0;

/// Metres in one international foot, exactly.
const METRES_PER_FOOT: f64 = 0.3048;

/// Where the position on a [`crate::types::ScanInfo`]'s site came from.
///
/// The order of the variants is the precedence order, and
/// `the_precedence_is_volume_then_learned_then_table` pins it as a table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SitePositionSource {
    /// The volume in hand said so, in its own Volume Data Block. Self-
    /// correcting: no network, no table, no cache.
    Volume,
    /// A volume said so in an earlier session, and it was remembered.
    Learned,
    /// [`crate::sites::RADARS`], the compiled-in snapshot.
    Table,
    /// Nothing knows where this site is.
    ///
    /// Reachable for an identifier the table does not carry whose data
    /// carries no position either — a pre-2010 volume or a chunk feed for a
    /// radar commissioned after this binary was built. The coordinates on the
    /// site are **not an answer**; see [`crate::sites::UNKNOWN_SITE_NAME`].
    Unknown,
}

/// A radar's position and heights, as its own volume reports them.
///
/// Constructed only by [`SitePosition::from_volume`] and by deserialization,
/// and both go through the same range check — so a value of this type is
/// always a plausible point on Earth. See the module note for why the fields
/// are integers.
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
    /// This is the only float-to-integer boundary in the type, and it is where
    /// three things are refused rather than encoded:
    ///
    /// * **non-finite coordinates**, which is what makes the persisted form
    ///   safe by construction rather than by a filter somebody has to
    ///   remember;
    /// * **coordinates off the planet**, which would also overflow the `i32`
    ///   at ±2147 degrees;
    /// * **exactly (0, 0)**, which is a zero-filled Volume Data Block rather
    ///   than a radar in the Gulf of Guinea. No WSR-88D or TDWR is within
    ///   1500 km of Null Island, and treating a zeroed block as a measurement
    ///   is the same defect this whole change exists to remove — one that is
    ///   worse here, because a learned (0, 0) would then be *persisted* and
    ///   outlive the volume that produced it.
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
            // `round` before `as`: the saturating cast would otherwise be the
            // only thing standing between a bad float and the integer, and the
            // checks above already mean it can never be reached.
            lat_udeg: (lat * MICRO_DEGREES_PER_DEGREE).round() as i32,
            lon_udeg: (lon * MICRO_DEGREES_PER_DEGREE).round() as i32,
            site_height_m: i32::from(site.height_meters()),
            tower_height_m: i32::from(site.tower_height_meters()),
        })
    }

    /// Latitude in degrees.
    pub fn lat(&self) -> f64 {
        f64::from(self.lat_udeg) / MICRO_DEGREES_PER_DEGREE
    }

    /// Longitude in degrees.
    pub fn lon(&self) -> f64 {
        f64::from(self.lon_udeg) / MICRO_DEGREES_PER_DEGREE
    }

    /// This position's heights, keeping `table`'s finer figures wherever the
    /// volume cannot contradict them.
    ///
    /// # Why the table is not simply overwritten
    ///
    /// The volume's height fields are **whole metres** and
    /// [`crate::sites::RADARS`]' are feet, and the feet are the finer
    /// expression of the same measurement: `TICH` reads 1351 ft where its
    /// volume truncates to 411 m, and 411.78 m is what the published station
    /// record holds. Overwriting a row with `feet(411)` would move it 3 ft for
    /// no reason.
    ///
    /// So this applies the rule the table itself was corrected under: a figure
    /// stands unless it disagrees with its volume **by a metre or more**, the
    /// resolution of the volume's own field, below which the volume cannot
    /// adjudicate. Over all 367 height components in the shipped table exactly
    /// one crosses that line — `RKSG`'s base, by 423 m, because the radar
    /// moved from Osan to Camp Humphreys. That is precisely the case worth
    /// catching, and it is the only one.
    ///
    /// # The shape comes from the volume
    ///
    /// Every TDWR volume reports `tower_height` byte-identical to
    /// `site_height`, and no WSR-88D volume does — an exact correspondence
    /// across all 205 volumes read when the table was built. So a volume whose
    /// two heights are equal is reporting one feedhorn figure with no
    /// separable tower, and says so with [`SiteHeights::FeedhornOnly`]; the
    /// base is *unknown*, not equal to it.
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
    ///
    /// `table` keeps only its name: everything else here is measured, and the
    /// name is the one thing a volume cannot supply as a `&'static str`.
    pub fn applied_to(&self, table: Option<&'static RadarSite>) -> RadarSite {
        RadarSite {
            name: table.map_or(crate::sites::UNKNOWN_SITE_NAME, |row| row.name),
            lat: self.lat(),
            lon: self.lon(),
            heights: Some(self.heights_over(table.and_then(|row| row.heights))),
        }
    }
}

/// Feet MSL from a whole-metre figure the archive truncated.
fn feet_from_metres(m: i32) -> i32 {
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
