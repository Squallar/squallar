//! Fetching the four Storm Prediction Center products.
//!
//! Probed with curl 2026-07-25 on `/products/spcmdrss.xml`,
//! `/products/outlook/*.lyr.geojson` and `/climo/reports/today_*.csv`, and
//! again 2026-08-21 over all 28 `.../fire_wx/day{1..8}fw_*.lyr.geojson`:
//! `www.spc.noaa.gov` answers a plain `GET` with `Access-Control-Allow-Origin: *`
//! but answers `OPTIONS` with `403` and **no CORS headers at all** (IEM answers
//! `405`, same shape). Any non-safelisted request header — `User-Agent`
//! included — turns the `GET` into a preflight the browser never gets past, and
//! outlooks, fire weather outlooks, MDs and storm reports go silently missing
//! on web only. Use [`spc_client`], never `ctx.client`.

use squallar_source::origins::DataSources;

use super::discussion::{SpcDiscussion, parse_md_rss};
use super::firewx::{FireDay, FireHazard, FireProduct, SpcFireOutlook, firewx_url};
use super::outlook::{OutlookDay, OutlookProduct, SpcOutlook, outlook_url, parse_geojson};
use crate::fetch_policy::{FetchError, NotFound};

const SPC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// The client every SPC fetch must use — **not `ctx.client`**, see module docs.
/// Driven by [`DataSources::spc_sends_user_agent`] so the rule lives with the
/// origin, and so the choice is assertable in one place.
pub fn spc_client(sources: &DataSources) -> Result<reqwest::Client, String> {
    sources
        .spc_client(SPC_TIMEOUT)
        .build()
        .map_err(|e| format!("could not build the SPC client: {e}"))
}

fn md_rss_url(sources: &DataSources) -> String {
    format!("{}/products/spcmdrss.xml", sources.spc_base)
}

/// A 404 here is **routine**: SPC takes an outlook down when it expires, and
/// there is no Day 4-8 probabilistic product up at every hour of the day. The
/// layer must read that as "not published right now" rather than as a fault it
/// should retry — see [`NotFound`].
pub async fn fetch_outlook(
    client: &reqwest::Client,
    sources: &DataSources,
    day: OutlookDay,
    product: OutlookProduct,
) -> Result<SpcOutlook, FetchError> {
    let url = outlook_url(sources, day, product);
    log::info!("Fetching SPC outlook: {}", url);

    let response = client.get(&url).send().await.map_err(|e| {
        FetchError::from_transport(&e, format!("HTTP request failed for {url}: {e}"))
    })?;

    if !response.status().is_success() {
        return Err(FetchError::from_status(
            response.status(),
            NotFound::IsRoutine,
            format!("SPC returned HTTP {} for {url}", response.status()),
        ));
    }

    let text = response.text().await.map_err(|e| {
        FetchError::from_transport(&e, format!("Failed to read response body: {e}"))
    })?;

    let json: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| FetchError::transient(format!("Invalid JSON from {url}: {e}")))?;

    parse_geojson(&json, day, product).map_err(FetchError::transient)
}

/// The fire-weather twin of [`fetch_outlook`], and a 404 is **routine** for the
/// same reason: SPC takes a fire outlook down when it expires. All 28 paths
/// answered `200` when probed on 2026-08-21, but an expired one will not.
pub async fn fetch_firewx(
    client: &reqwest::Client,
    sources: &DataSources,
    day: FireDay,
    hazard: FireHazard,
    product: FireProduct,
) -> Result<SpcFireOutlook, FetchError> {
    let url = firewx_url(sources, day, hazard, product);
    log::info!("Fetching SPC fire weather outlook: {url}");

    let response = client.get(&url).send().await.map_err(|e| {
        FetchError::from_transport(&e, format!("HTTP request failed for {url}: {e}"))
    })?;

    if !response.status().is_success() {
        return Err(FetchError::from_status(
            response.status(),
            NotFound::IsRoutine,
            format!("SPC returned HTTP {} for {url}", response.status()),
        ));
    }

    let text = response.text().await.map_err(|e| {
        FetchError::from_transport(&e, format!("Failed to read response body: {e}"))
    })?;

    let json: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| FetchError::transient(format!("Invalid JSON from {url}: {e}")))?;

    super::firewx::parse_geojson(&json, day, hazard, product).map_err(FetchError::transient)
}

/// A 404 here is **broken**, not routine: `spcmdrss.xml` is a standing feed, so
/// its absence means the path moved rather than that no discussion is active.
/// An active-MD-free day still serves the feed, with no `<item>` elements.
pub async fn fetch_active_discussions(
    client: &reqwest::Client,
    sources: &DataSources,
) -> Result<Vec<SpcDiscussion>, FetchError> {
    let url = md_rss_url(sources);
    log::info!("Fetching SPC Mesoscale Discussions from {url}");

    let response =
        client.get(&url).send().await.map_err(|e| {
            FetchError::from_transport(&e, format!("SPC MD RSS request failed: {e}"))
        })?;

    if !response.status().is_success() {
        return Err(FetchError::from_status(
            response.status(),
            NotFound::IsBroken,
            format!("SPC returned HTTP {} for MD RSS feed", response.status()),
        ));
    }

    let text = response.text().await.map_err(|e| {
        FetchError::from_transport(&e, format!("Failed to read SPC MD RSS body: {e}"))
    })?;

    parse_md_rss(&text).map_err(FetchError::transient)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spc::reports::{StormReportKind, report_url};

    /// Fails if SPC is fetched with a `User-Agent`-bearing client, which
    /// preflights outlooks, MDs and storm reports out of existence on web only.
    #[test]
    fn the_spc_client_sends_no_user_agent() {
        let client = spc_client(&DataSources::production()).expect("the SPC client must build");
        assert!(
            !squallar_source::tls::sends_user_agent(&client),
            "the SPC client carries a User-Agent, so the browser preflights \
             the GET and SPC answers OPTIONS with 403 — outlooks, MDs and \
             storm reports silently never arrive, and only on web",
        );
    }

    /// Fails if the rule is a constant rather than the origin's recorded one.
    #[test]
    fn the_spc_client_follows_the_origins_recorded_rule() {
        let sources = DataSources {
            spc_sends_user_agent: true,
            ..DataSources::production()
        };
        let client = spc_client(&sources).expect("the SPC client must build");
        assert!(
            squallar_source::tls::sends_user_agent(&client),
            "spc_client ignores DataSources::spc_sends_user_agent",
        );
    }

    /// Fails if any SPC URL bypasses `spc_base`. Without this the origin
    /// table's reachability check inspects a string no URL is built from.
    #[test]
    fn every_spc_url_comes_from_the_declared_origin() {
        let sources = DataSources {
            spc_base: std::borrow::Cow::Borrowed("http://127.0.0.1:8080"),
            ..DataSources::production()
        };
        let urls = [
            md_rss_url(&sources),
            outlook_url(&sources, OutlookDay::Day1, OutlookProduct::Categorical),
            outlook_url(&sources, OutlookDay::Day4, OutlookProduct::Probabilistic),
            report_url(&sources, StormReportKind::Tornado),
            report_url(&sources, StormReportKind::Hail),
            report_url(&sources, StormReportKind::Wind),
            firewx_url(
                &sources,
                FireDay::Day1,
                FireHazard::DryThunderstorm,
                FireProduct::Categorical,
            ),
            firewx_url(
                &sources,
                FireDay::Day2,
                FireHazard::WindRh,
                FireProduct::Categorical,
            ),
            firewx_url(
                &sources,
                FireDay::Day3,
                FireHazard::DryThunderstorm,
                FireProduct::Probabilistic,
            ),
            firewx_url(
                &sources,
                FireDay::Day8,
                FireHazard::WindRh,
                FireProduct::Categorical,
            ),
        ];
        for url in &urls {
            assert!(
                url.starts_with("http://127.0.0.1:8080/"),
                "{url} does not come from spc_base",
            );
            assert!(
                !url.contains("spc.noaa.gov"),
                "{url} still hardcodes the origin"
            );
        }
    }

    /// Paths transcribed from the endpoints probed live 2026-07-25. Hardcoded,
    /// so threading `spc_base` cleanly while mangling a path still fails.
    #[test]
    fn the_production_spc_paths_are_the_ones_that_were_probed() {
        let s = DataSources::production();
        assert_eq!(
            md_rss_url(&s),
            "https://www.spc.noaa.gov/products/spcmdrss.xml"
        );
        assert_eq!(
            outlook_url(&s, OutlookDay::Day1, OutlookProduct::Categorical),
            "https://www.spc.noaa.gov/products/outlook/day1otlk_cat.lyr.geojson",
        );
        assert_eq!(
            outlook_url(&s, OutlookDay::Day2, OutlookProduct::Tornado),
            "https://www.spc.noaa.gov/products/outlook/day2otlk_torn.lyr.geojson",
        );
        assert_eq!(
            outlook_url(&s, OutlookDay::Day3, OutlookProduct::Probabilistic),
            "https://www.spc.noaa.gov/products/outlook/day3otlk_prob.lyr.geojson",
        );
        assert_eq!(
            outlook_url(&s, OutlookDay::Day5, OutlookProduct::Probabilistic),
            "https://www.spc.noaa.gov/products/exper/day4-8/day5prob.lyr.geojson",
        );
        assert_eq!(
            report_url(&s, StormReportKind::Tornado),
            "https://www.spc.noaa.gov/climo/reports/today_torn.csv",
        );
        assert_eq!(
            report_url(&s, StormReportKind::Wind),
            "https://www.spc.noaa.gov/climo/reports/today_wind.csv",
        );
        // One of each fire arm; all 28 are pinned in
        // `firewx::tests::the_production_fire_paths_are_the_ones_that_were_probed`.
        assert_eq!(
            firewx_url(
                &s,
                FireDay::Day1,
                FireHazard::DryThunderstorm,
                FireProduct::Categorical
            ),
            "https://www.spc.noaa.gov/products/fire_wx/day1fw_dryt.lyr.geojson",
        );
        assert_eq!(
            firewx_url(
                &s,
                FireDay::Day3,
                FireHazard::WindRh,
                FireProduct::Probabilistic
            ),
            "https://www.spc.noaa.gov/products/exper/fire_wx/day3fw_windrhprob.lyr.geojson",
        );
    }
}
