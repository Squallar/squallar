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
//! * **What place they are at** — the same station record's `name`, which is a
//!   settlement or a landmark and never the identifier: `"Milwaukee"` for
//!   `KMKX`, `"Andrews Air Force Base"` for `TADW`. Measured over the live body
//!   on 2026-08-21: all 208 stations carry one, every one ASCII, the longest 22
//!   characters. It is free text with no separable state — `"Charleston,SC"` and
//!   `"Western Arkansas"` are both whole names — so it is carried as one string
//!   and never split. Two radars may share a name — 18 names are carried by a
//!   pair each, 36 radars, almost always a metro's WSR-88D and its TDWR — which
//!   is a fact about the network rather than a collision to resolve.
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

use crate::sites::{RadarNetwork, SiteFix};
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

/// What the published station record says about one radar: where it is, which
/// network it is on, and what place it is at.
///
/// Not `Copy`, since [`place`](Self::place) is owned text. It was `Copy` while
/// every field was an `i32`; the callers that leant on that clone instead.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CataloguePosition {
    /// Latitude, micro-degrees north.
    pub lat_udeg: i32,
    /// Longitude, micro-degrees east.
    pub lon_udeg: i32,
    /// The station record's one elevation, whole metres MSL — the **ground**
    /// under the tower for a WSR-88D, not the feedhorn.
    #[serde(alias = "feedhorn_m")]
    pub elevation_m: i32,
    /// Which network the station record says this radar belongs to, or `None`
    /// where it says nothing this build recognises.
    ///
    /// **The API is the authority here and the identifier rule is the offline
    /// approximation of it** — [`RadarNetwork::of_id`] answers for every
    /// identifier, including the ones no station record places, and
    /// `the_prefix_rule_agrees_with_the_api_on_every_placed_station` is what
    /// keeps the two from drifting apart in silence.
    ///
    /// **The `Option` is what makes an old cache load, not the attribute.**
    /// [`SiteCatalogue`] is `#[serde(transparent)]` over a bare map and is
    /// persisted as a cache; appending a field leaves every key already in the
    /// blob where it was, a cache written before this field existed loads with
    /// `None` here, and a cache written *after* it loads into an older build
    /// with the field ignored and dropped on that build's next write. Both
    /// directions are lossless for the positions, which is all a cache is for.
    ///
    /// `#[serde(default)]` is redundant beside an `Option` and is kept only to
    /// state the intent — **measured**: removing it leaves
    /// `a_cache_written_before_the_network_was_learned_loads_without_one`
    /// green, and making the field required is what turns it red.
    #[serde(default)]
    pub network: Option<RadarNetwork>,
    /// The station record's `name` — the place, not the identifier — or `None`
    /// where the record carries none or carries one `place_from_record`
    /// rejects.
    ///
    /// Cached with the position because it is read before the first frame and
    /// the fetch that could supply it lands after: a launch that had to wait
    /// for the network would show a list of bare ICAOs and then relabel it
    /// under the reader. Additive on the same terms as
    /// [`network`](Self::network) — a cache written before this field existed
    /// loads with `None` here.
    #[serde(default)]
    pub place: Option<String>,
}

/// The longest station name this build will carry, characters.
///
/// The live body's longest is 22 (`"Andrews Air Force Base"`). This is a bound
/// on what a doctored feed or a hand-edited cache can push into a row, not a
/// judgement about real names.
const MAX_PLACE_CHARS: usize = 64;

/// The station record's `name` as a place worth showing, or `None`.
///
/// One rule, applied at both trust boundaries — the parsed feed and the loaded
/// cache — so a name that reaches a row got there the same way whichever it
/// came from. Empty is `None` rather than `Some("")`: a caller drawing
/// `"KMKX — "` with nothing after the dash is worse than one drawing `"KMKX"`.
pub(crate) fn place_from_record(raw: &str) -> Option<&str> {
    let trimmed = raw.trim();
    (!trimmed.is_empty() && trimmed.chars().count() <= MAX_PLACE_CHARS).then_some(trimmed)
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
                    let position = positions.get(&id).cloned();
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
        self.sites.get(id).cloned().flatten()
    }

    /// What place this catalogue says `id` is at, or `None` where it does not
    /// carry `id`, cannot place it, or was written before the station record's
    /// name was read.
    ///
    /// A **placed** row is the only one that can carry this, for the same
    /// reason [`network`](Self::network) gives: the record that names a radar
    /// is the record that places it.
    pub fn place(&self, id: &str) -> Option<&str> {
        self.sites
            .get(id)?
            .as_ref()?
            .place
            .as_deref()
            .and_then(place_from_record)
    }

    /// Which network this catalogue says `id` is on, or `None` where it does
    /// not carry `id`, cannot place it, or was written before the station
    /// record's type was read.
    ///
    /// A **placed** row is the only one that can carry this: an identifier the
    /// bucket lists and the NWS does not has no station record to have stated a
    /// type. [`RadarNetwork::of_id`] is what answers for those.
    pub fn network(&self, id: &str) -> Option<RadarNetwork> {
        self.sites.get(id)?.as_ref()?.network
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
    pub fn fixes(&self) -> impl Iterator<Item = (&str, SiteFix<'_>)> {
        self.sites.iter().map(|(id, position)| {
            let fix = match position {
                Some(position) => SiteFix::Network {
                    lat_udeg: position.lat_udeg,
                    lon_udeg: position.lon_udeg,
                    elevation_m: position.elevation_m,
                    // Re-checked here and not only at the parse: this half of
                    // the pair is read straight off a cache file, which is
                    // outside the process and editable.
                    place: position.place.as_deref().and_then(place_from_record),
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
    // Counted over the fixes that carry something, not over `fixes()` itself:
    // every member yields a fix, so the old `fixes().count()` was `len()` in
    // another spelling and read as "all of them placed" whatever had happened.
    let placed = catalogue
        .fixes()
        .filter(|(_, fix)| matches!(fix, SiteFix::Network { .. }))
        .count();
    let named = catalogue
        .fixes()
        .filter(|(_, fix)| matches!(fix, SiteFix::Network { place: Some(_), .. }))
        .count();
    log::info!(
        "site catalogue: {} radars listed, {placed} placed, {named} named",
        catalogue.len(),
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
    /// The place this radar is at — `"Milwaukee"`, `"Andrews Air Force Base"`.
    /// Optional because a record with no name is still a record with a
    /// position, and the position is what the table is built from.
    #[serde(default)]
    name: Option<String>,
    elevation: Option<Quantity>,
    /// `"WSR-88D"`, `"TDWR"`, or something this build does not recognise —
    /// the API carries profilers too. Never a reason to skip a station: an
    /// unrecognised type leaves the network unknown and the position good.
    #[serde(rename = "stationType", default)]
    station_type: Option<String>,
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
            let network = match station.properties.station_type.as_deref() {
                Some("WSR-88D") => Some(RadarNetwork::Wsr88d),
                Some("TDWR") => Some(RadarNetwork::Tdwr),
                _ => None,
            };
            let place = station
                .properties
                .name
                .as_deref()
                .and_then(place_from_record)
                .map(str::to_owned);
            Some((
                station.properties.id,
                CataloguePosition {
                    lat_udeg: crate::site_position::micro_from_degrees(lat),
                    lon_udeg: crate::site_position::micro_from_degrees(lon),
                    elevation_m: elevation_m.round() as i32,
                    network,
                    place,
                },
            ))
        })
        .collect()
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod datum_tests;
