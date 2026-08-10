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
    /// `LPLA` (Lajes) is here too, for a different reason — see [`RADARS`].
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

/// Every radar site, with the heights its own Level II volume reports.
///
/// # Where the heights come from
///
/// One archive volume per site, read out of the Volume Data Block of its
/// first message 31 — 205 of the 207 rows, fetched from the public Google
/// mirror of the Level II archive by the `site_elev_probe` instrument on the
/// `campaign-harness` branch, which is also what measured everything claimed
/// below. The two rows with no volume in any year of the mirror that was
/// tried are `KCRI` (the ROC test bed) and `LPLA` (Lajes); both keep the
/// height the table already carried, and `KCRI`'s tower comes from the
/// published station record rather than from a volume.
///
/// # What the measurement found
///
/// Before it, the table held one `elev` per row and a note saying six rows
/// had been checked, of which five sat on `site_height` and KMSX sat on
/// neither. Over 205 rows:
///
/// * 139 sat on `site_height` within 2 ft. **None** sat on
///   `site_height + tower_height` — the one row classified that way, PACG,
///   is 63 ft above its own volume's base and lands on the feedhorn by
///   arithmetic accident.
/// * All 45 TDWR rows sat within 3.2 ft of the single height their volumes
///   report, 29 of them within 2 ft. The asymmetry — every delta positive —
///   is the archive truncating metres downward rather than rounding.
/// * **50 rows sat on neither**, by −63 to +81 ft. KMSX was one of 50, not a
///   singleton, so the six-row generalisation was wrong about the rule *and*
///   wrong about the exception. Forty-nine of them now carry the height their
///   volume reports (the fiftieth is `RKSG`, below); every other row keeps the
///   figure it had, which is the more precise of the two wherever they agree
///   (the archive's is a whole-metre figure, this table's a whole-foot one).
///
/// # What is still wrong here
///
/// `RKSG` deliberately keeps its old height. Its volume and the published
/// station record agree the RDA is at 37.2076, 127.2856 at 439 m — 40 km and
/// 1388 ft from the 36.95972, 127.01833 at 52 ft this row carries, which is
/// the pre-move Osan location. The row is wrong in its *coordinates*, and
/// giving it Camp Humphreys' height while it keeps Osan's position would
/// make a self-consistent wrong row into an incoherent one. Fixing the
/// coordinates is a separate change.
///
/// A further 54 rows differ from their volume's reported position by more
/// than 0.002°, most of them TDWRs and none by more than 0.11°. That is
/// recorded, not corrected, for the same reason.
pub const RADARS: [RadarSite; 207] = [
    RadarSite {
        name: "KABR",
        lat: 45.45583,
        lon: -98.41306,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1302,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KABX",
        lat: 35.14972,
        lon: -106.82333,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 5870,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KAKQ",
        lat: 36.98389,
        lon: -77.0075,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 157,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KAMA",
        lat: 35.23333,
        lon: -101.70889,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 3622,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KAMX",
        lat: 25.61056,
        lon: -80.41306,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 14,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KAPX",
        lat: 44.90722,
        lon: -84.71972,
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
        lat: 48.19472,
        lon: -122.49444,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 528,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KBBX",
        lat: 39.49611,
        lon: -121.63167,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 173,
            tower_ft: 46,
        }),
    },
    RadarSite {
        name: "KBGM",
        lat: 42.19972,
        lon: -75.985,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1606,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KBHX",
        lat: 40.49833,
        lon: -124.29194,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 2402,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KBIS",
        lat: 46.77083,
        lon: -100.76028,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1658,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KBLX",
        lat: 45.85389,
        lon: -108.60611,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 3638,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KBMX",
        lat: 33.17194,
        lon: -86.76972,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 645,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KBOX",
        lat: 41.95583,
        lon: -71.1375,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 118,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KBRO",
        lat: 25.91556,
        lon: -97.41861,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 23,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KBUF",
        lat: 42.94861,
        lon: -78.73694,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 693,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KBYX",
        lat: 24.59694,
        lon: -81.70333,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 8,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KCAE",
        lat: 33.94861,
        lon: -81.11861,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 231,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KCBW",
        lat: 46.03917,
        lon: -67.80694,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 746,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KCBX",
        lat: 43.49083,
        lon: -116.23444,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 3091,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KCCX",
        lat: 40.92306,
        lon: -78.00389,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 2405,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KCLE",
        lat: 41.41306,
        lon: -81.86,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 763,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KCLX",
        lat: 32.65556,
        lon: -81.04222,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 115,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KCRI",
        lat: 35.2383,
        lon: -97.4602,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1201,
            tower_ft: 114,
        }),
    },
    RadarSite {
        name: "KCRP",
        lat: 27.78389,
        lon: -97.51083,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 45,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KCXX",
        lat: 44.51111,
        lon: -73.16639,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 317,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KCYS",
        lat: 41.15194,
        lon: -104.80611,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 6128,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KDAX",
        lat: 38.50111,
        lon: -121.67667,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 30,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KDDC",
        lat: 37.76083,
        lon: -99.96833,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 2590,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KDFX",
        lat: 29.2725,
        lon: -100.28028,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1131,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KDGX",
        lat: 32.28,
        lon: -89.98444,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 495,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KDIX",
        lat: 39.94694,
        lon: -74.41111,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 149,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KDLH",
        lat: 46.83694,
        lon: -92.20972,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1428,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KDMX",
        lat: 41.73111,
        lon: -93.72278,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 981,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KDOX",
        lat: 38.82556,
        lon: -75.44,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 50,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KDTX",
        lat: 42.69972,
        lon: -83.47167,
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
        lat: 32.53833,
        lon: -99.25417,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1517,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KEAX",
        lat: 38.81028,
        lon: -94.26417,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 995,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KEMX",
        lat: 31.89361,
        lon: -110.63028,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 5202,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KENX",
        lat: 42.58639,
        lon: -74.06444,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1854,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KEOX",
        lat: 31.46028,
        lon: -85.45944,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 472,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KEPZ",
        lat: 31.87306,
        lon: -106.6975,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 4104,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KESX",
        lat: 35.70111,
        lon: -114.89139,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 4867,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KEVX",
        lat: 30.56417,
        lon: -85.92139,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 140,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KEWX",
        lat: 29.70361,
        lon: -98.02806,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 669,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KEYX",
        lat: 35.09778,
        lon: -117.56,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 2776,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KFCX",
        lat: 37.02417,
        lon: -80.27417,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 2868,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KFDR",
        lat: 34.36222,
        lon: -98.97611,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1267,
            tower_ft: 46,
        }),
    },
    RadarSite {
        name: "KFDX",
        lat: 34.63528,
        lon: -103.62944,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 4650,
            tower_ft: 46,
        }),
    },
    RadarSite {
        name: "KFFC",
        lat: 33.36333,
        lon: -84.56583,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 858,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KFSD",
        lat: 43.58778,
        lon: -96.72889,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1430,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KFSX",
        lat: 34.57444,
        lon: -111.19833,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 7418,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KFTG",
        lat: 39.78667,
        lon: -104.54528,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 5497,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KFWS",
        lat: 32.57278,
        lon: -97.30278,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 696,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KGGW",
        lat: 48.20639,
        lon: -106.62417,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 2303,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KGJX",
        lat: 39.06222,
        lon: -108.21306,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 10036,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KGLD",
        lat: 39.36694,
        lon: -101.7,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 3651,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KGRB",
        lat: 44.49833,
        lon: -88.11111,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 709,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KGRK",
        lat: 30.72167,
        lon: -97.38278,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 538,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KGRR",
        lat: 42.89389,
        lon: -85.54472,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 778,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KGSP",
        lat: 34.88306,
        lon: -82.22028,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 955,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KGWX",
        lat: 33.89667,
        lon: -88.32889,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 509,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KGYX",
        lat: 43.89139,
        lon: -70.25694,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 409,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KHDC",
        lat: 30.519,
        lon: -90.407,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 43,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KHDX",
        lat: 33.07639,
        lon: -106.12222,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 4222,
            tower_ft: 46,
        }),
    },
    RadarSite {
        name: "KHGX",
        lat: 29.47194,
        lon: -95.07889,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 18,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KHNX",
        lat: 36.31417,
        lon: -119.63111,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 243,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KHPX",
        lat: 36.73667,
        lon: -87.285,
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
        lat: 37.65444,
        lon: -97.4425,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1335,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KICX",
        lat: 37.59083,
        lon: -112.86222,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 10643,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KILN",
        lat: 39.42028,
        lon: -83.82167,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1056,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KILX",
        lat: 40.15056,
        lon: -89.33667,
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
        lat: 36.175,
        lon: -95.56444,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 668,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KIWA",
        lat: 33.28917,
        lon: -111.66917,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1362,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KIWX",
        lat: 41.40861,
        lon: -85.7,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 960,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KJAX",
        lat: 30.48444,
        lon: -81.70194,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 62,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KJGX",
        lat: 32.675,
        lon: -83.35111,
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
        lat: 33.65417,
        lon: -101.81361,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 3297,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KLCH",
        lat: 30.125,
        lon: -93.21583,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 56,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KLGX",
        lat: 47.1158,
        lon: -124.1069,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 252,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KLIX",
        lat: 30.33667,
        lon: -89.82528,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 66,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KLNX",
        lat: 41.95778,
        lon: -100.57583,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 3015,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KLOT",
        lat: 41.60444,
        lon: -88.08472,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 663,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KLRX",
        lat: 40.73972,
        lon: -116.80278,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 6781,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KLSX",
        lat: 38.69889,
        lon: -90.68278,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 608,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KLTX",
        lat: 33.98917,
        lon: -78.42917,
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
        lat: 38.97628,
        lon: -77.48751,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 292,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KLZK",
        lat: 34.83639,
        lon: -92.26194,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 568,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KMAF",
        lat: 31.94333,
        lon: -102.18889,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 2897,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KMAX",
        lat: 42.08111,
        lon: -122.71611,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 7513,
            tower_ft: 46,
        }),
    },
    RadarSite {
        name: "KMBX",
        lat: 48.3925,
        lon: -100.86444,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1493,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KMHX",
        lat: 34.77583,
        lon: -76.87639,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 31,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KMKX",
        lat: 42.96778,
        lon: -88.55056,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 958,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KMLB",
        lat: 28.11306,
        lon: -80.65444,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 36,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KMOB",
        lat: 30.67944,
        lon: -88.23972,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 208,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KMPX",
        lat: 44.84889,
        lon: -93.56528,
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
        lat: 36.16833,
        lon: -83.40194,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1337,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KMSX",
        lat: 47.04111,
        lon: -113.98611,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 7930,
            tower_ft: 46,
        }),
    },
    RadarSite {
        name: "KMTX",
        lat: 41.26278,
        lon: -112.44694,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 6480,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KMUX",
        lat: 37.15528,
        lon: -121.8975,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 3469,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KMVX",
        lat: 47.52806,
        lon: -97.325,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 986,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KMXX",
        lat: 32.53667,
        lon: -85.78972,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 446,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KNKX",
        lat: 32.91889,
        lon: -117.04194,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 955,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KNQA",
        lat: 35.34472,
        lon: -89.87333,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 338,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KOAX",
        lat: 41.32028,
        lon: -96.36639,
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
        lat: 40.86556,
        lon: -72.86444,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 85,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KOTX",
        lat: 47.68056,
        lon: -117.62583,
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
        lat: 40.53167,
        lon: -80.21833,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1185,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KPDT",
        lat: 45.69056,
        lon: -118.85278,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1515,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KPOE",
        lat: 31.15528,
        lon: -92.97583,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 408,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KPUX",
        lat: 38.45944,
        lon: -104.18139,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 5299,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KRAX",
        lat: 35.66528,
        lon: -78.49,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 348,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KRGX",
        lat: 39.75417,
        lon: -119.46111,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 8299,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KRIW",
        lat: 43.06611,
        lon: -108.47667,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 5568,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KRLX",
        lat: 38.31194,
        lon: -81.72389,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1099,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KRTX",
        lat: 45.715,
        lon: -122.96417,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1614,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KSFX",
        lat: 43.10583,
        lon: -112.68528,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 4474,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KSGF",
        lat: 37.23528,
        lon: -93.40028,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1278,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KSHV",
        lat: 32.45056,
        lon: -93.84111,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 273,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KSJT",
        lat: 31.37111,
        lon: -100.49222,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1890,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KSOX",
        lat: 33.81778,
        lon: -117.635,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 3041,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KSRX",
        lat: 35.29056,
        lon: -94.36167,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 656,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KTBW",
        lat: 27.70528,
        lon: -82.40194,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 41,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KTFX",
        lat: 47.45972,
        lon: -111.38444,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 3740,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KTLH",
        lat: 30.3975,
        lon: -84.32889,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 63,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KTLX",
        lat: 35.33306,
        lon: -97.2775,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1213,
            tower_ft: 62,
        }),
    },
    RadarSite {
        name: "KTWX",
        lat: 38.99694,
        lon: -96.2325,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1367,
            tower_ft: 46,
        }),
    },
    RadarSite {
        name: "KTYX",
        lat: 43.75583,
        lon: -75.68,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1846,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KUDX",
        lat: 44.125,
        lon: -102.82944,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 3081,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KUEX",
        lat: 40.32083,
        lon: -98.44167,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1976,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KVAX",
        lat: 30.89,
        lon: -83.00194,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 217,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KVBX",
        lat: 34.83806,
        lon: -120.39583,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1257,
            tower_ft: 95,
        }),
    },
    RadarSite {
        name: "KVNX",
        lat: 36.74083,
        lon: -98.1275,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 1210,
            tower_ft: 46,
        }),
    },
    RadarSite {
        name: "KVTX",
        lat: 34.41167,
        lon: -119.17861,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 2726,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "KVWX",
        lat: 38.2600,
        lon: -87.7247,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 512,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "KYUX",
        lat: 32.49528,
        lon: -114.65583,
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
        lat: 60.79278,
        lon: -161.87417,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 162,
            tower_ft: 30,
        }),
    },
    RadarSite {
        name: "PACG",
        lat: 56.85278,
        lon: -135.52917,
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
        lat: 60.725914,
        lon: -151.35146,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 243,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "PAIH",
        lat: 59.46194,
        lon: -146.30111,
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
        lat: 65.03556,
        lon: -147.49917,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 2593,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "PGUA",
        lat: 13.45444,
        lon: 144.80833,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 272,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "PHKI",
        lat: 21.89417,
        lon: -159.55222,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 226,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "PHKM",
        lat: 20.12556,
        lon: -155.77778,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 3852,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "PHMO",
        lat: 21.13278,
        lon: -157.18,
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
        lat: 36.95972,
        lon: 127.01833,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 52,
            tower_ft: 79,
        }),
    },
    RadarSite {
        name: "RODN",
        lat: 26.30194,
        lon: 127.90972,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 299,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "TJUA",
        lat: 18.1175,
        lon: -66.07861,
        heights: Some(SiteHeights::BaseAndTower {
            base_ft: 2844,
            tower_ft: 112,
        }),
    },
    RadarSite {
        name: "TJFK",
        lat: 40.5668,
        lon: -73.8874,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 112 }),
    },
    RadarSite {
        name: "TADW",
        lat: 38.6704,
        lon: -76.8446,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 346 }),
    },
    RadarSite {
        name: "TATL",
        lat: 33.6433,
        lon: -84.2524,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 1075 }),
    },
    RadarSite {
        name: "TBNA",
        lat: 35.9767,
        lon: -86.6618,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 817 }),
    },
    RadarSite {
        name: "TBOS",
        lat: 42.1515,
        lon: -70.9302,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 264 }),
    },
    RadarSite {
        name: "TBWI",
        lat: 39.0870,
        lon: -76.6276,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 297 }),
    },
    RadarSite {
        name: "TCLT",
        lat: 35.3269,
        lon: -80.8772,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 871 }),
    },
    RadarSite {
        name: "TCMH",
        lat: 39.9878,
        lon: -82.71,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 1148 }),
    },
    RadarSite {
        name: "TCVG",
        lat: 38.8799,
        lon: -84.5737,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 1053 }),
    },
    RadarSite {
        name: "TDAL",
        lat: 32.9076,
        lon: -96.9568,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 622 }),
    },
    RadarSite {
        name: "TDAY",
        lat: 39.9875,
        lon: -84.1102,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 1019 }),
    },
    RadarSite {
        name: "TDCA",
        lat: 38.7474,
        lon: -76.9509,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 345 }),
    },
    RadarSite {
        name: "TDEN",
        lat: 39.7256,
        lon: -104.5431,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 5701 }),
    },
    RadarSite {
        name: "TDFW",
        lat: 33.0396,
        lon: -96.8974,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 585 }),
    },
    RadarSite {
        name: "TDTW",
        lat: 42.0710,
        lon: -83.4704,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 772 }),
    },
    RadarSite {
        name: "TEWR",
        lat: 40.5880,
        lon: -74.2503,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 136 }),
    },
    RadarSite {
        name: "TFLL",
        lat: 26.1263,
        lon: -80.3478,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 120 }),
    },
    RadarSite {
        name: "THOU",
        lat: 29.5328,
        lon: -95.2444,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 117 }),
    },
    RadarSite {
        name: "TIAD",
        lat: 39.0675,
        lon: -77.5012,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 473 }),
    },
    RadarSite {
        name: "TIAH",
        lat: 30.0297,
        lon: -95.5708,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 253 }),
    },
    RadarSite {
        name: "TICH",
        lat: 37.4069,
        lon: -97.4764,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 1351 }),
    },
    RadarSite {
        name: "TIDS",
        lat: 39.5978,
        lon: -86.4085,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 847 }),
    },
    RadarSite {
        name: "TLAS",
        lat: 36.1292,
        lon: -115.0147,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 2058 }),
    },
    RadarSite {
        name: "TLVE",
        lat: 41.2805,
        lon: -81.9659,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 931 }),
    },
    RadarSite {
        name: "TMCI",
        lat: 39.4488,
        lon: -94.7396,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 1090 }),
    },
    RadarSite {
        name: "TMCO",
        lat: 28.2584,
        lon: -81.3133,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 169 }),
    },
    RadarSite {
        name: "TMDW",
        lat: 41.69,
        lon: -87.8034,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 763 }),
    },
    RadarSite {
        name: "TMEM",
        lat: 34.8867,
        lon: -90.0007,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 483 }),
    },
    RadarSite {
        name: "TMIA",
        lat: 25.7555,
        lon: -80.4932,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 125 }),
    },
    RadarSite {
        name: "TMKE",
        lat: 42.7619,
        lon: -87.9994,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 933 }),
    },
    RadarSite {
        name: "TMSP",
        lat: 44.8197,
        lon: -92.9392,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 1121 }),
    },
    RadarSite {
        name: "TMSY",
        lat: 29.9385,
        lon: -90.3811,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 99 }),
    },
    RadarSite {
        name: "TOKC",
        lat: 35.2474,
        lon: -97.5395,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 1308 }),
    },
    RadarSite {
        name: "TORD",
        lat: 41.7712,
        lon: -87.8363,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 744 }),
    },
    RadarSite {
        name: "TPBI",
        lat: 26.6572,
        lon: -80.2586,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 133 }),
    },
    RadarSite {
        name: "TPHL",
        lat: 39.9084,
        lon: -75.0426,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 153 }),
    },
    RadarSite {
        name: "TPHX",
        lat: 33.3678,
        lon: -112.1580,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 1089 }),
    },
    RadarSite {
        name: "TPIT",
        lat: 40.4641,
        lon: -80.4697,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 1386 }),
    },
    RadarSite {
        name: "TRDU",
        lat: 35.9898,
        lon: -78.6787,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 515 }),
    },
    RadarSite {
        name: "TSDF",
        lat: 38.0109,
        lon: -85.5995,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 731 }),
    },
    RadarSite {
        name: "TSJU",
        lat: 18.4313,
        lon: -66.1722,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 157 }),
    },
    RadarSite {
        name: "TSLC",
        lat: 40.9341,
        lon: -111.9214,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 4295 }),
    },
    RadarSite {
        name: "TSTL",
        lat: 38.7668,
        lon: -90.4698,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 647 }),
    },
    RadarSite {
        name: "TTPA",
        lat: 27.8196,
        lon: -82.5179,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 93 }),
    },
    RadarSite {
        name: "TTUL",
        lat: 36.0236,
        lon: -95.8175,
        heights: Some(SiteHeights::FeedhornOnly { feedhorn_ft: 823 }),
    },
];

pub fn get_radar_site(site: &str) -> Option<&'static RadarSite> {
    use std::collections::HashMap;
    use std::sync::LazyLock;

    static SITE_MAP: LazyLock<HashMap<&'static str, &'static RadarSite>> = LazyLock::new(|| {
        let mut map = HashMap::with_capacity(RADARS.len());
        for radar in RADARS.iter() {
            map.insert(radar.name, radar);
        }
        map
    });

    SITE_MAP.get(site).copied()
}

/// Great-circle distance between two coordinates, in kilometres.
///
/// Haversine rather than the cheaper equirectangular approximation: the caller
/// compares sites up to a continent apart (a fix in Hawaii against a table that
/// is mostly CONUS), and the flat approximation's error grows with both
/// separation and latitude — exactly the regime the comparison runs in.
pub fn distance_km(lat_a: f64, lon_a: f64, lat_b: f64, lon_b: f64) -> f64 {
    /// Mean radius, the value the WGS-84 sphere approximation uses.
    const EARTH_RADIUS_KM: f64 = 6371.0088;

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
    /// reflectivity to show through that path. [`RADARS`] lists both because the
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
    /// Only automatic selection consults this. The site stays in [`RADARS`], the
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
/// happens to sit first in [`RADARS`], which reads as a deliberate choice.
///
/// No distance cap: a caller in Europe gets the nearest NEXRAD and a very large
/// number, and it is the caller's business whether that is useful. Callers that
/// care should test the returned distance rather than expect `None`.
pub fn nearest_radar_site(lat: f64, lon: f64) -> Option<(&'static RadarSite, f64)> {
    nearest_site_where(lat, lon, |_| true)
}

/// The closest site an automatic pick should open on, with its distance in km.
///
/// This is the one startup site selection wants: the nearest operational
/// WSR-88D. Downtown Oklahoma City illustrates both filters at once — the
/// literal nearest site is the TDWR `TOKC`, and the nearest WSR-88D is the ROC
/// test bed `KCRI`. Neither reliably shows a viewer reflectivity, and the site
/// a person there actually wants is the third one out, `KTLX`.
pub fn nearest_wsr88d_site(lat: f64, lon: f64) -> Option<(&'static RadarSite, f64)> {
    nearest_site_where(lat, lon, |site| site.is_wsr88d() && site.is_operational())
}

fn nearest_site_where(
    lat: f64,
    lon: f64,
    accept: impl Fn(&RadarSite) -> bool,
) -> Option<(&'static RadarSite, f64)> {
    if !lat.is_finite() || !lon.is_finite() {
        return None;
    }
    RADARS
        .iter()
        .filter(|site| accept(site))
        .map(|site| (site, distance_km(lat, lon, site.lat, site.lon)))
        // `total_cmp`, not `partial_cmp().unwrap()`: the distances are finite
        // given a finite input, but the unwrap would be a panic path in a
        // startup routine to save nothing.
        .min_by(|(_, a), (_, b)| a.total_cmp(b))
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

        let (nearest_88d, _) = nearest_site_where(35.4676, -97.5164, RadarSite::is_wsr88d)
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
        let tdwrs = RADARS.iter().filter(|s| s.is_tdwr()).count();
        assert_eq!(tdwrs, 45);
        assert_eq!(RADARS.len() - tdwrs, 162);
    }

    /// A NaN must not silently degrade to "the first entry in the table".
    #[test]
    fn a_non_finite_coordinate_has_no_nearest_site() {
        assert!(nearest_wsr88d_site(f64::NAN, -97.0).is_none());
        assert!(nearest_wsr88d_site(35.0, f64::INFINITY).is_none());
    }

    /// The rows this table corrected against their own Level II volume, as
    /// (site, the height it recorded before, the height its volume reports).
    ///
    /// Feet, on [`Datum::SiteBase`]. Measured by `site_elev_probe` on the
    /// `campaign-harness` branch over one volume per site.
    const CORRECTED_AGAINST_A_VOLUME: [(&str, i32, i32); 49] = [
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
        let missing: Vec<&str> = RADARS
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
        let missing: Vec<&str> = RADARS
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
        let no_base: Vec<&str> = RADARS
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
        let mut gaps: Vec<i32> = RADARS
            .iter()
            .filter_map(|s| Some(s.height_ft(Datum::Feedhorn)? - s.height_ft(Datum::SiteBase)?))
            .collect();
        gaps.sort_unstable();
        assert_eq!(gaps.len(), 161, "every row that records both");
        assert_eq!(*gaps.first().expect("non-empty"), 30, "the shortest tower");
        assert_eq!(*gaps.last().expect("non-empty"), 114, "the tallest tower");
    }

    /// The 49 rows whose height was corrected against their own volume,
    /// pinned by value.
    ///
    /// The table used to hold one number per row and a note saying six had
    /// been checked. Checking all 207 against one archive volume each found
    /// these disagreeing with the height their own RDA reports by more than
    /// the whole-metre rounding of the field — from 63 ft high to 81 ft low.
    /// KMSX, the one the old note called unexplained, is item 31 of 49 rather
    /// than a singleton.
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
        assert_eq!(CORRECTED_AGAINST_A_VOLUME.len(), 49);
    }

    /// Every site must be reachable as its own nearest neighbour, which catches
    /// a transposed lat/lon in any single table row.
    #[test]
    fn every_site_is_its_own_nearest_neighbour() {
        for site in RADARS.iter() {
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
