//! Which datum `api.weather.gov/radar/stations` publishes, settled against the
//! archive.
//!
//! # Why this file exists
//!
//! [`SiteFix::Network`](crate::sites::SiteFix::Network) asserted for months
//! that the published elevation was the **feedhorn**, and it is the **ground
//! under the tower**. The claim was not unexamined — it had been checked, and
//! the check could not have caught it. It compared the station record against
//! the compiled-in seed table, and the seed and the record were two spellings
//! of one belief about the network, so they agreed and went on agreeing. When
//! the seed was deleted the comparison went with it and the sentence stayed,
//! by then unfalsifiable by anything in the tree.
//!
//! # Why this one cannot fail the same way
//!
//! **Neither input is ours, and the two come from different organisations by
//! different routes.**
//!
//! * `testdata/*_volume_data_block.msg31` are unmodified bytes lifted out of
//!   five NOAA Level II archive volumes — one whole Message 31, header and
//!   moments intact, exactly as the record held it. A Volume Data Block states
//!   `site_height` and `tower_height` as two separate fields, which is what
//!   makes the datum question answerable at all rather than arguable, and they
//!   are read here by [`nexrad_decode`] on the same path `crate::scan` uses.
//! * `testdata/nws_radar_stations.json` carries those five stations verbatim
//!   from the `api.weather.gov` response, and is read by the same
//!   [`parse_stations`] production calls.
//!
//! So nothing this workspace computes appears on either side of the
//! comparison. If [`parse_stations`] or
//! [`SiteFix::applied`](crate::sites::SiteFix) put the elevation on the wrong
//! datum, both assertions below fail — there is no shared premise for them to
//! agree through.
//!
//! # Why five sites
//!
//! One site proves an equality; five prove it is not an accident. They were
//! chosen to be the five standard WSR-88D tower builds — 14, 19, 24, 29 and
//! 34 m — because the whole finding is that the error *is* the tower, so a set
//! that held the tower constant could not distinguish "the record is the
//! ground" from "the record is 25 m below whatever this radar reports". They
//! are also five regions (Pacific Northwest, Upper Midwest, Southern Plains,
//! Gulf Coast, Northeast) with ground from 14 m to 2290 m, which is what stops
//! one region's survey convention standing in for all of them.
//!
//! `KCRP`'s volume is from 2017 and the other four from 2024, so the older
//! Volume Data Block layout is exercised alongside the current one.
//!
//! # What is *not* pinned here
//!
//! No expected height appears in this file. The volume's figures are read out
//! of the volume and the record's out of the record; only the *relationship*
//! between them is asserted. A transcription cannot drift because there is no
//! transcription.

use super::parse_stations;
use crate::site_position::SitePosition;
use crate::sites::{Datum, SiteFix, build_table};

/// The five sites, and nothing about them. Every number this file compares is
/// read from a fixture at run time.
const SITES: [&str; 5] = ["KMAX", "KMKX", "KINX", "KCRP", "KCBW"];

/// The whole-metre rounding both sources apply, in metres. A Volume Data Block
/// truncates its two height fields to whole metres and [`parse_stations`]
/// rounds the record's decimal to the same, so two figures describing one
/// ground can legitimately differ by this and no more.
const ROUNDING_M: i32 = 1;

/// The range of real WSR-88D tower heights, metres. 9.75 m is the shortest
/// build measured anywhere in the network (`PABC`, `PAEC`) and 34.76 m the
/// tallest (`KLGX`); the bound is widened to whole metres either side.
const TOWER_RANGE_M: std::ops::RangeInclusive<i32> = 9..=35;

/// What one site's own volume says about itself.
fn from_volume(site: &str) -> SitePosition {
    let path = format!("testdata/{site}_volume_data_block.msg31");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let messages = nexrad_decode::messages::decode_messages(&bytes)
        .unwrap_or_else(|e| panic!("{site}: the archive's own bytes must decode: {e:?}"));

    let block = messages
        .into_iter()
        .find_map(|message| match message.into_contents() {
            nexrad_decode::messages::MessageContents::DigitalRadarData(m) => {
                m.volume_data_block().map(|v| {
                    nexrad_model::meta::Site::new(
                        [0; 4],
                        v.inner().latitude_raw(),
                        v.inner().longitude_raw(),
                        v.inner().site_height_raw(),
                        v.inner().tower_height_raw(),
                    )
                })
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("{site}: the fixture is the radial that carries the VOL block"));

    SitePosition::from_volume(&block)
        .unwrap_or_else(|| panic!("{site}: a real volume states a position on the planet"))
}

/// What the published station record says about the same site.
fn from_station_record(site: &str) -> super::CataloguePosition {
    let body = std::fs::read_to_string("testdata/nws_radar_stations.json")
        .expect("the station fixture is in the tree");
    *parse_stations(&body)
        .get(site)
        .unwrap_or_else(|| panic!("{site} is in the station fixture and is placeable"))
}

/// The published elevation is the ground the tower stands on, not the feedhorn.
///
/// The two clauses are equally load-bearing. That the record matches the base
/// is the finding; that it *misses* the feedhorn by a plausible tower is what
/// rules out the two sources simply being imprecise about one number.
#[test]
fn the_station_record_elevation_is_the_ground_its_volume_reports() {
    let mut worst_base_gap = 0;

    for site in SITES {
        let volume = from_volume(site);
        let record = from_station_record(site);

        let base_gap = record.elevation_m - volume.site_height_m;
        assert!(
            base_gap.abs() <= ROUNDING_M,
            "{site}: the record's {} m and the volume's base of {} m differ by \
             {base_gap} m, which is more than the whole-metre rounding both \
             apply — they are not the same measurement",
            record.elevation_m,
            volume.site_height_m,
        );
        worst_base_gap = worst_base_gap.max(base_gap.abs());

        let feedhorn_m = volume.site_height_m + volume.tower_height_m;
        let feedhorn_gap = feedhorn_m - record.elevation_m;
        assert!(
            TOWER_RANGE_M.contains(&feedhorn_gap),
            "{site}: the record sits {feedhorn_gap} m below the volume's \
             feedhorn of {feedhorn_m} m. One tower is what that gap has to be \
             for this finding to be about a datum at all",
        );
        assert_eq!(
            feedhorn_gap, volume.tower_height_m,
            "{site}: and the gap is exactly this radar's own tower, not an \
             offset that happens to be tower-sized",
        );
    }

    assert!(
        worst_base_gap <= ROUNDING_M,
        "worst base disagreement across {} sites was {worst_base_gap} m",
        SITES.len(),
    );
}

/// The rows the two sources build agree about the ground, and differ about the
/// feedhorn by exactly the tower one of them had to assume.
///
/// This is the same fact one level up, through [`build_table`] and
/// [`RadarSite::height_ft`](crate::sites::RadarSite::height_ft), so it fails on
/// a change to the shape a fix produces and not only on a change to the parse.
/// Switching the `Network` arm back to
/// [`SiteHeights::FeedhornOnly`](crate::sites::SiteHeights::FeedhornOnly)
/// breaks the first assertion outright: a feedhorn-only row answers `None` on
/// [`Datum::SiteBase`].
#[test]
fn a_fetched_row_states_the_ground_a_learned_row_measures() {
    for site in SITES {
        let volume = from_volume(site);
        let record = from_station_record(site);

        let learned = build_table([(site, SiteFix::Learned(volume))]);
        let learned = learned.get(site).expect("a learned row");
        let fetched = build_table([(
            site,
            SiteFix::Network {
                lat_udeg: record.lat_udeg,
                lon_udeg: record.lon_udeg,
                elevation_m: record.elevation_m,
            },
        )]);
        let fetched = fetched.get(site).expect("a fetched row");

        let (measured_base, stated_base) = (
            learned.height_ft(Datum::SiteBase).expect("a volume base"),
            fetched
                .height_ft(Datum::SiteBase)
                .expect("a record's ground"),
        );
        assert!(
            (measured_base - stated_base).abs() <= 4,
            "{site}: the volume's ground is {measured_base} ft and the \
             record's {stated_base} ft; a metre of rounding is 3.3 ft and \
             anything past that is a datum difference",
        );

        // The feedhorn is measured on one row and estimated on the other, so
        // they are allowed to differ — by the difference between this radar's
        // real tower and the nominal one, and by nothing else.
        let measured_feedhorn = learned
            .height_ft(Datum::Feedhorn)
            .expect("a volume feedhorn");
        let estimated_feedhorn = fetched.height_ft(Datum::Feedhorn).expect("an estimate");
        let error_ft = estimated_feedhorn - measured_feedhorn;
        assert!(
            error_ft.abs() <= 50,
            "{site}: the estimated feedhorn is {error_ft} ft off the measured \
             one. The widest a real tower sits from the nominal is 15 m, so \
             anything past 50 ft means the nominal has moved or the datum has",
        );
        // The direction is the point of the whole change: it used to be low at
        // every site without exception, and now it is centred.
        assert_ne!(
            estimated_feedhorn, stated_base,
            "{site}: the estimated feedhorn is the bare ground again, which is \
             the defect this variant exists to remove",
        );
    }
}
