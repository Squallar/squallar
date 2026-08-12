//! The radars this crate's tests run against.
//!
//! # Why a test has to place them at all
//!
//! `rustdar-radar` carries no list of the network — see
//! [`SiteTable`](rustdar_radar::sites::SiteTable). A process learns which
//! radars exist from a volume it decoded or from the catalogue it fetched, and
//! a headless `App` over a `MemoryConfigStore` with no network does neither. So
//! without this every test here runs against an empty table: `get_radar_site`
//! answers `None`, a pane names a site nothing can place, a timezone resolves
//! to no radar and a cross-section is anchored at sea level.
//!
//! This stands in for what a returning user's install has already cached, and
//! it is what the application itself resolves in `App::with_instance` before
//! its first frame.
//!
//! # Why it is one list and not one per test module
//!
//! [`rustdar_radar::sites::resolve`] sets a **process-wide** table, and every
//! test in this crate's library binary shares it. Two modules installing two
//! different fixtures would therefore install their union, and an assertion
//! about which radar is nearest a coordinate would depend on which module's
//! rows happened to be there — which is a test whose outcome is a scheduling
//! accident.
//!
//! It caught one: with `KOUN` in one fixture and not the other, the nearest
//! operational WSR-88D to downtown Oklahoma City was `KOUN` or `KTLX`
//! depending on test order.
//!
//! # What is deliberately absent
//!
//! `KMBX`, which `a_run_with_no_config_store_still_applies_the_volumes_own_position`
//! places itself so that no sibling can move it.

use rustdar_radar::site_position::SitePosition;
use rustdar_radar::sites::SiteFix;

/// `(ICAO, latitude, longitude, site_height_m, tower_height_m)`.
///
/// Real sites at their own positions to 5 dp, in the whole metres a Volume
/// Data Block reports, and spread across the country so that a nearest-search
/// has something to be wrong about — a fixture with one radar in it answers
/// every question with that radar.
///
/// The three around Oklahoma City are the case that exercises both filters
/// `nearest_wsr88d_site` applies: `TOKC` is a TDWR with no Level II data and
/// is nearest, `KCRI` is the ROC test bed and is nearer than `KTLX`, and
/// `KTLX` is the answer an automatic pick should reach.
///
/// `TOKC` states one height twice, which is how a TDWR reports itself.
const SITES: [(&str, i32, i32, i32, i32); 15] = [
    ("KTLX", 35_333_060, -97_277_500, 370, 19),
    ("TOKC", 35_276_000, -97_510_000, 386, 386),
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
///
/// Idempotent anyway — `resolve` builds nothing when the fixes reproduce the
/// rows already there — but tests run in parallel and the [`Once`] is the
/// cheaper way to say so.
///
/// Tests that must see a genuinely **empty** table do not call this and must
/// not read the process table at all: they go through
/// [`build_table`](rustdar_radar::sites::build_table), which never consults
/// what this resolved.
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
