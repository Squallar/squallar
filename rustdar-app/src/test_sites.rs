//! The radars this crate's tests run against.

use rustdar_radar::site_position::SitePosition;
use rustdar_radar::sites::SiteFix;

/// `(ICAO, latitude, longitude, site_height_m, tower_height_m)`.
const SITES: [(&str, i32, i32, i32, i32); 17] = [
    ("KTLX", 35_333_060, -97_277_500, 370, 19),
    ("TOKC", 35_276_000, -97_510_000, 386, 386),
    // Pittsburgh's WSR-88D and the TDWR that shares its metro.
    ("KPBZ", 40_531_670, -80_218_060, 361, 30),
    ("TPIT", 40_501_000, -80_486_000, 366, 366),
    ("KCRI", 35_238_330, -97_460_280, 383, 19),
    ("KINX", 36_175_000, -95_565_000, 204, 30),
    ("KDDC", 37_761_000, -99_969_000, 789, 24),
    ("KABR", 45_455_830, -98_413_330, 397, 24),
    ("KMPX", 44_849_000, -93_566_000, 288, 30),
    ("KLOT", 41_604_440, -88_084_720, 202, 30),
    ("KDLH", 46_836_940, -92_209_720, 435, 30),
    ("KFTG", 39_786_670, -104_545_830, 1675, 30),
    ("KSOX", 33_817_780, -117_635_830, 923, 24),
    ("KDIX", 39_946_940, -74_411_110, 45, 34),
    ("KATX", 48_194_440, -122_495_830, 151, 30),
    ("PHMO", 21_132_780, -157_180_280, 415, 24),
    ("TJUA", 18_115_670, -66_078_160, 867, 34),
];

/// Place the fixture network, once per process.
pub(crate) fn install() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        rustdar_radar::sites::resolve(SITES.map(
            |(name, lat_udeg, lon_udeg, site_height_m, tower_height_m)| {
                (
                    name,
                    SiteFix::Learned(SitePosition {
                        lat_udeg,
                        lon_udeg,
                        site_height_m,
                        tower_height_m,
                    }),
                )
            },
        ));
    });
}
