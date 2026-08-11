use crate::site_position::SitePosition;
use crate::types::EARTH_RADIUS_KM;
use std::collections::HashMap;
use std::sync::{LazyLock, RwLock};

/// Which height above mean sea level a caller means.
///
/// A site has two, and they are 30–115 ft apart: the ground the tower stands
/// on, and the feedhorn on top of it. A single number cannot say which, and
/// for 201 of the 207 rows nobody had ever checked which one this table was
/// on — so every consumer that added a site height to a beam height was
/// choosing a datum by inheritance. This type makes the choice a word in the
/// call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Datum {
    /// The ground under the tower — `site_height` in a Volume Data Block.
    ///
    /// This is what the table was on before [`Datum`] existed, and it is
    /// **not** what a beam height should be added to: it is the terrain, not
    /// the instrument. Kept because the table records it, because it is what
    /// a question about the ground would want, and because
    /// `the_two_datums_are_a_tower_apart` needs both to compare.
    SiteBase,
    /// The feedhorn — `site_height + tower_height`, the point [`crate::beam`]
    /// measures every height above, and the figure a published station record
    /// quotes as the radar's elevation.
    Feedhorn,
}

/// What a row knows about its own height, and on which datum.
///
/// Two shapes rather than one, because the archive genuinely reports two
/// shapes and flattening them would put the old ambiguity back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiteHeights {
    /// The two heights a WSR-88D's Volume Data Block reports separately: the
    /// ground under the tower, and the tower above that ground.
    ///
    /// `tower_ft` is the archive's own figure converted, and the archive
    /// truncates to whole metres — the five standard towers read back as 14,
    /// 19, 24, 29 and 34 m against published heights of 48, 65, 81, 97 and
    /// 114 ft — so a feedhorn built from it sits up to 3 ft low. That is the
    /// precision of the source, and it is two orders below the 30–115 ft this
    /// type exists to stop losing.
    BaseAndTower { base_ft: i32, tower_ft: i32 },
    /// One height, on the feedhorn, with no separable tower.
    ///
    /// Every TDWR volume reports `tower_height` byte-identical to
    /// `site_height`, and no WSR-88D volume does — the correspondence is
    /// exact across all 205 volumes read. So a TDWR carries one figure, and
    /// the published station record agrees with it to 3.2 ft while agreeing
    /// with the *feedhorn* everywhere it can be checked on a WSR-88D. Hence
    /// feedhorn, and hence no answer at all for [`Datum::SiteBase`]: the base
    /// is unknown, not equal to this.
    ///
    /// `LPLA` (Lajes) is here too, for a different reason — see [`radars()`].
    FeedhornOnly { feedhorn_ft: i32 },
}

#[derive(Debug, Clone)]
pub struct RadarSite {
    pub name: &'static str,
    pub lat: f64,
    pub lon: f64,
    /// The heights this row records, or `None` if it records none.
    ///
    /// Nothing in the shipped table is `None` —
    /// `every_site_records_an_elevation` keeps it that way, because a missing
    /// elevation used to reach [`crate::eet::radar_height_ft_near`] and come
    /// back as sea level, which is a plausible-looking answer for a coastal
    /// site and a 292 ft error at KLWX.
    pub heights: Option<SiteHeights>,
}

impl RadarSite {
    /// This site's height on `datum`, feet MSL, or `None` if the row does not
    /// record that datum.
    ///
    /// `None` is a real answer, not a formality: a [`SiteHeights::FeedhornOnly`]
    /// row has no base, and returning its feedhorn for [`Datum::SiteBase`]
    /// would be the same silent substitution this type was introduced to
    /// remove.
    pub fn height_ft(&self, datum: Datum) -> Option<i32> {
        match (self.heights?, datum) {
            (SiteHeights::BaseAndTower { base_ft, .. }, Datum::SiteBase) => Some(base_ft),
            (SiteHeights::BaseAndTower { base_ft, tower_ft }, Datum::Feedhorn) => {
                Some(base_ft + tower_ft)
            }
            (SiteHeights::FeedhornOnly { feedhorn_ft }, Datum::Feedhorn) => Some(feedhorn_ft),
            (SiteHeights::FeedhornOnly { .. }, Datum::SiteBase) => None,
        }
    }
}

/// The name a [`RadarSite`] carries when nothing in the build knows the
/// identifier.
///
/// It is a placeholder for a `&'static str` this crate cannot manufacture from
/// a four-byte ICAO read at runtime, **not** a claim that the row means
/// anything. A site carrying this name has no position of its own unless a
/// volume supplied one, and
/// [`ScanInfo::site_source`](crate::types::ScanInfo::site_source) is where a
/// caller finds out which.
pub const UNKNOWN_SITE_NAME: &str = "UNKNOWN";

/// Every radar site, at the position and height its own Level II volume
/// reports.
///
/// # Where the figures come from
///
/// One archive volume per site, read out of the Volume Data Block of its
/// first message 31 by the `site_elev_probe` instrument on the
/// `campaign/site-position-probe` branch, which is also what measured
/// everything claimed below. The bucket is `unidata-nexrad-level2` — the same
/// Level II origin [`crate::sources`] gives the application, current to the
/// day, rather than the Google mirror that runs three weeks behind.
///
/// Position and height are read from the *same* volume for every row, never
/// one without the other. A row carrying one site's coordinates and another
/// epoch's height is worse than a consistently stale row, and `RKSG` is why:
/// the pass that corrected heights alone had to skip it, because giving it
/// Camp Humphreys' height while it kept Osan's position would have turned a
/// self-consistently wrong row into an incoherent one.
///
/// # The epoch policy: the most recent volume, per site
///
/// Reported position and height are step functions, not drifting ones. Across
/// nine epochs from 2026-01 to 2026-08, no site changed its reported height
/// and exactly one changed its reported position; across 2017–2023 `RODN`
/// reports its position bit-identically at every epoch. Where a figure does
/// move it moves once and stays: `KTLX` stepped 43 m and 1 m in a re-survey
/// between 2013 and 2016 and has not moved since.
///
/// So the rule is **the most recent volume that decodes**, and no averaging
/// or voting across epochs — a vote would keep a relocated radar at its old
/// site for as many years as it stood there, which is exactly the `RKSG`
/// failure. `MOVED_ONTO_ITS_VOLUME` records the epoch each correction was
/// read at; 202 of the 206 come from one day, 2026-08-10.
///
/// # What is corrected, and by what rule
///
/// * **Position** — every row takes what its volume reports, to 5 dp. 193
///   rows moved, by a median of 49 m; 50 moved further than one 250 m data
///   cell and 42 further than a kilometre. Afterwards the table agrees with
///   `api.weather.gov/radar/stations` to a median of 1.5 m with no row over
///   a kilometre, against a median of 33 m and 41 rows over a kilometre
///   before.
/// * **Height** — a row keeps the figure it has unless it disagrees with its
///   volume by 1 m or more, the resolution of the volume's own field, below
///   which the volume cannot adjudicate. Over all 367 height components in
///   the table exactly one crosses that line: `RKSG`'s base, by 423 m. The
///   table's foot figures are otherwise the finer expression of the same
///   metre and are kept — `TICH` reads 1351 ft where its volume truncates to
///   411 m, and 411.78 m is what the published station record holds.
///
/// # The TDWR block came from somewhere else
///
/// The 45 TDWR rows were wrong in position at a scale the 161 others were
/// not: median 3.7 km against 33 m, every single row past a data cell, 40 of
/// them past a kilometre and `TICH` 11.7 km out. That is a whole-source
/// defect rather than 45 mistakes.
///
/// It is *not* the airport reference point, which is the obvious suspect and
/// was measured: across the 40 TDWR sites whose airport `api.weather.gov`
/// knows, the old rows sat a median 16.2 km from the airport and 3.2 km from
/// the radar, and not one was nearer the airport than the radar. Nor is it a
/// datum shift, which is a 100 m effect. The heights in the same 45 rows were
/// right all along, to better than a metre — so position and height in the
/// TDWR block never shared a source.
///
/// # Rows with no volume
///
/// `LPLA` (Lajes) is in no epoch of either the Unidata bucket or the Google
/// mirror, back to 2011, and keeps everything it had. `KLIX` and `RODN` are
/// corrected from their most recent volumes, of 2023-11 and 2023-08; neither
/// has produced data since, and `RODN` is the one row where the volume and
/// `api.weather.gov` disagree past 250 m — 901 m — with the volume steady
/// across seven epochs. The volume wins, because it is the position the RDA
/// georeferences its own data against, and the disagreement is recorded here
/// rather than split.
///
/// # Why this is a seed and not the table
///
/// Everything above describes a snapshot taken on one day, and a binary that
/// can only ever know these 207 rows rots: a radar commissioned after the
/// build is a site the app cannot name, cannot draw and cannot centre on, and
/// no amount of care about the figures below changes that.
///
/// So this is the *seed* the process resolves its table from. It is private,
/// and reachable only through [`radars()`] — never by position, because a
/// runtime table can be a different length than this one and an index into
/// the array would then name a different radar than the one it was minted
/// for. [`resolve`] overlays the seed with what an install has learned from
/// the volumes it has actually read, and a later stage will overlay it again
/// from the network. A row here is a starting guess with a name attached, not
/// the last word.
const SEED: [RadarSite; 207] = [
    RadarSite {
        name: "KABR",
        lat: 45.45583,
        lon: -98.41333,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1302,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KABX",
        lat: 35.14972,
        lon: -106.82389,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 5870,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KAKQ",
        lat: 36.98405,
        lon: -77.00736,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 157,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KAMA",
        lat: 35.23333,
        lon: -101.70927,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 3622,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KAMX",
        lat: 25.61108,
        lon: -80.41267,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 14,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KAPX",
        lat: 44.90635,
        lon: -84.71954,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1464,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KARX",
        lat: 43.82278,
        lon: -91.19111,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1276,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KATX",
        lat: 48.19461,
        lon: -122.4957,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 528,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KBBX",
        lat: 39.49564,
        lon: -121.63161,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 173,
            tower_ft: 46,
        }),
    },
    RadarSite {
        name: "KBGM",
        lat: 42.1997,
        lon: -75.98473,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1606,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KBHX",
        lat: 40.49858,
        lon: -124.29217,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 2402,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KBIS",
        lat: 46.77083,
        lon: -100.76056,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1658,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KBLX",
        lat: 45.85378,
        lon: -108.6068,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 3638,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KBMX",
        lat: 33.17242,
        lon: -86.77016,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 645,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KBOX",
        lat: 41.95578,
        lon: -71.13686,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 118,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KBRO",
        lat: 25.916,
        lon: -97.41897,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 23,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KBUF",
        lat: 42.94879,
        lon: -78.73678,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 693,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KBYX",
        lat: 24.5975,
        lon: -81.70316,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 8,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KCAE",
        lat: 33.94872,
        lon: -81.11828,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 231,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KCBW",
        lat: 46.03925,
        lon: -67.80643,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 746,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KCBX",
        lat: 43.49021,
        lon: -116.23603,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 3091,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KCCX",
        lat: 40.92317,
        lon: -78.00372,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 2405,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KCLE",
        lat: 41.41322,
        lon: -81.85986,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 763,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KCLX",
        lat: 32.65553,
        lon: -81.0422,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 115,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KCRI",
        lat: 35.23833,
        lon: -97.46,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1201,
            tower_ft: 114,
        }),
    },
    RadarSite {
        name: "KCRP",
        lat: 27.78402,
        lon: -97.51125,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 45,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KCXX",
        lat: 44.511,
        lon: -73.16643,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 317,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KCYS",
        lat: 41.15192,
        lon: -104.80603,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 6128,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KDAX",
        lat: 38.50111,
        lon: -121.67783,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 30,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KDDC",
        lat: 37.76083,
        lon: -99.96889,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 2590,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KDFX",
        lat: 29.27314,
        lon: -100.28033,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1131,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KDGX",
        lat: 32.27994,
        lon: -89.98444,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 495,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KDIX",
        lat: 39.94709,
        lon: -74.41073,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 149,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KDLH",
        lat: 46.83695,
        lon: -92.20972,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1428,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KDMX",
        lat: 41.7312,
        lon: -93.72287,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 981,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KDOX",
        lat: 38.82577,
        lon: -75.44012,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 50,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KDTX",
        lat: 42.7,
        lon: -83.47166,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1102,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KDVN",
        lat: 41.61167,
        lon: -90.58083,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 754,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KDYX",
        lat: 32.5385,
        lon: -99.25433,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1517,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KEAX",
        lat: 38.81025,
        lon: -94.26447,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 995,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KEMX",
        lat: 31.89365,
        lon: -110.63025,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 5202,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KENX",
        lat: 42.58655,
        lon: -74.06409,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1854,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KEOX",
        lat: 31.46056,
        lon: -85.45939,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 472,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KEPZ",
        lat: 31.87306,
        lon: -106.698,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 4104,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KESX",
        lat: 35.70135,
        lon: -114.89165,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 4867,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KEVX",
        lat: 30.56503,
        lon: -85.92167,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 140,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KEWX",
        lat: 29.70406,
        lon: -98.02861,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 669,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KEYX",
        lat: 35.09785,
        lon: -117.56075,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 2776,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KFCX",
        lat: 37.0244,
        lon: -80.27397,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 2868,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KFDR",
        lat: 34.36219,
        lon: -98.97667,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1267,
            tower_ft: 46,
        }),
    },
    RadarSite {
        name: "KFDX",
        lat: 34.63417,
        lon: -103.61889,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 4650,
            tower_ft: 46,
        }),
    },
    RadarSite {
        name: "KFFC",
        lat: 33.36355,
        lon: -84.56595,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 858,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KFSD",
        lat: 43.58778,
        lon: -96.72945,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1430,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KFSX",
        lat: 34.57433,
        lon: -111.19845,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 7418,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KFTG",
        lat: 39.78664,
        lon: -104.54581,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 5497,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KFWS",
        lat: 32.573,
        lon: -97.30315,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 696,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KGGW",
        lat: 48.20636,
        lon: -106.6247,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 2303,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KGJX",
        lat: 39.06217,
        lon: -108.21376,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 10036,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KGLD",
        lat: 39.36694,
        lon: -101.70028,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 3651,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KGRB",
        lat: 44.49863,
        lon: -88.11111,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 709,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KGRK",
        lat: 30.72183,
        lon: -97.38294,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 538,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KGRR",
        lat: 42.89389,
        lon: -85.54489,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 778,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KGSP",
        lat: 34.88331,
        lon: -82.21983,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 955,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KGWX",
        lat: 33.89691,
        lon: -88.32919,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 509,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KGYX",
        lat: 43.8913,
        lon: -70.25636,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 409,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KHDC",
        lat: 30.51931,
        lon: -90.40736,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 43,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KHDX",
        lat: 33.077,
        lon: -106.12003,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 4222,
            tower_ft: 46,
        }),
    },
    RadarSite {
        name: "KHGX",
        lat: 29.4719,
        lon: -95.07873,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 18,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KHNX",
        lat: 36.31418,
        lon: -119.63214,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 243,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KHPX",
        lat: 36.73697,
        lon: -87.28558,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 564,
            tower_ft: 46,
        }),
    },
    RadarSite {
        name: "KHTX",
        lat: 34.93056,
        lon: -86.08361,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1760,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KICT",
        lat: 37.65445,
        lon: -97.44305,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1335,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KICX",
        lat: 37.59105,
        lon: -112.86218,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 10643,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KILN",
        lat: 39.42048,
        lon: -83.82145,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1056,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KILX",
        lat: 40.1505,
        lon: -89.33679,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 617,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KIND",
        lat: 39.7075,
        lon: -86.28028,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 790,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KINX",
        lat: 36.17513,
        lon: -95.56416,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 668,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KIWA",
        lat: 33.28923,
        lon: -111.66991,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1362,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KIWX",
        lat: 41.35861,
        lon: -85.7,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 960,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KJAX",
        lat: 30.48463,
        lon: -81.7019,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 62,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KJGX",
        lat: 32.67568,
        lon: -83.35083,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 521,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KJKL",
        lat: 37.59083,
        lon: -83.31306,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1364,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KLBB",
        lat: 33.65414,
        lon: -101.81416,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 3297,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KLCH",
        lat: 30.12531,
        lon: -93.21589,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 56,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KLGX",
        lat: 47.11694,
        lon: -124.10667,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 252,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KLIX",
        lat: 30.33667,
        lon: -89.82542,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 66,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KLNX",
        lat: 41.95794,
        lon: -100.57622,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 3015,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KLOT",
        lat: 41.60444,
        lon: -88.08444,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 663,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KLRX",
        lat: 40.73955,
        lon: -116.8027,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 6781,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KLSX",
        lat: 38.69861,
        lon: -90.68278,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 608,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KLTX",
        lat: 33.98915,
        lon: -78.42911,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 64,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KLVX",
        lat: 37.97528,
        lon: -85.94389,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 719,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KLWX",
        lat: 38.97611,
        lon: -77.4875,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 292,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KLZK",
        lat: 34.8365,
        lon: -92.26219,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 568,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KMAF",
        lat: 31.94346,
        lon: -102.18925,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 2897,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KMAX",
        lat: 42.08117,
        lon: -122.71737,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 7513,
            tower_ft: 46,
        }),
    },
    RadarSite {
        name: "KMBX",
        lat: 48.39305,
        lon: -100.86444,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1493,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KMHX",
        lat: 34.77591,
        lon: -76.87619,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 31,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KMKX",
        lat: 42.9679,
        lon: -88.55067,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 958,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KMLB",
        lat: 28.11319,
        lon: -80.65408,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 36,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KMOB",
        lat: 30.67945,
        lon: -88.24,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 208,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KMPX",
        lat: 44.84889,
        lon: -93.56553,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 988,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KMQT",
        lat: 46.53111,
        lon: -87.54833,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1411,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KMRX",
        lat: 36.16861,
        lon: -83.40195,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1337,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KMSX",
        lat: 47.041,
        lon: -113.98622,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 7930,
            tower_ft: 46,
        }),
    },
    RadarSite {
        name: "KMTX",
        lat: 41.26278,
        lon: -112.44778,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 6480,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KMUX",
        lat: 37.15522,
        lon: -121.89844,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 3469,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KMVX",
        lat: 47.52778,
        lon: -97.32555,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 986,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KMXX",
        lat: 32.53665,
        lon: -85.78975,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 446,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KNKX",
        lat: 32.91902,
        lon: -117.0418,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 955,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KNQA",
        lat: 35.34472,
        lon: -89.87334,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 338,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KOAX",
        lat: 41.32037,
        lon: -96.36682,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1148,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KOHX",
        lat: 36.24722,
        lon: -86.5625,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 579,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KOKX",
        lat: 40.86553,
        lon: -72.86391,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 85,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KOTX",
        lat: 47.68042,
        lon: -117.62678,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 2384,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KPAH",
        lat: 37.06833,
        lon: -88.77194,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 392,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KPBZ",
        lat: 40.53171,
        lon: -80.21796,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1185,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KPDT",
        lat: 45.69065,
        lon: -118.85293,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1515,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KPOE",
        lat: 31.15528,
        lon: -92.97611,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 408,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KPUX",
        lat: 38.45955,
        lon: -104.18135,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 5299,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KRAX",
        lat: 35.66552,
        lon: -78.48975,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 348,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KRGX",
        lat: 39.75406,
        lon: -119.46203,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 8299,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KRIW",
        lat: 43.06609,
        lon: -108.4773,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 5568,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KRLX",
        lat: 38.31111,
        lon: -81.72278,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1099,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KRTX",
        lat: 45.71504,
        lon: -122.965,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1614,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KSFX",
        lat: 43.1056,
        lon: -112.68613,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 4474,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KSGF",
        lat: 37.23524,
        lon: -93.40042,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1278,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KSHV",
        lat: 32.45083,
        lon: -93.84125,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 273,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KSJT",
        lat: 31.37128,
        lon: -100.4925,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1890,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KSOX",
        lat: 33.81773,
        lon: -117.636,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 3041,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KSRX",
        lat: 35.29042,
        lon: -94.36189,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 656,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KTBW",
        lat: 27.7055,
        lon: -82.40178,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 41,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KTFX",
        lat: 47.45958,
        lon: -111.38533,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 3740,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KTLH",
        lat: 30.39758,
        lon: -84.32894,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 63,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KTLX",
        lat: 35.33336,
        lon: -97.27776,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1213,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KTWX",
        lat: 38.99695,
        lon: -96.23255,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1367,
            tower_ft: 46,
        }),
    },
    RadarSite {
        name: "KTYX",
        lat: 43.7557,
        lon: -75.67986,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1846,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KUDX",
        lat: 44.12472,
        lon: -102.83,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 3081,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KUEX",
        lat: 40.32084,
        lon: -98.44195,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1976,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KVAX",
        lat: 30.89028,
        lon: -83.00181,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 217,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KVBX",
        lat: 34.83855,
        lon: -120.39792,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1257,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KVNX",
        lat: 36.74062,
        lon: -98.12772,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1210,
            tower_ft: 46,
        }),
    },
    RadarSite {
        name: "KVTX",
        lat: 34.41202,
        lon: -119.17875,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 2726,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KVWX",
        lat: 38.26025,
        lon: -87.72452,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 512,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KYUX",
        lat: 32.49528,
        lon: -114.65671,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 174,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "LPLA",
        lat: 38.73028,
        lon: -27.32167,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 3334 }),
    },
    RadarSite {
        name: "PABC",
        lat: 60.79194,
        lon: -161.87639,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 162,
            tower_ft: 30,
        }),
    },
    RadarSite {
        name: "PACG",
        lat: 56.85278,
        lon: -135.52916,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 207,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "PAEC",
        lat: 64.51139,
        lon: -165.295,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 59,
            tower_ft: 30,
        }),
    },
    RadarSite {
        name: "PAHG",
        lat: 60.72591,
        lon: -151.35147,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 243,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "PAIH",
        lat: 59.46077,
        lon: -146.30345,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 67,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "PAKC",
        lat: 58.67944,
        lon: -156.62944,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 63,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "PAPD",
        lat: 65.03511,
        lon: -147.50143,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 2593,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "PGUA",
        lat: 13.45583,
        lon: 144.81111,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 272,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "PHKI",
        lat: 21.89389,
        lon: -159.5525,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 226,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "PHKM",
        lat: 20.12528,
        lon: -155.77777,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 3852,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "PHMO",
        lat: 21.13278,
        lon: -157.18028,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1363,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "PHWA",
        lat: 19.095,
        lon: -155.56889,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1381,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "RKJK",
        lat: 35.92417,
        lon: 126.62222,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 78,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "RKSG",
        lat: 37.20757,
        lon: 127.28556,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1440,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "RODN",
        lat: 26.3078,
        lon: 127.90347,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 299,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "TJUA",
        lat: 18.11567,
        lon: -66.07816,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 2844,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "TJFK",
        lat: 40.589,
        lon: -73.88,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 112 }),
    },
    RadarSite {
        name: "TADW",
        lat: 38.695,
        lon: -76.845,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 346 }),
    },
    RadarSite {
        name: "TATL",
        lat: 33.647,
        lon: -84.262,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 1075 }),
    },
    RadarSite {
        name: "TBNA",
        lat: 35.98,
        lon: -86.662,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 817 }),
    },
    RadarSite {
        name: "TBOS",
        lat: 42.158,
        lon: -70.933,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 264 }),
    },
    RadarSite {
        name: "TBWI",
        lat: 39.09,
        lon: -76.63,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 297 }),
    },
    RadarSite {
        name: "TCLT",
        lat: 35.337,
        lon: -80.885,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 871 }),
    },
    RadarSite {
        name: "TCMH",
        lat: 40.006,
        lon: -82.715,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 1148 }),
    },
    RadarSite {
        name: "TCVG",
        lat: 38.898,
        lon: -84.58,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 1053 }),
    },
    RadarSite {
        name: "TDAL",
        lat: 32.926,
        lon: -96.968,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 622 }),
    },
    RadarSite {
        name: "TDAY",
        lat: 40.022,
        lon: -84.123,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 1019 }),
    },
    RadarSite {
        name: "TDCA",
        lat: 38.759,
        lon: -76.962,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 345 }),
    },
    RadarSite {
        name: "TDEN",
        lat: 39.727,
        lon: -104.526,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 5701 }),
    },
    RadarSite {
        name: "TDFW",
        lat: 33.065,
        lon: -96.918,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 585 }),
    },
    RadarSite {
        name: "TDTW",
        lat: 42.111,
        lon: -83.515,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 772 }),
    },
    RadarSite {
        name: "TEWR",
        lat: 40.594,
        lon: -74.27,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 136 }),
    },
    RadarSite {
        name: "TFLL",
        lat: 26.143,
        lon: -80.344,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 120 }),
    },
    RadarSite {
        name: "THOU",
        lat: 29.516,
        lon: -95.242,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 117 }),
    },
    RadarSite {
        name: "TIAD",
        lat: 39.084,
        lon: -77.529,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 473 }),
    },
    RadarSite {
        name: "TIAH",
        lat: 30.065,
        lon: -95.567,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 253 }),
    },
    RadarSite {
        name: "TICH",
        lat: 37.507,
        lon: -97.437,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 1351 }),
    },
    RadarSite {
        name: "TIDS",
        lat: 39.637,
        lon: -86.435,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 847 }),
    },
    RadarSite {
        name: "TLAS",
        lat: 36.144,
        lon: -115.007,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 2058 }),
    },
    RadarSite {
        name: "TLVE",
        lat: 41.29,
        lon: -82.008,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 931 }),
    },
    RadarSite {
        name: "TMCI",
        lat: 39.499,
        lon: -94.742,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 1090 }),
    },
    RadarSite {
        name: "TMCO",
        lat: 28.344,
        lon: -81.326,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 169 }),
    },
    RadarSite {
        name: "TMDW",
        lat: 41.651,
        lon: -87.73,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 763 }),
    },
    RadarSite {
        name: "TMEM",
        lat: 34.896,
        lon: -89.993,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 483 }),
    },
    RadarSite {
        name: "TMIA",
        lat: 25.758,
        lon: -80.491,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 125 }),
    },
    RadarSite {
        name: "TMKE",
        lat: 42.819,
        lon: -88.046,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 933 }),
    },
    RadarSite {
        name: "TMSP",
        lat: 44.871,
        lon: -92.933,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 1121 }),
    },
    RadarSite {
        name: "TMSY",
        lat: 30.022,
        lon: -90.403,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 99 }),
    },
    RadarSite {
        name: "TOKC",
        lat: 35.276,
        lon: -97.51,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 1308 }),
    },
    RadarSite {
        name: "TORD",
        lat: 41.797,
        lon: -87.858,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 744 }),
    },
    RadarSite {
        name: "TPBI",
        lat: 26.688,
        lon: -80.273,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 133 }),
    },
    RadarSite {
        name: "TPHL",
        lat: 39.949,
        lon: -75.07,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 153 }),
    },
    RadarSite {
        name: "TPHX",
        lat: 33.42,
        lon: -112.163,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 1089 }),
    },
    RadarSite {
        name: "TPIT",
        lat: 40.501,
        lon: -80.486,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 1386 }),
    },
    RadarSite {
        name: "TRDU",
        lat: 36.002,
        lon: -78.697,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 515 }),
    },
    RadarSite {
        name: "TSDF",
        lat: 38.046,
        lon: -85.611,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 731 }),
    },
    RadarSite {
        name: "TSJU",
        lat: 18.474,
        lon: -66.18,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 157 }),
    },
    RadarSite {
        name: "TSLC",
        lat: 40.967,
        lon: -111.93,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 4295 }),
    },
    RadarSite {
        name: "TSTL",
        lat: 38.805,
        lon: -90.489,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 647 }),
    },
    RadarSite {
        name: "TTPA",
        lat: 27.86,
        lon: -82.518,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 93 }),
    },
    RadarSite {
        name: "TTUL",
        lat: 36.071,
        lon: -95.826,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 823 }),
    },
];

/// Every radar this process knows about, resolved once and then fixed.
///
/// Holds its rows as `&'static [RadarSite]` because they are leaked on the way
/// in, which is what lets every lookup below keep handing out
/// `&'static RadarSite` the way the compiled-in array used to. Leaking is
/// honest here rather than a dodge: a table is resolved a bounded number of
/// times per process — once at startup, and once more on Android when the
/// config directory arrives — and then read for the life of the process, so
/// "lives forever" is a description of the value's real lifetime and not a
/// promise being smuggled past the borrow checker.
///
/// The name index is built with the rows and travels with them, so the two
/// cannot disagree about which radars exist. That was the one real hazard in
/// making the table resizable: a `LazyLock` name map beside a swappable row
/// list would answer for a table nobody could still see.
pub struct SiteTable {
    rows: &'static [RadarSite],
    by_name: HashMap<&'static str, &'static RadarSite>,
}

impl SiteTable {
    /// Every row, in seed order with anything learned appended.
    ///
    /// Callers may walk this and may enumerate it, but must not store the
    /// enumeration index anywhere that outlives the walk: the next table can
    /// be a different length, and position `n` in it is a different radar.
    /// Keep the `&'static RadarSite` itself, which stays valid and keeps
    /// naming the row it named.
    pub fn rows(&self) -> &'static [RadarSite] {
        self.rows
    }

    /// The row for an ICAO identifier, or `None` if this table has no such
    /// radar.
    pub fn get(&self, site: &str) -> Option<&'static RadarSite> {
        self.by_name.get(site).copied()
    }

    /// The row closest to `lat`/`lon`, with its distance in kilometres.
    pub fn nearest(&self, lat: f64, lon: f64) -> Option<(&'static RadarSite, f64)> {
        self.nearest_where(lat, lon, |_| true)
    }

    /// The closest operational WSR-88D, with its distance in kilometres.
    pub fn nearest_wsr88d(&self, lat: f64, lon: f64) -> Option<(&'static RadarSite, f64)> {
        self.nearest_where(lat, lon, |site| site.is_wsr88d() && site.is_operational())
    }

    fn nearest_where(
        &self,
        lat: f64,
        lon: f64,
        accept: impl Fn(&RadarSite) -> bool,
    ) -> Option<(&'static RadarSite, f64)> {
        if !lat.is_finite() || !lon.is_finite() {
            return None;
        }
        self.rows
            .iter()
            .filter(|site| accept(site))
            .map(|site| (site, distance_km(lat, lon, site.lat, site.lon)))
            // `total_cmp`, not `partial_cmp().unwrap()`: the distances are
            // finite given a finite input, but the unwrap would be a panic
            // path in a startup routine to save nothing.
            .min_by(|(_, a), (_, b)| a.total_cmp(b))
    }
}

/// Build a table from [`SEED`], overlaid by what an install has learned.
///
/// Each `(name, position)` pair either **moves** the seed row of that name
/// onto the position its own volume reported, or — and this is the point of
/// the whole exercise — **adds a row the seed has never heard of**. A radar
/// commissioned after this binary was built reaches the user as a real row
/// with its real ICAO, because the position was learned from a volume and the
/// name is the key that position was filed under.
///
/// The added row's name is leaked, because [`RadarSite::name`] is
/// `&'static str` and a four-byte identifier read out of a volume header at
/// runtime is not. That is a handful of bytes per radar this install has ever
/// opened, once per resolution, against a `MAX_REMEMBERED_SITES` cap upstream.
///
/// Heights come from [`SitePosition::heights_over`], which keeps a seed row's
/// finer figures wherever the volume's whole metres cannot contradict them —
/// so an overlay never costs a row the precision it already had. A row the
/// seed never had takes the volume's heights outright, and therefore always
/// records *an* elevation: `every_site_records_an_elevation` holds over a
/// learned row for the same reason it holds over a seeded one, which matters
/// because the alternative is a section anchored at sea level.
pub fn build_table<'a, I>(learned: I) -> &'static SiteTable
where
    I: IntoIterator<Item = (&'a str, SitePosition)>,
{
    let mut rows: Vec<RadarSite> = SEED.to_vec();
    let mut at: HashMap<&'static str, usize> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| (row.name, index))
        .collect();

    for (name, position) in learned {
        match at.get(name) {
            Some(&index) => {
                let row = &rows[index];
                rows[index] = position.applied_to_named(row.name, row.heights);
            }
            None => {
                // Leaked so the row can carry it as `&'static str`. The name
                // is also re-keyed into `at`, so a second entry for the same
                // unseeded site updates the row it already added rather than
                // filing a duplicate radar beside it.
                let name: &'static str = Box::leak(name.to_owned().into_boxed_str());
                at.insert(name, rows.len());
                rows.push(position.applied_to_named(name, None));
            }
        }
    }

    let rows: &'static [RadarSite] = Box::leak(rows.into_boxed_slice());
    let by_name = rows.iter().map(|row| (row.name, row)).collect();
    Box::leak(Box::new(SiteTable { rows, by_name }))
}

/// The seed on its own, built once and shared by every resolution that learns
/// nothing.
///
/// Almost every process is this one — a fresh install, and every test that
/// constructs an app against an empty store — and giving them all the same
/// table means the overwhelmingly common `resolve` is a pointer store rather
/// than a fresh leak of 207 rows.
fn seed_table() -> &'static SiteTable {
    static SEED_TABLE: LazyLock<&'static SiteTable> =
        LazyLock::new(|| build_table(std::iter::empty()));
    *SEED_TABLE
}

/// The table this process resolved, or `None` until something resolves one.
static RESOLVED: RwLock<Option<&'static SiteTable>> = RwLock::new(None);

/// The table every lookup in this module reads.
///
/// Falls back to [`seed_table`] rather than panicking when nothing has
/// resolved yet: a crate-level test, a benchmark or a tool that never builds
/// an app still gets the compiled-in radars, which is exactly where the app
/// was before any of this existed.
pub fn table() -> &'static SiteTable {
    // `into_inner` rather than `expect`: a poisoned lock here means some other
    // thread panicked while swapping a pointer, and the pointer it was
    // swapping is valid either way. Refusing to draw the map over it would
    // turn an unrelated panic into a second one.
    match *RESOLVED.read().unwrap_or_else(|e| e.into_inner()) {
        Some(table) => table,
        None => seed_table(),
    }
}

/// Resolve the process-wide site table from what this install has learned.
///
/// # Call this before the first paint
///
/// A site's position or name that arrives *late* moves a marker, a label and
/// a section's height datum under a user who is already looking at them,
/// which is the one thing the reopen-is-1:1 rule forbids. So resolution
/// belongs with the rest of the startup read, beside the config load and
/// ahead of any frame — `App::new`, and again in `set_config_dir` where
/// Android finally has a store to read from. Both run before a frame exists.
///
/// Resolving is therefore idempotent-shaped rather than one-shot: Android
/// genuinely resolves twice, and a `OnceLock` would have made the second
/// attempt — the one that has the learned positions — silently do nothing.
///
/// Rows from the previous table stay valid; they are leaked, so a
/// `&'static RadarSite` handed out before a resolution keeps naming the radar
/// it named. What it must not be is an *index*, which is why nothing outside
/// this module can reach the rows by position.
pub fn resolve<'a, I>(learned: I) -> &'static SiteTable
where
    I: IntoIterator<Item = (&'a str, SitePosition)>,
{
    let learned: Vec<(&'a str, SitePosition)> = learned.into_iter().collect();
    let table = if learned.is_empty() {
        seed_table()
    } else {
        build_table(learned)
    };
    *RESOLVED.write().unwrap_or_else(|e| e.into_inner()) = Some(table);
    table
}

/// Every radar this process knows about.
///
/// The replacement for the compiled-in array: same rows on a fresh install,
/// plus whatever this one has learned. Walk it, filter it, count it — but see
/// [`SiteTable::rows`] on why an index into it must not outlive the walk.
pub fn radars() -> &'static [RadarSite] {
    table().rows()
}

/// The row for an ICAO identifier, or `None` if this process knows no such
/// radar.
pub fn get_radar_site(site: &str) -> Option<&'static RadarSite> {
    table().get(site)
}

/// Great-circle distance between two coordinates, in kilometres.
///
/// Haversine rather than the cheaper equirectangular approximation: the caller
/// compares sites up to a continent apart (a fix in Hawaii against a table that
/// is mostly CONUS), and the flat approximation's error grows with both
/// separation and latitude — exactly the regime the comparison runs in.
///
/// On [`crate::types::EARTH_RADIUS_KM`], the workspace's one sphere. This
/// used to carry its own `6371.0088`, the IUGG mean to four more decimals;
/// the two are 1.4e-6 % apart, which is 9 m across a continent and cannot
/// change which radar is nearest.
pub fn distance_km(lat_a: f64, lon_a: f64, lat_b: f64, lon_b: f64) -> f64 {
    let (lat_a_rad, lat_b_rad) = (lat_a.to_radians(), lat_b.to_radians());
    let d_lat = (lat_b - lat_a).to_radians();
    let d_lon = (lon_b - lon_a).to_radians();

    let h = (d_lat / 2.0).sin().powi(2)
        + lat_a_rad.cos() * lat_b_rad.cos() * (d_lon / 2.0).sin().powi(2);
    // `asin(sqrt(h))` rather than `atan2`: h is clamped below, so the numerically
    // delicate case `atan2` exists to handle cannot arise here.
    2.0 * EARTH_RADIUS_KM * h.clamp(0.0, 1.0).sqrt().asin()
}

impl RadarSite {
    /// Whether this is a Terminal Doppler Weather Radar rather than a WSR-88D.
    ///
    /// The distinction is load-bearing rather than trivia: the Level II archive
    /// this app reads carries WSR-88D volume scans only, so a TDWR site has no
    /// reflectivity to show through that path. [`radars()`] lists both because the
    /// map draws a marker for every site.
    ///
    /// The `T` prefix identifies the 45 TDWRs, with one exception that a naive
    /// `starts_with('T')` gets wrong: `TJUA` is San Juan's WSR-88D.
    pub fn is_tdwr(&self) -> bool {
        self.name.starts_with('T') && self.name != "TJUA"
    }

    /// Whether this site is a WSR-88D, the network the Level II archive covers.
    pub fn is_wsr88d(&self) -> bool {
        !self.is_tdwr()
    }

    /// Whether this site runs an operational scan an ordinary viewer can rely on.
    ///
    /// `KCRI` is the Radar Operations Center's test bed in Norman. It is a real
    /// WSR-88D and it does reach the archive, but it scans to whatever schedule
    /// the ROC is testing that day rather than continuously. It also sits 0.4 km
    /// closer to downtown Oklahoma City than `KTLX` does, so *every* automatic
    /// pick for the Oklahoma City metro would land on it and intermittently show
    /// an empty map.
    ///
    /// Only automatic selection consults this. The site stays in [`radars()`], the
    /// map still draws it, and a user who picks it by hand still gets it.
    pub fn is_operational(&self) -> bool {
        self.name != "KCRI"
    }
}

/// The radar site closest to `lat`/`lon`, with its distance in kilometres.
///
/// Considers every site including TDWRs. Callers picking a site to *display*
/// almost certainly want [`nearest_wsr88d_site`] instead.
///
/// Returns `None` only for a non-finite input. A NaN coordinate would otherwise
/// compare `false` against every candidate and silently yield whichever site
/// happens to sit first in [`radars()`], which reads as a deliberate choice.
///
/// No distance cap: a caller in Europe gets the nearest NEXRAD and a very large
/// number, and it is the caller's business whether that is useful. Callers that
/// care should test the returned distance rather than expect `None`.
pub fn nearest_radar_site(lat: f64, lon: f64) -> Option<(&'static RadarSite, f64)> {
    table().nearest(lat, lon)
}

/// The closest site an automatic pick should open on, with its distance in km.
///
/// This is the one startup site selection wants: the nearest operational
/// WSR-88D. Downtown Oklahoma City illustrates both filters at once — the
/// literal nearest site is the TDWR `TOKC`, and the nearest WSR-88D is the ROC
/// test bed `KCRI`. Neither reliably shows a viewer reflectivity, and the site
/// a person there actually wants is the third one out, `KTLX`.
pub fn nearest_wsr88d_site(lat: f64, lon: f64) -> Option<(&'static RadarSite, f64)> {
    table().nearest_wsr88d(lat, lon)
}

#[cfg(test)]
mod nearest_tests {
    use super::*;

    /// Two points ~111 km apart along a meridian, where the expected answer is
    /// a definition rather than a measurement.
    #[test]
    fn one_degree_of_latitude_is_about_111_km() {
        let d = distance_km(35.0, -97.0, 36.0, -97.0);
        assert!((d - 111.19).abs() < 0.5, "{d}");
    }

    #[test]
    fn a_point_is_zero_km_from_itself() {
        assert_eq!(distance_km(35.3331, -97.2778, 35.3331, -97.2778), 0.0);
    }

    /// Longitude wrapping is the case an unsigned subtraction gets wrong: these
    /// are 2° apart across the antimeridian, not 358°.
    #[test]
    fn distance_wraps_across_the_antimeridian() {
        let d = distance_km(51.0, 179.0, 51.0, -179.0);
        assert!(d < 200.0, "{d} km — the meridian wrap was not handled");
    }

    /// Downtown Oklahoma City resolves to KTLX, which is the site the old
    /// hardcoded default happened to name. The point is that it is now *derived*.
    ///
    /// This is also the case that motivates the TDWR filter: the literal nearest
    /// site to this coordinate is `TOKC`, which has no Level II data.
    #[test]
    fn oklahoma_city_resolves_to_ktlx() {
        let (site, dist) = nearest_wsr88d_site(35.4676, -97.5164).expect("a finite coordinate");
        assert_eq!(site.name, "KTLX");
        assert!(dist < 50.0, "{dist}");

        // Both filters are doing work here, and a change to either would
        // otherwise silently stop mattering while the assertion above still
        // passed for the wrong reason.
        let (unfiltered, _) = nearest_radar_site(35.4676, -97.5164).expect("a finite coordinate");
        assert_eq!(
            unfiltered.name, "TOKC",
            "the literal nearest site is a TDWR"
        );

        let (nearest_88d, _) = table()
            .nearest_where(35.4676, -97.5164, RadarSite::is_wsr88d)
            .expect("a finite coordinate");
        assert_eq!(
            nearest_88d.name, "KCRI",
            "the nearest WSR-88D is the ROC test bed"
        );
    }

    /// The regression the whole feature exists for: somewhere far from Oklahoma
    /// must not resolve to Oklahoma's radar.
    #[test]
    fn seattle_does_not_resolve_to_an_oklahoma_radar() {
        let (site, _) = nearest_wsr88d_site(47.6062, -122.3321).expect("a finite coordinate");
        assert_eq!(site.name, "KATX");
    }

    /// Miami sits beside `TMIA`, so this is a second independent check that the
    /// TDWR filter holds in a different part of the table.
    #[test]
    fn miami_resolves_to_the_south_florida_wsr88d() {
        let (site, _) = nearest_wsr88d_site(25.7617, -80.1918).expect("a finite coordinate");
        assert_eq!(site.name, "KAMX");
    }

    /// Non-CONUS coverage: the table holds Alaska, Hawaii, Puerto Rico and Guam,
    /// and a naive CONUS-only assumption would strand these users.
    #[test]
    fn outlying_coverage_resolves_locally_rather_than_to_the_mainland() {
        for (lat, lon, expected) in [
            (21.3069, -157.8583, "PHMO"),
            (61.2181, -149.9003, "PAHG"),
            (18.4655, -66.1057, "TJUA"),
            (13.4443, 144.7937, "PGUA"),
        ] {
            let (site, dist) = nearest_wsr88d_site(lat, lon).expect("a finite coordinate");
            assert_eq!(site.name, expected, "at {lat},{lon} (got {dist} km)");
        }
    }

    /// `TJUA` is San Juan's WSR-88D, not a TDWR, and a `starts_with('T')` test
    /// would wrongly exclude the only Level II site serving Puerto Rico.
    #[test]
    fn tjua_is_not_treated_as_a_tdwr() {
        let tjua = get_radar_site("TJUA").expect("TJUA is in the table");
        assert!(tjua.is_wsr88d());
        assert!(!tjua.is_tdwr());
    }

    /// Pins the split so a table edit that adds or drops a site is visible here
    /// rather than silently changing what startup selection can choose from.
    #[test]
    fn the_table_splits_into_45_tdwrs_and_the_wsr88d_network() {
        let tdwrs = SEED.iter().filter(|s| s.is_tdwr()).count();
        assert_eq!(tdwrs, 45);
        assert_eq!(SEED.len() - tdwrs, 162);
    }

    /// A NaN must not silently degrade to "the first entry in the table".
    #[test]
    fn a_non_finite_coordinate_has_no_nearest_site() {
        assert!(nearest_wsr88d_site(f64::NAN, -97.0).is_none());
        assert!(nearest_wsr88d_site(35.0, f64::INFINITY).is_none());
    }

    /// Every row that moved further than one 250 m data cell onto the
    /// position its own volume reports, as (site, old lat, old lon, new lat,
    /// new lon, the epoch the volume came from).
    ///
    /// Pinned by value so a re-import of the table from whatever list it came
    /// from originally cannot quietly put them back. Forty-five of the fifty
    /// are TDWRs — see [`radars()`] on why that is one defect and not
    /// forty-five.
    const MOVED_ONTO_ITS_VOLUME: [(&str, f64, f64, f64, f64, &str); 50] = [
        (
            "RKSG",
            36.95972,
            127.01833,
            37.20757,
            127.28556,
            "2026/08/10",
        ),
        ("TICH", 37.4069, -97.4764, 37.507, -97.437, "2026/06/25"),
        ("TMCO", 28.2584, -81.3133, 28.344, -81.326, "2026/08/10"),
        ("TMSY", 29.9385, -90.3811, 30.022, -90.403, "2026/08/10"),
        ("TMDW", 41.69, -87.8034, 41.651, -87.73, "2026/08/10"),
        ("TMKE", 42.7619, -87.9994, 42.819, -88.046, "2026/08/10"),
        ("TPHX", 33.3678, -112.158, 33.42, -112.163, "2026/08/10"),
        ("TDTW", 42.071, -83.4704, 42.111, -83.515, "2026/08/10"),
        ("TMSP", 44.8197, -92.9392, 44.871, -92.933, "2026/08/10"),
        ("TMCI", 39.4488, -94.7396, 39.499, -94.742, "2026/08/10"),
        ("KIWX", 41.40861, -85.7, 41.35861, -85.7, "2026/08/10"),
        ("TTUL", 36.0236, -95.8175, 36.071, -95.826, "2026/08/10"),
        ("TPHL", 39.9084, -75.0426, 39.949, -75.07, "2026/08/10"),
        ("TIDS", 39.5978, -86.4085, 39.637, -86.435, "2026/08/10"),
        ("TSJU", 18.4313, -66.1722, 18.474, -66.18, "2026/08/10"),
        ("TSTL", 38.7668, -90.4698, 38.805, -90.489, "2026/08/10"),
        ("TTPA", 27.8196, -82.5179, 27.86, -82.518, "2026/08/10"),
        ("TPIT", 40.4641, -80.4697, 40.501, -80.486, "2026/08/10"),
        ("TOKC", 35.2474, -97.5395, 35.276, -97.51, "2026/08/10"),
        ("TSDF", 38.0109, -85.5995, 38.046, -85.611, "2026/08/10"),
        ("TDAY", 39.9875, -84.1102, 40.022, -84.123, "2026/08/10"),
        ("TIAH", 30.0297, -95.5708, 30.065, -95.567, "2026/08/10"),
        ("TSLC", 40.9341, -111.9214, 40.967, -111.93, "2026/08/10"),
        ("TPBI", 26.6572, -80.2586, 26.688, -80.273, "2026/07/28"),
        ("TLVE", 41.2805, -81.9659, 41.29, -82.008, "2026/08/10"),
        ("TDFW", 33.0396, -96.8974, 33.065, -96.918, "2026/08/10"),
        ("TORD", 41.7712, -87.8363, 41.797, -87.858, "2026/08/10"),
        ("TIAD", 39.0675, -77.5012, 39.084, -77.529, "2026/08/10"),
        ("TADW", 38.6704, -76.8446, 38.695, -76.845, "2026/08/10"),
        ("TJFK", 40.5668, -73.8874, 40.589, -73.88, "2026/08/10"),
        ("TDAL", 32.9076, -96.9568, 32.926, -96.968, "2026/08/10"),
        ("TRDU", 35.9898, -78.6787, 36.002, -78.697, "2026/08/10"),
        ("TCVG", 38.8799, -84.5737, 38.898, -84.58, "2026/08/10"),
        ("TCMH", 39.9878, -82.71, 40.006, -82.715, "2026/08/10"),
        ("TFLL", 26.1263, -80.3478, 26.143, -80.344, "2026/08/10"),
        ("THOU", 29.5328, -95.2444, 29.516, -95.242, "2026/08/10"),
        ("TEWR", 40.588, -74.2503, 40.594, -74.27, "2026/08/10"),
        ("TLAS", 36.1292, -115.0147, 36.144, -115.007, "2026/08/10"),
        ("TDCA", 38.7474, -76.9509, 38.759, -76.962, "2026/08/10"),
        ("TDEN", 39.7256, -104.5431, 39.727, -104.526, "2026/08/10"),
        ("TCLT", 35.3269, -80.8772, 35.337, -80.885, "2026/08/10"),
        ("TMEM", 34.8867, -90.0007, 34.896, -89.993, "2026/08/10"),
        ("TATL", 33.6433, -84.2524, 33.647, -84.262, "2026/08/10"),
        (
            "KFDX",
            34.63528,
            -103.62944,
            34.63417,
            -103.61889,
            "2026/08/10",
        ),
        (
            "RODN",
            26.30194,
            127.90972,
            26.3078,
            127.90347,
            "2023/06/01",
        ),
        ("TBOS", 42.1515, -70.9302, 42.158, -70.933, "2026/08/10"),
        ("TBWI", 39.087, -76.6276, 39.09, -76.63, "2026/08/10"),
        ("TBNA", 35.9767, -86.6618, 35.98, -86.662, "2026/08/10"),
        ("TMIA", 25.7555, -80.4932, 25.758, -80.491, "2026/08/10"),
        (
            "PGUA",
            13.45444,
            144.80833,
            13.45583,
            144.81111,
            "2026/08/10",
        ),
    ];

    /// The rows this table corrected against their own Level II volume, as
    /// (site, the height it recorded before, the height its volume reports).
    ///
    /// Feet, on [`Datum::SiteBase`]. Measured by `site_elev_probe` on the
    /// `campaign-harness` branch over one volume per site.
    const CORRECTED_AGAINST_A_VOLUME: [(&str, i32, i32); 50] = [
        ("KAKQ", 112, 157),
        ("KAMA", 3587, 3622),
        ("KATX", 494, 528),
        ("KBLX", 3598, 3638),
        ("KCBX", 3061, 3091),
        ("KCLX", 97, 115),
        ("KDTX", 1072, 1102),
        ("KENX", 1826, 1854),
        ("KEOX", 434, 472),
        ("KEWX", 633, 669),
        ("KEYX", 2757, 2776),
        ("KFWS", 683, 696),
        ("KGGW", 2276, 2303),
        ("KGJX", 9992, 10036),
        ("KGRB", 682, 709),
        ("KGSP", 940, 955),
        ("KGWX", 476, 509),
        ("KHPX", 576, 564),
        ("KICX", 10600, 10643),
        ("KILX", 582, 617),
        ("KIWA", 1353, 1362),
        ("KJAX", 33, 62),
        ("KLBB", 3259, 3297),
        ("KLCH", 13, 56),
        ("KLIX", 24, 66),
        ("KLNX", 2970, 3015),
        ("KLRX", 6744, 6781),
        ("KMAF", 2868, 2897),
        ("KMLB", 99, 36),
        ("KMPX", 946, 988),
        ("KMSX", 7855, 7930),
        ("KMTX", 6460, 6480),
        ("KMXX", 400, 446),
        ("KNQA", 282, 338),
        ("KPUX", 5249, 5299),
        ("KRLX", 1080, 1099),
        ("KSOX", 3027, 3041),
        ("KTFX", 3714, 3740),
        ("KUDX", 3016, 3081),
        ("KVAX", 178, 217),
        ("KVBX", 1233, 1257),
        ("PACG", 270, 207),
        ("PAEC", 54, 59),
        ("PGUA", 264, 272),
        ("RKSG", 52, 1440),
        ("PHKI", 179, 226),
        ("PHKM", 3812, 3852),
        ("PHWA", 1370, 1381),
        ("RODN", 218, 299),
        ("TJUA", 2794, 2844),
    ];

    /// Every row must record an elevation.
    ///
    /// Six did not — KDGX, KFSX, KLWX, KRTX, KSRX, KVWX, all of them
    /// `-99999` sentinels in the source the table was generated from, turned
    /// into `None` by the `Option<i32>` refactor and never filled in. A row
    /// without one is not inert: it is the datum a cross-section's height axis
    /// is anchored on, and the old lookup answered sea level for it, which
    /// reads as a measurement rather than as a gap.
    ///
    /// This is the loud failure the elevation deserves, moved to where it can
    /// be seen — a test, rather than a render that silently sits 89 m low.
    #[test]
    fn every_site_records_an_elevation() {
        let missing: Vec<&str> = radars()
            .iter()
            .filter(|s| s.heights.is_none())
            .map(|s| s.name)
            .collect();
        assert!(
            missing.is_empty(),
            "these sites record no elevation and would anchor a section at sea \
             level: {missing:?}",
        );
    }

    /// Recording *an* elevation is not enough: it has to be the one every
    /// render path asks for.
    ///
    /// `every_site_records_an_elevation` would pass on a table where every
    /// row carried only a base, and every feedhorn lookup would then skip
    /// every row and answer 0 ft — the same sea-level hole in a new shape.
    /// This closes it against the datum the callers actually name.
    #[test]
    fn every_site_answers_the_feedhorn_datum() {
        let missing: Vec<&str> = radars()
            .iter()
            .filter(|s| s.height_ft(Datum::Feedhorn).is_none())
            .map(|s| s.name)
            .collect();
        assert!(missing.is_empty(), "no feedhorn height: {missing:?}");
    }

    /// The rows that cannot answer [`Datum::SiteBase`], named.
    ///
    /// Not a defect — a TDWR volume reports one height and copies it into the
    /// tower field, so there is no base to record — but it is the one place
    /// `height_ft` returns `None` for a shipped row, and a row joining or
    /// leaving that set should be visible rather than inferred.
    #[test]
    fn only_the_single_height_rows_lack_a_base() {
        let no_base: Vec<&str> = SEED
            .iter()
            .filter(|s| s.height_ft(Datum::SiteBase).is_none())
            .map(|s| s.name)
            .collect();
        assert_eq!(no_base.len(), 46, "{no_base:?}");
        assert!(no_base.contains(&"LPLA"), "Lajes carries a single height");
        let tdwrs = no_base.iter().filter(|n| **n != "LPLA").count();
        assert_eq!(tdwrs, 45, "every TDWR and nothing else: {no_base:?}");
        assert!(
            no_base
                .iter()
                .all(|n| get_radar_site(n).is_some_and(|s| s.is_tdwr() || *n == "LPLA"))
        );
    }

    /// The two datums are a tower apart, everywhere both are recorded.
    ///
    /// This is the property the old single `elev` could not express and the
    /// reason a consumer has to name one: had the gap been a foot or two,
    /// nothing here would matter. It is 30–115 ft.
    #[test]
    fn the_two_datums_are_a_tower_apart() {
        let mut gaps: Vec<i32> = SEED
            .iter()
            .filter_map(|s| Some(s.height_ft(Datum::Feedhorn)? - s.height_ft(Datum::SiteBase)?))
            .collect();
        gaps.sort_unstable();
        assert_eq!(gaps.len(), 161, "every row that records both");
        assert_eq!(*gaps.first().expect("non-empty"), 30, "the shortest tower");
        assert_eq!(*gaps.last().expect("non-empty"), 114, "the tallest tower");
    }

    /// The 50 rows whose height was corrected against their own volume,
    /// pinned by value.
    ///
    /// The table used to hold one number per row and a note saying six had
    /// been checked. Checking all 207 against one archive volume each found
    /// these disagreeing with the height their own RDA reports by more than
    /// the whole-metre rounding of the field — from 63 ft high to 81 ft low.
    /// KMSX, the one the old note called unexplained, is item 31 of 50 rather
    /// than a singleton.
    ///
    /// `RKSG` is the fiftieth and arrived a campaign later: the pass that
    /// corrected the other 49 left it alone because its *coordinates* were
    /// 36 km out, and a row holding Camp Humphreys' height over Osan's
    /// position would have been incoherent rather than merely stale. Both
    /// halves now come from the same volume.
    ///
    /// Pinned as (site, what it said, what it says now) so a re-import of the
    /// table from whatever list it originally came from cannot quietly put
    /// them back.
    #[test]
    fn the_corrected_rows_carry_the_height_their_volume_reports() {
        for (name, was, now) in CORRECTED_AGAINST_A_VOLUME {
            let site = get_radar_site(name).expect("in the table");
            assert_eq!(site.height_ft(Datum::SiteBase), Some(now), "{name}");
            assert_ne!(was, now, "{name} is listed as corrected but did not move");
        }
        assert_eq!(CORRECTED_AGAINST_A_VOLUME.len(), 50);
    }

    /// The rows that moved onto their volume's position carry it, and are
    /// not where they used to be.
    ///
    /// Both halves matter. Asserting only the new value would pass on a table
    /// that had never moved if the pin were generated from it; asserting the
    /// row is no longer at the old coordinates is what makes this a
    /// correction rather than a description.
    #[test]
    fn the_moved_rows_sit_where_their_volume_says() {
        for (name, was_lat, was_lon, lat, lon, epoch) in MOVED_ONTO_ITS_VOLUME {
            let site = get_radar_site(name).expect("in the table");
            assert_eq!(site.lat, lat, "{name} latitude");
            assert_eq!(site.lon, lon, "{name} longitude");
            assert!(
                distance_km(was_lat, was_lon, lat, lon) > 0.25,
                "{name} is listed as moved but did not move a data cell",
            );
            assert_eq!(epoch.len(), "YYYY/MM/DD".len(), "{name} epoch");
        }
        assert_eq!(MOVED_ONTO_ITS_VOLUME.len(), 50);
    }

    /// The far end of the correction, named: `RKSG` is a relocated radar, not
    /// a mis-transcribed one.
    ///
    /// It is the only row in the table whose position and height were *both*
    /// wrong, and the only one wrong by more than a few kilometres. Osan is
    /// 36 km from Camp Humphreys and 1388 ft below it, so a section anchored
    /// on the old row put the beam through the wrong air over the wrong
    /// ground.
    #[test]
    fn rksg_is_at_camp_humphreys_and_not_at_osan() {
        let site = get_radar_site("RKSG").expect("in the table");
        assert_eq!((site.lat, site.lon), (37.20757, 127.28556));
        assert_eq!(site.height_ft(Datum::SiteBase), Some(1440));
        assert_eq!(site.height_ft(Datum::Feedhorn), Some(1519));
        assert!(
            distance_km(site.lat, site.lon, 36.95972, 127.01833) > 35.0,
            "the pre-move Osan coordinates are 36 km away and must not come back",
        );
    }

    /// A digest over every row's name and coordinates.
    ///
    /// [`MOVED_ONTO_ITS_VOLUME`] names the 50 rows that moved far enough to
    /// argue about; 143 more moved by tens of metres, where a literal list
    /// would be noise but a silent revert would still be a defect. This
    /// covers all 207 at once: change any row's position by one place in the
    /// last decimal and this fails.
    ///
    /// FNV-1a over the IEEE bits, so it is exact rather than tolerant —
    /// tolerance is what the named tables are for.
    #[test]
    fn every_rows_coordinates_are_pinned() {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let mut eat = |bytes: &[u8]| {
            for byte in bytes {
                hash ^= u64::from(*byte);
                hash = hash.wrapping_mul(0x1000_0000_01b3);
            }
        };
        for site in SEED.iter() {
            eat(site.name.as_bytes());
            eat(&site.lat.to_bits().to_be_bytes());
            eat(&site.lon.to_bits().to_be_bytes());
        }
        assert_eq!(
            hash, 0x67b7_7f0e_1c1c_defc,
            "a row's coordinates changed; if that was deliberate, re-measure \
             against the volumes rather than editing this number",
        );
    }

    /// Every site must be reachable as its own nearest neighbour, which catches
    /// a transposed lat/lon in any single table row.
    #[test]
    fn every_site_is_its_own_nearest_neighbour() {
        for site in radars() {
            let (found, dist) =
                nearest_radar_site(site.lat, site.lon).expect("table coordinates are finite");
            assert_eq!(
                found.name, site.name,
                "{} resolved to {} at {} km",
                site.name, found.name, dist
            );
        }
    }
}
