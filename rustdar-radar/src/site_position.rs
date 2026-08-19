//! Where a radar says it is, in a form that cannot go non-finite.
//!
//! # Why this exists
//!
//! [`crate::sites::radars()`] is a snapshot of the network on the day the binary
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
/// [`rustdar_geo::KM_PER_DEGREE_LAT`] for the one thing in this workspace
/// that is allowed to do the latter.
const MICRO_DEGREES_PER_DEGREE: f64 = 1_000_000.0;

/// Metres in one international foot, exactly.
const METRES_PER_FOOT: f64 = 0.3048;

/// How far a volume's own position may sit from the fetched catalogue's before
/// the volume stops being believed, in kilometres.
///
/// # Measured, and then placed in the gap
///
/// **Where these figures come from, and what re-running them means.** Every
/// count and metre in this section was read by `site_elev_probe`, which lives
/// on branch `campaign/site-position-probe` together with the two scripts that
/// feed it (`harness/fetch_site_corpus.sh` fetches one 4 MB volume prefix per
/// site per epoch; `harness/sweep_site_epochs.sh` sweeps epochs into one TSV).
/// **None of that is in this tree, and the branch kept the apparatus and not
/// the readings** — so a check-out and a re-run produces a *new* measurement
/// against today's archive, not a reproduction of the table below. Read the
/// numbers as a dated observation of the archive, not as a property this build
/// enforces. What this tree does enforce is in `site_position/tests.rs`, and
/// it is the constant's behaviour, never the survey.
///
/// The argument survives that caveat better than the numbers do, and it is
/// worth separating the two: what actually holds this constant is the *gap*,
/// not the maximum. Nothing in the archive falls between 0.19 km and 4,180 km,
/// so every threshold in that band accepts and refuses the same volumes.
///
/// A volume and the published station record are two descriptions of one
/// antenna, so their disagreement is a quantity that can be read off the
/// archive rather than guessed at. Over **170 volumes from 77 sites** — the
/// whole longitude spread of the network, Alaska and Hawaii and Puerto Rico
/// included, both instruments, and both TDWR producers — the largest
/// disagreement is **186.7 m**, at `PAIH`. The distribution behind it:
///
/// ```text
///                     n      max        median
/// WSR-88D             60    186.7 m      2.5 m
/// TDWR               110    111.4 m      0.2 m
///   in degrees        82    111.4 m      0.2 m
///   in thousandths    28      0.0 m      0.0 m
/// ```
///
/// The TDWR ceiling is not a coincidence and not an error: the catalogue quotes
/// a terminal radar to three decimals, so 111.4 m is one quantum of its own
/// precision. And the 28 pre-correction volumes land **exactly** on the
/// catalogue — 0.0 m, every one — which is the strongest evidence available
/// that reading them as thousandths recovers the real position rather than a
/// plausible one.
///
/// Real *movement* is smaller still. The largest step any site in the archive
/// makes is `KTLX`'s re-survey: its own volumes state 35.333057/−97.277481 in
/// 2013 and 35.333363/−97.277763 in 2020, which is **43 m**.
///
/// So one kilometre is not a round number chosen for its roundness. It is 5.4×
/// the largest disagreement 170 real volumes produce and 23× the largest real
/// re-survey, and three orders of magnitude *below* the smallest thing it has
/// to catch: the nearest of the constructed corruptions in `nexrad_decode`'s
/// `position_tests` — a hypothetical hundredths producer writing
/// `(4180, -8786)` for `TORD` — lands 4,180 km away. Between 0.19 km and
/// 4,180 km the archive contains nothing at all, so every value in that band
/// accepts and refuses exactly the same volumes, and the constant is fitted to
/// nothing. It is stated in kilometres because that is the band it sits in.
///
/// # What it costs
///
/// A radar that genuinely *relocates* by more than this — `RKSG` moved from
/// Osan to Camp Humphreys, about 15 km — is refused until the catalogue catches
/// up, and the catalogue is fetched live rather than compiled in, so it
/// generally already has. The refusal is logged at error level for exactly this
/// case: it must never be silent, because a silent refusal is the same class of
/// defect as the silent acceptance it replaces.
pub const CATALOGUE_DISAGREEMENT_LIMIT_KM: f64 = 1.0;

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
    /// [`crate::sites::radars()`], the compiled-in snapshot.
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
            // `round` before `as` inside `micro_from_degrees`: the saturating
            // cast would otherwise be the only thing standing between a bad
            // float and the integer, and the checks above already mean it can
            // never be reached.
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
    /// # Why the table is not simply overwritten
    ///
    /// The volume's height fields are **whole metres** and
    /// [`crate::sites::radars()`]' are feet, and the feet are the finer
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
    /// across all 205 volumes read when the table was built. **That read is
    /// unreproducible from here**: it was the `campaign/site-position-probe`
    /// sweep, which kept its scripts and not its output, so the branch would
    /// re-measure rather than re-confirm. The dispatch below runs on it every
    /// time, which is the reason to say so out loud. So a volume whose
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
        self.applied_to_named(
            table.map_or(crate::sites::UNKNOWN_SITE_NAME, |row| row.name),
            table.and_then(|row| row.heights),
        )
    }

    /// The row a radar of this `name` becomes at this position.
    ///
    /// The same construction as [`applied_to`](Self::applied_to) with the name
    /// supplied separately, which is what a site the compiled-in seed has
    /// never heard of needs: it has no table row to take a name from, but it
    /// does have an ICAO — the key its position was learned under — and that
    /// is a better name than [`UNKNOWN_SITE_NAME`](crate::sites::UNKNOWN_SITE_NAME).
    /// [`crate::sites::build_table`] is the caller, and it leaks the
    /// identifier so this can take it as `&'static str`.
    ///
    /// `table_heights` are the seed's figures for this row if it has any, so
    /// [`heights_over`](Self::heights_over) can keep the finer of the two, and
    /// `None` for a row the seed never had — which then takes the volume's
    /// heights outright rather than recording none at all.
    pub fn applied_to_named(
        &self,
        name: &'static str,
        table_heights: Option<SiteHeights>,
    ) -> RadarSite {
        RadarSite {
            name,
            lat: self.lat(),
            lon: self.lon(),
            heights: Some(self.heights_over(table_heights)),
        }
    }
}

/// Degrees from a micro-degree count.
///
/// The one place the division happens, so
/// [`SiteFix::Network`](crate::sites::SiteFix::Network) — which carries
/// micro-degrees without carrying a whole [`SitePosition`] — cannot land on a
/// different last decimal place than a learned position does.
pub(crate) fn degrees_from_micro(udeg: i32) -> f64 {
    f64::from(udeg) / MICRO_DEGREES_PER_DEGREE
}

/// A micro-degree count from degrees, rounded.
///
/// The inverse of [`degrees_from_micro`], and the boundary
/// [`crate::catalogue::parse_stations`] crosses: it is the same conversion
/// [`SitePosition::from_volume`] does, kept in one place so a position learned
/// from a volume and the same position read from the station record round to
/// the same integer.
///
/// Callers must range-check first — this saturates rather than wrapping, but a
/// saturated value is still a coordinate nobody measured.
pub(crate) fn micro_from_degrees(degrees: f64) -> i32 {
    (degrees * MICRO_DEGREES_PER_DEGREE).round() as i32
}

/// Feet MSL from a whole-metre figure the archive truncated.
///
/// `pub(crate)` for `SiteFix::applied`, which converts a catalogue's feedhorn
/// metres the same way — the published record quotes whole metres exactly as
/// the Volume Data Block does.
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
