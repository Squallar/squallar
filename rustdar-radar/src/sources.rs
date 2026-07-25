//! Every network origin rustdar reads from, declared in one place.
//!
//! Origins used to be `const` string literals sitting next to the fetch code
//! that used them, which made "can the web build reach all of this?" a question
//! you could only answer by grepping. This module is the answer: one struct,
//! one production instance, and a `CORS` column that says why each origin was
//! chosen.
//!
//! # The browser constraint
//!
//! rustdar's web build issues every request through the browser's `fetch()`,
//! which means the *server* decides whether the response is readable. An origin
//! that answers `200` to `curl` but omits `Access-Control-Allow-Origin` is
//! unusable from the browser, and no amount of client-side work changes that —
//! the only fixes are a proxy (rejected: the web build must not require a
//! server) or a different origin.
//!
//! Three origins rustdar used to depend on are unusable this way. Verified
//! 2026-07-25 with `curl -H 'Origin: https://example.com'`:
//!
//! | Origin | `ACAO` | Verdict |
//! |---|---|---|
//! | `tgftp.nws.noaa.gov` | absent (and `403` with an `Origin:` header) | replaced by [`Self::level3_bucket`] |
//! | `nomads.ncep.noaa.gov` | absent | replaced by [`Self::hrrr_bucket`] |
//! | `aviationweather.gov` | absent | replaced by [`Self::iem_base`] |
//!
//! Everything below answers `Access-Control-Allow-Origin: *`.
//!
//! # Preflight is *not* uniformly harmless
//!
//! A request carrying a non-safelisted header — which `User-Agent` is — stops
//! being a "simple request" and the browser sends a `OPTIONS` preflight first.
//! The response to *that* must be 2xx and must allow the method and header, or
//! the real request is never sent.
//!
//! Most of rustdar's origins handle this correctly:
//!
//! ```text
//! unidata-nexrad-level3  OPTIONS -> 200, Allow-Methods: GET, HEAD, Allow-Headers: user-agent
//! noaa-hrrr-bdp-pds      OPTIONS -> 200, Allow-Methods: GET,       Allow-Headers: user-agent
//! api.weather.gov        OPTIONS -> 200, Allow-Methods: GET,       Allow-Headers: API-Key, User-Agent
//! ```
//!
//! **Iowa Environmental Mesonet does not.** Its `OPTIONS` answers `405 Method
//! Not Allowed` with `Allow: GET` and no `Access-Control-Allow-Methods`, so a
//! preflight fails outright even though the plain `GET` carries `ACAO: *`:
//!
//! ```text
//! mesonet.agron.iastate.edu  GET     -> 200, Access-Control-Allow-Origin: *
//! mesonet.agron.iastate.edu  OPTIONS -> 405 Method Not Allowed
//! ```
//!
//! So METAR requests must stay *simple*: no `User-Agent`, no custom headers.
//! [`Self::metar_sends_user_agent`] is that rule, stated where the origin is
//! declared rather than buried in the fetch function, and
//! [`crate::tls::simple_client`] is the client that honours it.

use std::borrow::Cow;

/// A borrowed-or-owned origin string.
///
/// `Cow` rather than `&'static str` so a test can point one field at a local
/// mock server without every other field having to be allocated too.
pub type Source = Cow<'static, str>;

/// The set of network origins rustdar reads from.
///
/// Construct with [`DataSources::production`]. Fields are public so a test can
/// override exactly one origin and leave the rest alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataSources {
    /// NEXRAD Level II archive volumes. Keys are `YYYY/MM/DD/SITE/NAME`.
    pub level2_bucket: Source,
    /// NEXRAD Level II real-time chunks.
    pub level2_chunks_bucket: Source,
    /// NEXRAD Level III products. Keys are **flat**: `SSS_PPP_YYYY_MM_DD_HH_MM_SS`.
    pub level3_bucket: Source,
    /// HRRR GRIB2 output, mirrored from NCEP by the Big Data Program.
    pub hrrr_bucket: Source,
    /// GOES-East (GOES-19) granules, for GLM lightning.
    pub goes_east_bucket: Source,
    /// GOES-West (GOES-18) granules, for GLM lightning.
    pub goes_west_bucket: Source,
    /// NWS public API: active alerts and zone geometry.
    pub nws_api_base: Source,
    /// Storm Prediction Center: outlooks, mesoscale discussions, storm reports.
    pub spc_base: Source,
    /// Iowa Environmental Mesonet: current ASOS/METAR observations.
    pub iem_base: Source,
    /// Whether METAR requests may carry a `User-Agent`.
    ///
    /// `false` for [`Self::production`]: see the module docs. Sending one turns
    /// the request into a preflight, and IEM answers `405` to `OPTIONS`.
    pub metar_sends_user_agent: bool,
}

impl Default for DataSources {
    fn default() -> Self {
        Self::production()
    }
}

impl DataSources {
    /// The origins the shipped application uses.
    ///
    /// Every one of these answers `Access-Control-Allow-Origin: *`, so the web
    /// build reaches all of them without a proxy.
    pub const fn production() -> Self {
        Self {
            level2_bucket: Cow::Borrowed("unidata-nexrad-level2"),
            level2_chunks_bucket: Cow::Borrowed("unidata-nexrad-level2-chunks"),
            level3_bucket: Cow::Borrowed("unidata-nexrad-level3"),
            hrrr_bucket: Cow::Borrowed("noaa-hrrr-bdp-pds"),
            goes_east_bucket: Cow::Borrowed("noaa-goes19"),
            goes_west_bucket: Cow::Borrowed("noaa-goes18"),
            nws_api_base: Cow::Borrowed("https://api.weather.gov"),
            spc_base: Cow::Borrowed("https://www.spc.noaa.gov"),
            iem_base: Cow::Borrowed("https://mesonet.agron.iastate.edu"),
            metar_sends_user_agent: false,
        }
    }

    /// `https://{bucket}.s3.amazonaws.com/{key}`.
    ///
    /// The key is interpolated, not encoded: every key rustdar builds is drawn
    /// from `[A-Za-z0-9_./-]`, and encoding would have to leave the `/`
    /// separators intact anyway.
    pub fn s3_object_url(bucket: &str, key: &str) -> String {
        format!("https://{bucket}.s3.amazonaws.com/{key}")
    }

    /// Object URL for one Level III product file.
    pub fn level3_object_url(&self, key: &str) -> String {
        Self::s3_object_url(&self.level3_bucket, key)
    }

    /// The flat key prefix for one site/product/day in the Level III bucket.
    ///
    /// The bucket has **no directory structure and no `sn.last`**: keys are
    /// `TLX_N0S_2026_07_25_01_20_27`, so "the latest product" is the last key
    /// of a prefix listing rather than a fixed filename.
    ///
    /// `site3` is the **three**-letter site code — `TLX`, not `KTLX`. See
    /// [`crate::level3::site_code`].
    pub fn level3_day_prefix(site3: &str, product: &str, date: &chrono::NaiveDate) -> String {
        format!("{site3}_{product}_{}", date.format("%Y_%m_%d"))
    }

    /// The GRIB2 object key for one HRRR run and forecast hour.
    pub fn hrrr_key(date: &chrono::NaiveDate, run_hour: u8, forecast_hour: u8) -> String {
        format!(
            "hrrr.{}/conus/hrrr.t{run_hour:02}z.wrfsfcf{forecast_hour:02}.grib2",
            date.format("%Y%m%d"),
        )
    }

    /// URL of one HRRR GRIB2 file.
    pub fn hrrr_grib_url(&self, date: &chrono::NaiveDate, run_hour: u8, forecast_hour: u8) -> String {
        Self::s3_object_url(&self.hrrr_bucket, &Self::hrrr_key(date, run_hour, forecast_hour))
    }

    /// URL of the `.idx` sidecar listing that GRIB2 file's records and their
    /// byte offsets.
    ///
    /// This is what makes the S3 path cheaper than the NOMADS filter it
    /// replaces: fetch ~9 KB of index, then `Range:`-request the one record
    /// wanted instead of having a CGI re-pack and stream the whole field.
    pub fn hrrr_idx_url(&self, date: &chrono::NaiveDate, run_hour: u8, forecast_hour: u8) -> String {
        format!(
            "{}.idx",
            self.hrrr_grib_url(date, run_hour, forecast_hour)
        )
    }

    /// Current ASOS observations for one US state, as JSON.
    ///
    /// Scoped to a state (~72 KB) rather than the whole network: the
    /// `?networkclass=ASOS` form is one request but **54 MB, ungzipped**.
    pub fn metar_state_url(&self, state: &str) -> String {
        format!(
            "{}/api/1/currents.json?network={state}_ASOS",
            self.iem_base,
        )
    }

    /// Active NWS alerts, as GeoJSON.
    pub fn nws_alerts_url(&self) -> String {
        format!("{}/alerts/active?status=actual", self.nws_api_base)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn date() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 7, 25).unwrap()
    }

    /// The three origins this module exists to get rid of must not reappear in
    /// any production URL.
    ///
    /// Each of them answers `curl` fine and is invisible to every other test in
    /// the workspace; the only symptom of a regression is that the web build
    /// silently loses a layer.
    #[test]
    fn no_production_origin_is_one_the_browser_cannot_reach() {
        let s = DataSources::production();
        let urls = [
            s.level3_object_url("TLX_N0S_2026_07_25_01_20_27"),
            s.hrrr_grib_url(&date(), 3, 0),
            s.hrrr_idx_url(&date(), 3, 0),
            s.metar_state_url("OK"),
            s.nws_alerts_url(),
            DataSources::s3_object_url(&s.level2_bucket, "k"),
            DataSources::s3_object_url(&s.goes_east_bucket, "k"),
            DataSources::s3_object_url(&s.goes_west_bucket, "k"),
            s.spc_base.to_string(),
        ];
        for url in urls {
            for blocked in ["tgftp.nws.noaa.gov", "nomads.ncep.noaa.gov", "aviationweather.gov"] {
                assert!(
                    !url.contains(blocked),
                    "{url} still points at {blocked}, which sends no \
                     Access-Control-Allow-Origin and is unreachable from the web build",
                );
            }
            assert!(url.starts_with("https://"), "{url} is not https");
        }
    }

    /// Level III keys are flat and carry no `sn.last`, so the prefix is the
    /// whole addressing scheme.
    ///
    /// The expected string is the shape of a key observed in the live bucket
    /// (`TLX_N0S_2026_07_25_17_23_22`), truncated at the day.
    #[test]
    fn a_level3_prefix_is_site_product_and_an_underscored_date() {
        assert_eq!(
            DataSources::level3_day_prefix("TLX", "N0S", &date()),
            "TLX_N0S_2026_07_25",
        );
        // Zero-padded, not `2026_7_5`: S3 prefix matching is bytewise.
        let january = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        assert_eq!(
            DataSources::level3_day_prefix("FWS", "DPR", &january),
            "FWS_DPR_2026_01_05",
        );
    }

    /// The HRRR key layout, transcribed from a live listing of
    /// `noaa-hrrr-bdp-pds`.
    #[test]
    fn the_hrrr_key_names_a_run_and_a_forecast_hour() {
        assert_eq!(
            DataSources::hrrr_key(&date(), 3, 0),
            "hrrr.20260725/conus/hrrr.t03z.wrfsfcf00.grib2",
        );
        // f01 is where the UH accumulation window is nonzero, and 14z exercises
        // a two-digit run hour.
        assert_eq!(
            DataSources::hrrr_key(&date(), 14, 1),
            "hrrr.20260725/conus/hrrr.t14z.wrfsfcf01.grib2",
        );
    }

    /// The index URL must be the GRIB URL plus `.idx` — a separate object, not
    /// a query parameter.
    #[test]
    fn the_idx_url_is_the_grib_url_with_a_suffix() {
        let s = DataSources::production();
        assert_eq!(
            s.hrrr_idx_url(&date(), 3, 0),
            format!("{}.idx", s.hrrr_grib_url(&date(), 3, 0)),
        );
        assert!(s.hrrr_idx_url(&date(), 3, 0).ends_with("wrfsfcf00.grib2.idx"));
    }

    /// `network=<ST>_ASOS`, never `networkclass=ASOS`.
    ///
    /// The two differ by a factor of 750 in transfer size (72 KB vs 54 MB,
    /// measured), and the wrong one still returns valid JSON — so nothing
    /// downstream would notice.
    #[test]
    fn metar_is_scoped_to_one_state_not_the_whole_network() {
        let url = DataSources::production().metar_state_url("OK");
        assert_eq!(
            url,
            "https://mesonet.agron.iastate.edu/api/1/currents.json?network=OK_ASOS",
        );
        assert!(
            !url.contains("networkclass"),
            "networkclass=ASOS is a 54 MB ungzipped response",
        );
    }

    /// The preflight rule is a property of the origin, so it is recorded on the
    /// origin. Production must not send a `User-Agent` to IEM.
    #[test]
    fn production_keeps_metar_requests_preflight_free() {
        assert!(
            !DataSources::production().metar_sends_user_agent,
            "IEM answers 405 to OPTIONS; a User-Agent turns the GET into a \
             preflight and the request never happens in a browser",
        );
    }

    /// Overriding one field must not disturb the others — this is what lets a
    /// test point at a mock server.
    #[test]
    fn one_origin_can_be_overridden_in_isolation() {
        let s = DataSources {
            iem_base: Cow::Owned("http://127.0.0.1:8080".to_string()),
            ..DataSources::production()
        };
        assert_eq!(
            s.metar_state_url("TX"),
            "http://127.0.0.1:8080/api/1/currents.json?network=TX_ASOS",
        );
        assert_eq!(s.level3_bucket, DataSources::production().level3_bucket);
    }
}
