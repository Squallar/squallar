//! Which radars exist, and where they are — fetched, not compiled in.
//!
//! The binary used to carry `sites::SEED`, a snapshot of the network on the
//! day it was built, and a binary that can only ever know those 207 rows rots:
//! a radar commissioned afterwards is one the app cannot name, cannot draw and
//! cannot centre on. The seed is deleted and this module is where the answer
//! comes from instead.
//!
//! # It is a union of two sources, and neither one alone is correct
//!
//! Established by measurement, not by preference:
//!
//! * **Which identifiers exist** — the root listing of the
//!   [`level2_chunks_bucket`](crate::sources::DataSources::level2_chunks_bucket).
//!   One delimited request, ~11 KB, ~0.29 s, and it answers with every site the
//!   live feed carries.
//! * **Where they are** — `api.weather.gov/radar/stations`. 208 stations (159
//!   WSR-88D, 45 TDWR, 4 profiler) with identifier, position and elevation;
//!   510 KB raw and 22 KB gzipped. Its **positions** agree with the archive:
//!   against each site's own Volume Data Block over 54 corpus sites the median
//!   separation is 1.7 m and the largest is 73.4 m, a third of one 250 m gate.
//!   Its **elevation** is the ground under the tower and not the feedhorn, and
//!   that is worth stating here because the two were confused for a while —
//!   see [`SiteFix::Network`] and [`crate::sites::SiteHeights::GroundOnly`].
//!
//! Three live counterexamples are why it has to be both:
//!
//! ```text
//! TPBI   archive data through 2026/07/15   404 from the NWS API (renamed TDJT)
//! KCRI   archive data through 2026         404 from the NWS API
//! TDJT   listed by the NWS API             no archive data at all
//! ```
//!
//! So **the bucket decides existence and the NWS decides position**, and
//! [`SiteCatalogue::union`] is that sentence as code. An identifier the NWS
//! lists and the bucket does not is not a radar this application can show
//! anything for, and must not become selectable; an identifier the bucket lists
//! and the NWS does not is real, and is recorded here as a member with no
//! position of its own — see [`SiteCatalogue`].
//!
//! # No new origin
//!
//! Both hosts are already in [`crate::sources::DataSources`], which is what
//! keeps this off the four-file CORS ceremony every *new* origin triggers (the
//! service worker's never-cache list, Android's `network_security_config.xml`,
//! `pwa_assets.rs`, and the staging loop). `no_new_origin_is_needed_for_the_catalogue`
//! in [`crate::sources`] asserts that rather than leaving it as a claim.
//!
//! What that test does *not* pin — and nothing else in the tree does either —
//! is that the two hosts actually answer a cross-origin `fetch()`. That was
//! checked once by hand from a real origin, and no apparatus for it was
//! committed, so read it as an observation and not as evidence. It could not
//! be a regression test in any case: the `Access-Control-Allow-Origin` header
//! is served by hosts this project does not control, so no commit here can
//! break it and no commit here can fix it. What a commit here *can* break is
//! which host gets asked, and that is exactly what the test pins.
//!
//! Do not move either to AWS `noaa-nexrad-level2` (grants neither listing nor
//! public GET) or to the Google mirror (~3.5 weeks stale, `.tar`-bundled).
//!
//! # Fetching and parsing are separate
//!
//! Everything that decides anything — the union rule, the station parse, the
//! identifier filter — is pure and tested offline. The two network round-trips
//! are three lines each and are covered by `#[ignore]`d live tests, because CI
//! is hermetic.

use crate::sites::SiteFix;
use crate::sources::DataSources;
use std::collections::{BTreeMap, BTreeSet};

/// Connection-setup allowance for a bad link, not transfer time: the two
/// responses are ~11 KB and ~22 KB gzipped.
const CATALOGUE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// The delimiter that turns the bucket's ~11 million keys into ~200 directory
/// prefixes.
const DELIMITER: &str = "/";

/// The path on [`DataSources::nws_api_base`] that lists every station.
const STATIONS_PATH: &str = "/radar/stations";

/// Where the published station record puts one radar.
///
/// All three fields are integers, and for the reason
/// [`crate::site_position`] gives: `serde_json` writes a non-finite float as
/// `null`, `null` then fails to deserialize on the *next* load, and one bad
/// `f64` in a persisted record destroys the whole record a run after the bug.
/// The floats are converted once, in [`parse_stations`], and everything
/// downstream of it is integers.
///
/// # Why a position without an elevation is not a position
///
/// All three or none. The NWS gives position and elevation in the same record,
/// so a record missing one is a record this does not trust the rest of; and a
/// row that reaches the table with no elevation anchors a cross-section at sea
/// level, which reads as a measurement rather than as a gap — 292 ft of it at
/// `KLWX`. Every one of the 208 live stations carries all three.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CataloguePosition {
    /// Latitude, micro-degrees north.
    pub lat_udeg: i32,
    /// Longitude, micro-degrees east.
    pub lon_udeg: i32,
    /// The station record's one elevation, whole metres MSL — the **ground**
    /// under the tower for a WSR-88D, not the feedhorn.
    ///
    /// See [`SiteFix::Network`] for the measurement that settles the datum and
    /// for why this field carried the opposite name until 2026-08-13.
    ///
    /// `serde(alias)` for that old name so a cache written before the rename
    /// still loads. Without it every install would silently drop its cached
    /// catalogue on the first launch after the upgrade and run with no radars
    /// until a fetch came back — which on an offline launch is the whole site
    /// list. `a_cache_written_under_the_old_field_name_still_loads` pins it.
    #[serde(alias = "feedhorn_m")]
    pub elevation_m: i32,
}

/// Every radar the live archive carries, with a position where one is
/// published.
///
/// The membership is the bucket's and the positions are the NWS's — see the
/// module note. A member whose value is `None` is a radar that exists and that
/// this catalogue cannot place: it stays a member, because dropping it would
/// make the union an intersection and take `TPBI` and `KCRI` off the map, and
/// it contributes no [`SiteFix`], because there is nothing to say about where
/// it is. Such a radar keeps whatever the seed or a learned position gives it.
///
/// `BTreeMap` rather than `HashMap` so the serialized form is stable: two runs
/// that fetch the same catalogue write byte-identical JSON, which makes the
/// cached blob diffable by a human and comparable by a test.
///
/// `serde(transparent)` so the persisted form is the bare map and not a map
/// inside a wrapper object — the cache is data, not a versioned document.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct SiteCatalogue {
    sites: BTreeMap<String, Option<CataloguePosition>>,
}

impl SiteCatalogue {
    /// The union rule, and the only place it is expressed.
    ///
    /// Membership comes from `bucket_ids` alone. `positions` supplies
    /// coordinates for the members it knows and is otherwise ignored — an
    /// identifier only it has is not a radar the archive carries, and it never
    /// becomes a member.
    pub fn union<I>(bucket_ids: I, positions: &BTreeMap<String, CataloguePosition>) -> Self
    where
        I: IntoIterator<Item = String>,
    {
        Self {
            sites: bucket_ids
                .into_iter()
                .map(|id| {
                    let position = positions.get(&id).copied();
                    (id, position)
                })
                .collect(),
        }
    }

    /// Whether this catalogue carries `id` at all, placed or not.
    pub fn contains(&self, id: &str) -> bool {
        self.sites.contains_key(id)
    }

    /// Where this catalogue puts `id`, or `None` if it does not carry it or
    /// cannot place it.
    pub fn position(&self, id: &str) -> Option<CataloguePosition> {
        self.sites.get(id).copied().flatten()
    }

    /// How many radars this catalogue carries, placed or not.
    pub fn len(&self) -> usize {
        self.sites.len()
    }

    /// Whether it carries none — which is what a failed or never-run fetch
    /// leaves behind.
    pub fn is_empty(&self) -> bool {
        self.sites.is_empty()
    }

    /// Every identifier, placed or not, ascending.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.sites.keys().map(String::as_str)
    }

    /// What this catalogue has to say to [`crate::sites::resolve`].
    ///
    /// **Every** member, placed or not. A placed one becomes a
    /// [`SiteFix::Network`] carrying its position; an unplaced one becomes a
    /// [`SiteFix::Unplaced`], which asserts that the radar exists and nothing
    /// else.
    ///
    /// Emitting the unplaced ones is not a formality. `TPBI` and `KCRI` are
    /// the two identifiers the bucket lists and `api.weather.gov` will not
    /// place, and while a compiled-in table existed they were placed by it.
    /// With it deleted, dropping them here would remove them from the site
    /// list altogether — a radar with real archive data that the application
    /// simply refuses to mention. The alternative that must not be taken is a
    /// fix carrying a made-up position, which is a marker at Null Island drawn
    /// with the confidence of a real one; hence a variant with no numbers in
    /// it rather than a variant with zeros.
    pub fn fixes(&self) -> impl Iterator<Item = (&str, SiteFix)> {
        self.sites.iter().map(|(id, position)| {
            let fix = match position {
                Some(position) => SiteFix::Network {
                    lat_udeg: position.lat_udeg,
                    lon_udeg: position.lon_udeg,
                    elevation_m: position.elevation_m,
                },
                None => SiteFix::Unplaced,
            };
            (id.as_str(), fix)
        })
    }
}

/// Fetch both halves and union them, or `None` if either half failed.
///
/// **Both or neither.** A bucket listing without positions is a catalogue of
/// identifiers nobody can place, and writing it over a good cache would cost
/// every position the last run had; a station list without a bucket listing is
/// the NWS deciding existence, which is the one thing the counterexamples in
/// the module note say it must not do.
///
/// Every failure — DNS, TLS, status, a body that will not parse — returns
/// `None` and is logged at `debug`. The caller's response is the same for all
/// of them (keep the cache, run on seed plus whatever is already there), and a
/// warning per launch for an app that is simply offline is noise.
pub async fn fetch(sources: &DataSources) -> Option<SiteCatalogue> {
    let ids = match fetch_bucket_ids(sources).await {
        Ok(ids) => ids,
        Err(e) => {
            log::debug!("site catalogue: bucket listing failed, keeping the cache: {e}");
            return None;
        }
    };
    let positions = fetch_station_positions(sources).await?;
    let catalogue = SiteCatalogue::union(ids, &positions);
    log::info!(
        "site catalogue: {} radars listed, {} placed",
        catalogue.len(),
        catalogue.fixes().count(),
    );
    Some(catalogue)
}

/// Every identifier the real-time chunk bucket carries, from one delimited
/// listing of its root.
///
/// The bucket lays out `SITE/VOLUME/CHUNK`, so a delimited listing at the root
/// returns one `CommonPrefixes` entry per site and no keys — ~11 KB against the
/// ~11 million keys a flat listing would walk.
async fn fetch_bucket_ids(sources: &DataSources) -> crate::archive::Result<Vec<String>> {
    crate::tls::init();
    let client = crate::archive::shared_client();
    let prefixes = crate::archive::collect_common_prefixes(
        &sources.s3_bucket_url(&sources.level2_chunks_bucket),
        "",
        DELIMITER,
        |url| crate::archive::get_text(client, url),
    )
    .await?;
    Ok(parse_bucket_ids(&prefixes))
}

/// Pull the identifiers out of `SITE/` directory prefixes.
///
/// Split from the request so the filter is testable without a socket, and the
/// filter is the point: a bucket root can hold anything somebody uploaded, and
/// a stray `logs/` prefix would otherwise become a radar with a name, a marker
/// and a place in the site list.
///
/// Four characters, ASCII uppercase or digits — the shape of every ICAO in the
/// network and of nothing else that has ever appeared there. Deduplicated and
/// sorted, because two spellings of one radar would file two rows.
pub(crate) fn parse_bucket_ids(prefixes: &[String]) -> Vec<String> {
    let ids: BTreeSet<String> = prefixes
        .iter()
        .filter_map(|prefix| {
            let id = prefix.trim_end_matches('/');
            (id.len() == 4
                && id
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()))
            .then(|| id.to_owned())
        })
        .collect();
    ids.into_iter().collect()
}

/// One GET of `api.weather.gov/radar/stations`, parsed by [`parse_stations`].
///
/// Carries the ordinary `User-Agent`. `api.weather.gov` answers `OPTIONS` with
/// `200` and `Access-Control-Allow-Headers: API-Key, User-Agent` — it is one of
/// the preflight-*tolerant* origins recorded in [`crate::sources`], unlike IEM
/// and SPC — so the preflight round-trip succeeds and the request is readable
/// from the web build.
async fn fetch_station_positions(
    sources: &DataSources,
) -> Option<BTreeMap<String, CataloguePosition>> {
    crate::tls::init();
    let url = format!("{}{STATIONS_PATH}", sources.nws_api_base);
    let client = crate::tls::client(crate::tls::USER_AGENT, CATALOGUE_TIMEOUT)
        .build()
        .ok()?;
    let response = match client.get(&url).send().await {
        Ok(response) => response,
        Err(e) => {
            log::debug!("site catalogue: {url} unreachable, keeping the cache: {e}");
            return None;
        }
    };
    if !response.status().is_success() {
        log::debug!("site catalogue: HTTP {} from {url}", response.status());
        return None;
    }
    let body = response.text().await.ok()?;
    let stations = parse_stations(&body);
    if stations.is_empty() {
        log::debug!("site catalogue: {url} placed no stations");
        return None;
    }
    Some(stations)
}

/// The slice of a `/radar/stations` GeoJSON response this module reads.
#[derive(serde::Deserialize)]
struct StationCollection {
    features: Vec<Station>,
}

#[derive(serde::Deserialize)]
struct Station {
    geometry: Option<Geometry>,
    properties: StationProperties,
}

/// GeoJSON order is `[longitude, latitude]`, which is the opposite of every
/// other coordinate pair in this workspace and the one thing worth reading
/// twice here.
#[derive(serde::Deserialize)]
struct Geometry {
    coordinates: [f64; 2],
}

#[derive(serde::Deserialize)]
struct StationProperties {
    id: String,
    elevation: Option<Quantity>,
}

/// A JSON-LD quantity: `{"unitCode": "wmoUnit:m", "value": 386.7}`.
///
/// The unit is read and checked rather than assumed. The API is versioned and
/// has changed units on other fields before; a station quoting feet that was
/// taken for metres would put a radar 2.3 times too high, which is a plausible
/// number and therefore one nobody would notice.
#[derive(serde::Deserialize)]
struct Quantity {
    #[serde(rename = "unitCode")]
    unit_code: Option<String>,
    value: Option<f64>,
}

/// Every station the response places, by identifier.
///
/// Pure, and the reason the fetch above is three lines: this is what
/// `testdata/nws_radar_stations.json` exercises.
///
/// A station is skipped rather than defaulted whenever anything is missing or
/// implausible — no geometry, no elevation, an elevation in a unit this does not
/// recognise, a coordinate off the planet, or exactly (0, 0), which is a zeroed
/// record and not a radar in the Gulf of Guinea. Skipping leaves the radar
/// unplaced, which the union already has a meaning for; defaulting would invent
/// a position and persist it.
pub(crate) fn parse_stations(body: &str) -> BTreeMap<String, CataloguePosition> {
    let collection: StationCollection = match serde_json::from_str(body) {
        Ok(collection) => collection,
        Err(e) => {
            log::debug!("site catalogue: unreadable station list: {e}");
            return BTreeMap::new();
        }
    };
    collection
        .features
        .into_iter()
        .filter_map(|station| {
            let [lon, lat] = station.geometry?.coordinates;
            let elevation = station.properties.elevation?;
            // Metres only. `wmoUnit:m` is what the API sends today; anything
            // else is a unit change this code has not been read against.
            if elevation.unit_code.as_deref() != Some("wmoUnit:m") {
                return None;
            }
            let elevation_m = elevation.value?;
            if !lat.is_finite() || !lon.is_finite() || !elevation_m.is_finite() {
                return None;
            }
            if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
                return None;
            }
            if lat == 0.0 && lon == 0.0 {
                return None;
            }
            Some((
                station.properties.id,
                CataloguePosition {
                    lat_udeg: crate::site_position::micro_from_degrees(lat),
                    lon_udeg: crate::site_position::micro_from_degrees(lon),
                    elevation_m: elevation_m.round() as i32,
                },
            ))
        })
        .collect()
}

// Not on wasm: the live tests are `#[tokio::test]`, and tokio's
// `rt-multi-thread` is a dev-dependency this crate gates off wasm32 for that
// reason. `--all-targets` builds test code, so an ungated `mod tests` breaks
// `cargo check --target wasm32-unknown-unknown`.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;

// Split from `tests` because it is a different kind of test: everything in
// there is this crate checking its own arithmetic, and everything in here is
// this crate being checked against two outside sources that have never heard
// of it. Its module doc is the argument for why that distinction matters.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod datum_tests;
