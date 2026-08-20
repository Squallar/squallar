//! Which radars exist, and where they are — fetched, not compiled in.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CataloguePosition {
    /// Latitude, micro-degrees north.
    pub lat_udeg: i32,
    /// Longitude, micro-degrees east.
    pub lon_udeg: i32,
    /// The station record's one elevation, whole metres MSL — the **ground**
    /// under the tower for a WSR-88D, not the feedhorn.
    #[serde(alias = "feedhorn_m")]
    pub elevation_m: i32,
}

/// Every radar the live archive carries, with a position where one is
/// published.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct SiteCatalogue {
    sites: BTreeMap<String, Option<CataloguePosition>>,
}

impl SiteCatalogue {
    /// The union rule, and the only place it is expressed.
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
    pub fn is_empty(&self) -> bool {
        self.sites.is_empty()
    }

    /// Every identifier, placed or not, ascending.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.sites.keys().map(String::as_str)
    }

    /// What this catalogue has to say to [`crate::sites::resolve`].
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
#[derive(serde::Deserialize)]
struct Quantity {
    #[serde(rename = "unitCode")]
    unit_code: Option<String>,
    value: Option<f64>,
}

/// Every station the response places, by identifier.
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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod datum_tests;
