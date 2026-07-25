//! Fetching the three Storm Prediction Center products.
//!
//! # The requests must stay "simple"
//!
//! `www.spc.noaa.gov` answers a plain `GET` with `Access-Control-Allow-Origin: *`
//! but answers `OPTIONS` with `403` and **no CORS headers at all** — verified
//! 2026-07-25 for `/products/spcmdrss.xml`, `/products/outlook/*.lyr.geojson`
//! and `/climo/reports/today_*.csv`. That is the same shape as the Iowa
//! Environmental Mesonet, which answers `405`.
//!
//! So any non-safelisted request header — `User-Agent` included — turns the
//! `GET` into a preflighted request the browser never gets past, and outlooks,
//! mesoscale discussions and storm reports go silently missing on web only.
//! These three fetches used to share the application's ordinary
//! `User-Agent`-bearing client. They now use [`spc_client`].

use rustdar_radar::sources::DataSources;

use super::discussion::{SpcDiscussion, parse_md_rss};
use super::outlook::{OutlookDay, OutlookProduct, SpcOutlook, outlook_url, parse_geojson};

/// How long an SPC request may take.
const SPC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// The client every SPC fetch must use.
///
/// **Not `ctx.client`** — see the module docs. Split out so the choice is
/// assertable rather than restated at three handler call sites, and driven by
/// [`DataSources::spc_sends_user_agent`] so the rule lives with the origin.
pub fn spc_client(sources: &DataSources) -> Result<reqwest::Client, String> {
    sources
        .spc_client(SPC_TIMEOUT)
        .build()
        .map_err(|e| format!("could not build the SPC client: {e}"))
}

/// The mesoscale discussion RSS feed.
fn md_rss_url(sources: &DataSources) -> String {
    format!("{}/products/spcmdrss.xml", sources.spc_base)
}

/// Fetch an SPC outlook product and parse it into an `SpcOutlook`.
pub async fn fetch_outlook(
    client: &reqwest::Client,
    sources: &DataSources,
    day: OutlookDay,
    product: OutlookProduct,
) -> Result<SpcOutlook, String> {
    let url = outlook_url(sources, day, product);
    log::info!("Fetching SPC outlook: {}", url);

    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("HTTP request failed for {}: {}", url, e))?;

    if !response.status().is_success() {
        return Err(format!(
            "SPC returned HTTP {} for {}",
            response.status(),
            url
        ));
    }

    let text = response
        .text()
        .await
        .map_err(|e| format!("Failed to read response body: {}", e))?;

    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("Invalid JSON from {}: {}", url, e))?;

    parse_geojson(&json, day, product)
}


/// Fetch all currently active SPC Mesoscale Discussions from the RSS feed.
pub async fn fetch_active_discussions(
    client: &reqwest::Client,
    sources: &DataSources,
) -> Result<Vec<SpcDiscussion>, String> {
    let url = md_rss_url(sources);
    log::info!("Fetching SPC Mesoscale Discussions from {url}");

    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("SPC MD RSS request failed: {}", e))?;

    if !response.status().is_success() {
        return Err(format!(
            "SPC returned HTTP {} for MD RSS feed",
            response.status()
        ));
    }

    let text = response
        .text()
        .await
        .map_err(|e| format!("Failed to read SPC MD RSS body: {}", e))?;

    parse_md_rss(&text)
}

/// Which products are available for a given day.
pub fn available_products(day: OutlookDay) -> Vec<OutlookProduct> {
    if day.is_extended() {
        // Days 4-8: single "any severe" probabilistic product
        return vec![OutlookProduct::Probabilistic];
    }
    match day {
        OutlookDay::Day1 | OutlookDay::Day2 => vec![
            OutlookProduct::Categorical,
            OutlookProduct::Tornado,
            OutlookProduct::Wind,
            OutlookProduct::Hail,
        ],
        OutlookDay::Day3 => vec![
            OutlookProduct::Categorical,
            OutlookProduct::Probabilistic,
        ],
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spc::reports::{StormReportKind, report_url};

    /// SPC must be fetched with a client that carries no `User-Agent`.
    ///
    /// SPC was missing from both evidence tables in `DataSources` while three
    /// handlers fetched it through the shared `User-Agent`-bearing client. It
    /// answers `GET` with `ACAO: *` and `OPTIONS` with `403` and no CORS
    /// headers, so outlooks, mesoscale discussions and storm reports were
    /// preflighted out of existence in the browser — and nowhere else.
    #[test]
    fn the_spc_client_sends_no_user_agent() {
        let client = spc_client(&DataSources::production()).expect("the SPC client must build");
        assert!(
            !rustdar_radar::tls::sends_user_agent(&client),
            "the SPC client carries a User-Agent, so the browser preflights \
             the GET and SPC answers OPTIONS with 403 — outlooks, MDs and \
             storm reports silently never arrive, and only on web",
        );
    }

    /// …and the origin's recorded rule is what decides that, not a constant.
    #[test]
    fn the_spc_client_follows_the_origins_recorded_rule() {
        let sources = DataSources { spc_sends_user_agent: true, ..DataSources::production() };
        let client = spc_client(&sources).expect("the SPC client must build");
        assert!(
            rustdar_radar::tls::sends_user_agent(&client),
            "spc_client ignores DataSources::spc_sends_user_agent",
        );
    }

    /// Every SPC URL is built from `spc_base`, so pointing that field at a mock
    /// moves all of them.
    ///
    /// This is what makes `spc_base` load-bearing: it was a declared field no
    /// URL was built from, which meant the origin table's own "no production
    /// origin is one the browser cannot reach" check was inspecting a string
    /// nothing used.
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
        ];
        for url in &urls {
            assert!(
                url.starts_with("http://127.0.0.1:8080/"),
                "{url} does not come from spc_base",
            );
            assert!(!url.contains("spc.noaa.gov"), "{url} still hardcodes the origin");
        }
    }

    /// The production paths, transcribed from the endpoints probed live on
    /// 2026-07-25. Hardcoded expectations, so threading `spc_base` through
    /// cleanly while mangling a path still fails.
    #[test]
    fn the_production_spc_paths_are_the_ones_that_were_probed() {
        let s = DataSources::production();
        assert_eq!(md_rss_url(&s), "https://www.spc.noaa.gov/products/spcmdrss.xml");
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
    }
}
