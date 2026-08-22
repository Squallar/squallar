//! Every network origin rustdar reads from, declared in one place.
//!
//! The web build issues every request through `fetch()`: an origin that omits
//! `Access-Control-Allow-Origin` is unusable from the browser.
//!
//! Per-origin evidence, verified 2026-07-25 by `curl -H 'Origin: …'` including
//! preflight. **Not re-derivable by reading code; re-probe before changing.**
//!
//! Unreachable, replaced:
//! ```text
//! tgftp.nws.noaa.gov     no ACAO (403 with an Origin: header)  -> level3_bucket
//! nomads.ncep.noaa.gov   no ACAO                               -> hrrr_bucket
//! aviationweather.gov    no ACAO                               -> iem_base
//! ```
//!
//! Preflight-**hostile** — plain `GET` carries `ACAO: *`, so curl and every
//! native build are happy while no browser issues the real request:
//! ```text
//! mesonet.agron.iastate.edu  GET -> 200 ACAO: *   OPTIONS -> 405, Allow: GET, no ACA-Methods
//! www.spc.noaa.gov           GET -> 200 ACAO: *   OPTIONS -> 403 (CloudFront), no CORS headers
//! ```
//!
//! So METAR and SPC requests must stay **simple**: no `User-Agent`, no custom
//! headers; [`DataSources`] records that per origin.

use std::borrow::Cow;

/// `Cow` so a test can point one field at a local mock server.
pub type Source = Cow<'static, str>;

/// The set of network origins rustdar reads from. Construct with
/// [`DataSources::production`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataSources {
    /// NEXRAD Level II archive volumes. Keys are `YYYY/MM/DD/SITE/NAME`.
    pub level2_bucket: Source,
    /// NEXRAD Level II real-time chunks. Keys are `SITE/VOLUME/NAME`.
    pub level2_chunks_bucket: Source,
    /// NEXRAD Level III products. Keys are **flat**: `SSS_PPP_YYYY_MM_DD_HH_MM_SS`.
    pub level3_bucket: Source,
    /// HRRR GRIB2 output, mirrored from NCEP by the Big Data Program.
    pub hrrr_bucket: Source,
    /// GOES-East granules, for GLM lightning. Names the orbital *slot*, currently
    /// GOES-19: `noaa-goes16` has no GLM data after 2025 day 097.
    pub goes_east_bucket: Source,
    /// GOES-West (currently GOES-18) granules, for GLM lightning.
    pub goes_west_bucket: Source,
    /// MRMS national mosaic products. Keys are
    /// `CONUS/{product}/{YYYYMMDD}/MRMS_{product}_{YYYYMMDD}-{HHMMSS}.grib2.gz`.
    pub mrms_bucket: Source,
    /// GMGSI global geostationary mosaic. Keys are
    /// `GMGSI_{LW,SW,VIS,WV}/{YYYY}/{MM}/{DD}/{HH}/GLOBCOMP*_v3r0_blend_s*_e*_c*.nc`.
    /// The trailing creation stamp is unpredictable, so the key is always
    /// listed and never constructed.
    pub gmgsi_bucket: Source,
    /// NWS public API: active alerts and zone geometry.
    pub nws_api_base: Source,
    /// Storm Prediction Center: outlooks, mesoscale discussions, storm reports.
    pub spc_base: Source,
    /// Iowa Environmental Mesonet: current ASOS/METAR observations.
    pub iem_base: Source,
    /// Where the eight bucket fields above are addressed — the one definition of
    /// the S3 URL shape, with `{bucket}` substituted by [`Self::s3_bucket_url`].
    pub s3_base: Source,
    /// Open-Meteo forecast API: environmental sounding heights (0 °C and
    /// −20 °C levels) per radar site. See `rustdar_radar::sounding`.
    pub sounding_base: Source,
    /// `false` in production: IEM answers `OPTIONS` with `405`, so a
    /// `User-Agent` makes the request preflighted and it never happens.
    pub metar_sends_user_agent: bool,
    /// `false` in production: SPC's CloudFront answers `OPTIONS` with `403` and
    /// no CORS headers, so a `User-Agent` makes outlooks, MDs and storm reports
    /// unreachable from the browser and only from the browser.
    pub spc_sends_user_agent: bool,
}

impl Default for DataSources {
    fn default() -> Self {
        Self::production()
    }
}

impl DataSources {
    /// The origins the shipped application uses. All answer `ACAO: *`.
    pub const fn production() -> Self {
        Self {
            level2_bucket: Cow::Borrowed("unidata-nexrad-level2"),
            level2_chunks_bucket: Cow::Borrowed("unidata-nexrad-level2-chunks"),
            level3_bucket: Cow::Borrowed("unidata-nexrad-level3"),
            hrrr_bucket: Cow::Borrowed("noaa-hrrr-bdp-pds"),
            goes_east_bucket: Cow::Borrowed("noaa-goes19"),
            goes_west_bucket: Cow::Borrowed("noaa-goes18"),
            mrms_bucket: Cow::Borrowed("noaa-mrms-pds"),
            gmgsi_bucket: Cow::Borrowed("noaa-gmgsi-pds"),
            nws_api_base: Cow::Borrowed("https://api.weather.gov"),
            spc_base: Cow::Borrowed("https://www.spc.noaa.gov"),
            iem_base: Cow::Borrowed("https://mesonet.agron.iastate.edu"),
            s3_base: Cow::Borrowed("https://{bucket}.s3.amazonaws.com"),
            sounding_base: Cow::Borrowed("https://api.open-meteo.com"),
            metar_sends_user_agent: false,
            spc_sends_user_agent: false,
        }
    }

    /// **Every DATA origin this application reads from, as URLs — the one
    /// enumeration of the set.**
    ///
    /// Eight bucket-object URLs (addressed through [`s3_base`](Self::s3_base),
    /// so a test pointing that field at a mock server moves these with it) and
    /// the four API bases. Callers that want hostnames take the host of each.
    ///
    /// It exists because two walkers used to restate this list independently —
    /// Android's `network_security_config` coverage pair and the service
    /// worker's never-cache declaration — which is precisely the drift they
    /// were written to catch. One list, two consumers.
    ///
    /// **Tile hosts are deliberately absent.** A basemap tile is cacheable and
    /// is meant to be cached; `sw.js` routes it by regex to a cache with a size
    /// cap, and it must never appear in a never-cache deny list. The Android
    /// config *does* need the tile subdomains, so that walker adds them itself
    /// — the difference is real, and keeping it out of the shared enumeration
    /// is what stops the reverse pin demanding tile hosts in `sw.js`.
    pub fn origin_urls(&self) -> Vec<String> {
        let buckets = [
            &self.level2_bucket,
            &self.level2_chunks_bucket,
            &self.level3_bucket,
            &self.hrrr_bucket,
            &self.goes_east_bucket,
            &self.goes_west_bucket,
            &self.mrms_bucket,
            &self.gmgsi_bucket,
        ];
        let bases = [
            &self.nws_api_base,
            &self.spc_base,
            &self.iem_base,
            &self.sounding_base,
        ];
        buckets
            .into_iter()
            .map(|bucket| self.s3_object_url(bucket, "k"))
            .chain(bases.into_iter().map(|base| base.to_string()))
            .collect()
    }

    /// Every METAR fetch goes through here, so the origin's recorded rule is the
    /// rule the request obeys.
    pub fn metar_client(&self, timeout: std::time::Duration) -> reqwest::ClientBuilder {
        crate::tls::client_for(self.metar_sends_user_agent, timeout)
    }

    /// For SPC outlooks, mesoscale discussions and storm reports.
    pub fn spc_client(&self, timeout: std::time::Duration) -> reqwest::ClientBuilder {
        crate::tls::client_for(self.spc_sends_user_agent, timeout)
    }

    /// Where one bucket lives, from [`s3_base`](Self::s3_base).
    pub fn s3_bucket_url(&self, bucket: &str) -> String {
        self.s3_base.replace("{bucket}", bucket)
    }

    /// `https://{bucket}.s3.amazonaws.com/{key}`.
    ///
    /// The key is interpolated, not encoded: every key rustdar builds is drawn from
    /// `[A-Za-z0-9_./-]`.
    pub fn s3_object_url(&self, bucket: &str, key: &str) -> String {
        format!("{}/{key}", self.s3_bucket_url(bucket))
    }

    /// Object URL for one Level III product file.
    pub fn level3_object_url(&self, key: &str) -> String {
        self.s3_object_url(&self.level3_bucket, key)
    }

    /// The flat key prefix for one site/product/day in the Level III bucket.
    ///
    /// The bucket has **no directory structure and no `sn.last`**: keys are
    /// `TLX_N0S_2026_07_25_01_20_27`, so "the latest product" is the last key of a
    /// prefix listing. `site3` is the **three**-letter code — `TLX`, not `KTLX`.
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
    pub fn hrrr_grib_url(
        &self,
        date: &chrono::NaiveDate,
        run_hour: u8,
        forecast_hour: u8,
    ) -> String {
        self.s3_object_url(
            &self.hrrr_bucket,
            &Self::hrrr_key(date, run_hour, forecast_hour),
        )
    }

    /// The `.idx` sidecar listing that GRIB2 file's records and byte offsets.
    /// ~9 KB, then a `Range:` request for the one record wanted.
    pub fn hrrr_idx_url(
        &self,
        date: &chrono::NaiveDate,
        run_hour: u8,
        forecast_hour: u8,
    ) -> String {
        format!("{}.idx", self.hrrr_grib_url(date, run_hour, forecast_hour))
    }

    /// The day directory holding one MRMS product's files.
    ///
    /// `CONUS/` and not `CONUS_5KM/`: the 5 km prefix exists in the bucket but
    /// **stops at ~2021-02-24**, so a reader who finds it while listing is
    /// looking at a dead product rather than a cheaper one.
    pub fn mrms_day_prefix(product: &str, date: &chrono::NaiveDate) -> String {
        format!("CONUS/{product}/{}/", date.format("%Y%m%d"))
    }

    /// The object key for one MRMS granule.
    ///
    /// The day directory is taken from `stamp` rather than passed separately:
    /// every key in the bucket files under the date its own timestamp carries,
    /// and two parameters could disagree.
    ///
    /// **Timestamps are not clock-aligned** — `000039`, `000242`, `000442`,
    /// `000641` across one observed hour — so this builds a key from a stamp
    /// somebody already read off a listing. Nothing may round a wall clock into
    /// it and expect an object to be there; see `mrms::fetch::latest_key`.
    pub fn mrms_key(product: &str, stamp: &chrono::NaiveDateTime) -> String {
        format!(
            "{}MRMS_{product}_{}.grib2.gz",
            Self::mrms_day_prefix(product, &stamp.date()),
            stamp.format("%Y%m%d-%H%M%S"),
        )
    }

    /// URL of one MRMS granule.
    pub fn mrms_object_url(&self, product: &str, stamp: &chrono::NaiveDateTime) -> String {
        self.s3_object_url(&self.mrms_bucket, &Self::mrms_key(product, stamp))
    }

    /// The hour directory holding one GMGSI channel's granule.
    ///
    /// An hour prefix and not a day one: each hour holds a single object, so
    /// listing the hour is one request that returns one key. There is no
    /// `gmgsi_key` counterpart on purpose -- the object name ends in a
    /// creation timestamp (`_c202506011234579`) that no clock can predict, so
    /// the key must come off a listing.
    pub fn gmgsi_hour_prefix(channel_prefix: &str, stamp: &chrono::NaiveDateTime) -> String {
        format!("{channel_prefix}/{}/", stamp.format("%Y/%m/%d/%H"))
    }

    /// Current ASOS observations for one US state, as JSON.
    ///
    /// Scoped to a state (~72 KB): the `?networkclass=ASOS` form is one request but
    /// **54 MB, ungzipped**.
    pub fn metar_state_url(&self, state: &str) -> String {
        format!("{}/api/1/currents.json?network={state}_ASOS", self.iem_base,)
    }

    /// Active NWS alerts, as GeoJSON.
    pub fn nws_alerts_url(&self) -> String {
        format!("{}/alerts/active?status=actual", self.nws_api_base)
    }

    /// Environmental sounding heights near one radar site, as JSON (~900 B).
    ///
    /// `freezing_level_height` is the 0 °C height directly; the four
    /// temperature/geopotential-height pressure-level pairs (600–300 hPa) are
    /// what `rustdar_radar::sounding::parse_env_heights` interpolates the −20 °C
    /// height from — that span brackets the −20 °C surface in every ordinary
    /// atmosphere (~−13 °C climatological mean at 600 hPa, ~−45 °C at 300).
    ///
    /// `forecast_hours=2` keeps the response at two hourly rows. Coordinates are
    /// truncated to three decimals (~110 m), far inside the model's grid spacing.
    pub fn sounding_url(&self, lat: f64, lon: f64) -> String {
        format!(
            "{}/v1/forecast?latitude={lat:.3}&longitude={lon:.3}\
             &hourly=freezing_level_height,\
             temperature_600hPa,geopotential_height_600hPa,\
             temperature_500hPa,geopotential_height_500hPa,\
             temperature_400hPa,geopotential_height_400hPa,\
             temperature_300hPa,geopotential_height_300hPa\
             &forecast_hours=2",
            self.sounding_base,
        )
    }

    /// For Open-Meteo soundings: no `User-Agent` is required, so the request stays
    /// simple and skips the preflight.
    pub fn sounding_client(&self, timeout: std::time::Duration) -> reqwest::ClientBuilder {
        crate::tls::simple_client(timeout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn date() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 7, 25).unwrap()
    }

    /// The three no-ACAO origins must not reappear in a production URL.
    #[test]
    fn no_production_origin_is_one_the_browser_cannot_reach() {
        let s = DataSources::production();
        let urls = [
            s.level3_object_url("TLX_N0S_2026_07_25_01_20_27"),
            s.hrrr_grib_url(&date(), 3, 0),
            s.hrrr_idx_url(&date(), 3, 0),
            s.metar_state_url("OK"),
            s.nws_alerts_url(),
            s.sounding_url(41.320, -96.367),
            s.s3_object_url(&s.level2_bucket, "k"),
            s.s3_object_url(&s.goes_east_bucket, "k"),
            s.s3_object_url(&s.goes_west_bucket, "k"),
            s.spc_base.to_string(),
        ];
        for url in urls {
            for blocked in [
                "tgftp.nws.noaa.gov",
                "nomads.ncep.noaa.gov",
                "aviationweather.gov",
            ] {
                assert!(
                    !url.contains(blocked),
                    "{url} still points at {blocked}, which sends no \
                     Access-Control-Allow-Origin and is unreachable from the web build",
                );
            }
            assert!(url.starts_with("https://"), "{url} is not https");
        }
    }

    /// The S3 origins are injectable like every other origin in this table.
    #[test]
    fn the_s3_origin_is_injectable_and_production_still_addresses_the_bucket_host() {
        let s = DataSources::production();
        assert_eq!(
            s.s3_bucket_url("noaa-goes19"),
            "https://noaa-goes19.s3.amazonaws.com",
        );
        assert_eq!(
            s.s3_object_url("noaa-goes19", "GLM-L2-LCFA/2026/225/01/OR_x.nc"),
            "https://noaa-goes19.s3.amazonaws.com/GLM-L2-LCFA/2026/225/01/OR_x.nc",
        );

        let local = DataSources {
            s3_base: "http://127.0.0.1:9/{bucket}".into(),
            ..DataSources::production()
        };
        assert_eq!(
            local.s3_object_url("noaa-goes19", "k"),
            "http://127.0.0.1:9/noaa-goes19/k",
            "an overridden base must carry the bucket and the key, or a test \
             server is serving somebody else's request",
        );
        assert_eq!(
            local.level3_object_url("TLX_N0S_2026_07_25_01_20_27"),
            "http://127.0.0.1:9/unidata-nexrad-level3/TLX_N0S_2026_07_25_01_20_27",
            "every S3 URL in the tree must come from this one definition, not \
             only the one the caller remembered to route",
        );
    }

    /// The network site catalogue reaches for two hosts, and **both are already
    /// here**. A genuinely new origin has to be added in four more places (service
    /// worker `NEVER_CACHE_HOSTS`, `network_security_config.xml`, `pwa_assets`,
    /// staging loop).
    #[test]
    fn no_new_origin_is_needed_for_the_catalogue() {
        let s = DataSources::production();
        assert_eq!(
            s.level2_chunks_bucket, "unidata-nexrad-level2-chunks",
            "the catalogue's membership half lists this bucket's root",
        );
        assert_eq!(
            s.nws_api_base, "https://api.weather.gov",
            "the catalogue's position half GETs {}/radar/stations",
            s.nws_api_base,
        );
        // `noaa-nexrad-level2` grants neither listing nor public GET, and the Google
        // mirror runs ~3.5 weeks behind with `.tar`-bundled volumes.
        for url in [
            s.s3_object_url(&s.level2_chunks_bucket, "KTLX/"),
            format!("{}/radar/stations", s.nws_api_base),
        ] {
            for rejected in ["noaa-nexrad-level2", "storage.googleapis.com"] {
                assert!(
                    !url.contains(rejected),
                    "{url} points at {rejected}, which the catalogue cannot use",
                );
            }
            assert!(url.starts_with("https://"), "{url} is not https");
        }
    }

    /// Level III keys are flat with no `sn.last`.
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

    /// The HRRR key layout, transcribed from a live listing.
    #[test]
    fn the_hrrr_key_names_a_run_and_a_forecast_hour() {
        assert_eq!(
            DataSources::hrrr_key(&date(), 3, 0),
            "hrrr.20260725/conus/hrrr.t03z.wrfsfcf00.grib2",
        );
        // f01 is where the UH accumulation window is nonzero; 14z is a two-digit run hour.
        assert_eq!(
            DataSources::hrrr_key(&date(), 14, 1),
            "hrrr.20260725/conus/hrrr.t14z.wrfsfcf01.grib2",
        );
    }

    /// The MRMS key layout, transcribed from a live listing on 2026-08-21.
    #[test]
    fn the_mrms_key_names_a_product_and_a_stamp() {
        let stamp = NaiveDate::from_ymd_opt(2026, 8, 21)
            .unwrap()
            .and_hms_opt(0, 0, 39)
            .unwrap();
        assert_eq!(
            DataSources::mrms_key("MergedReflectivityQCComposite_00.50", &stamp),
            "CONUS/MergedReflectivityQCComposite_00.50/20260821/\
             MRMS_MergedReflectivityQCComposite_00.50_20260821-000039.grib2.gz",
        );
        // The key lives under its own stamp's day, so the prefix a listing
        // walks and the key it hands back cannot name two different days.
        assert!(
            DataSources::mrms_key("PrecipRate_00.00", &stamp).starts_with(
                &DataSources::mrms_day_prefix("PrecipRate_00.00", &stamp.date())
            ),
        );
        // Zero-padded throughout: S3 prefix matching is bytewise, and the
        // seconds field is what makes these keys sort by time at all.
        let january = NaiveDate::from_ymd_opt(2026, 1, 5)
            .unwrap()
            .and_hms_opt(1, 2, 3)
            .unwrap();
        assert_eq!(
            DataSources::mrms_key("PrecipRate_00.00", &january),
            "CONUS/PrecipRate_00.00/20260105/MRMS_PrecipRate_00.00_20260105-010203.grib2.gz",
        );
        // Never `CONUS_5KM`: that prefix exists in the bucket and stops at
        // ~2021-02-24, so addressing it would read a four-year-old mosaic.
        assert!(!DataSources::mrms_day_prefix("PrecipRate_00.00", &january.date()).contains("5KM"),);
    }

    /// The index URL must be the GRIB URL plus `.idx` — a separate object.
    #[test]
    fn the_idx_url_is_the_grib_url_with_a_suffix() {
        let s = DataSources::production();
        assert_eq!(
            s.hrrr_idx_url(&date(), 3, 0),
            format!("{}.idx", s.hrrr_grib_url(&date(), 3, 0)),
        );
        assert!(
            s.hrrr_idx_url(&date(), 3, 0)
                .ends_with("wrfsfcf00.grib2.idx")
        );
    }

    /// `network=<ST>_ASOS`, never `networkclass=ASOS`: 72 KB vs 54 MB measured.
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

    /// The sounding query, pinned verbatim: the CORS probes were run against it.
    #[test]
    fn the_sounding_url_is_the_probed_query_shape() {
        let url = DataSources::production().sounding_url(41.320, -96.367);
        assert_eq!(
            url,
            "https://api.open-meteo.com/v1/forecast?latitude=41.320&longitude=-96.367\
             &hourly=freezing_level_height,\
             temperature_600hPa,geopotential_height_600hPa,\
             temperature_500hPa,geopotential_height_500hPa,\
             temperature_400hPa,geopotential_height_400hPa,\
             temperature_300hPa,geopotential_height_300hPa\
             &forecast_hours=2",
        );
    }

    /// Simple request, no `User-Agent`: Open-Meteo requires none.
    #[test]
    fn the_sounding_client_sends_no_user_agent() {
        let s = DataSources::production();
        let t = std::time::Duration::from_secs(1);
        assert!(
            !crate::tls::sends_user_agent(&s.sounding_client(t).build().expect("client")),
            "the sounding client carries a User-Agent; the fetch was probed \
             and shipped as a simple request",
        );
    }

    /// Production must not send a `User-Agent` to IEM or to SPC.
    #[test]
    fn production_keeps_preflight_hostile_origins_preflight_free() {
        let s = DataSources::production();
        assert!(
            !s.metar_sends_user_agent,
            "IEM answers 405 to OPTIONS; a User-Agent turns the GET into a \
             preflight and the request never happens in a browser",
        );
        assert!(
            !s.spc_sends_user_agent,
            "SPC answers 403 to OPTIONS with no CORS headers; a User-Agent \
             makes outlooks, MDs and storm reports unreachable from the web build",
        );
    }

    /// The rule must reach the client, not just sit in a field.
    #[test]
    fn the_preflight_hostile_origins_get_a_client_with_no_user_agent() {
        let s = DataSources::production();
        let t = std::time::Duration::from_secs(1);
        assert!(
            !crate::tls::sends_user_agent(&s.metar_client(t).build().expect("client")),
            "the METAR client carries a User-Agent; IEM's OPTIONS answers 405",
        );
        assert!(
            !crate::tls::sends_user_agent(&s.spc_client(t).build().expect("client")),
            "the SPC client carries a User-Agent; SPC's OPTIONS answers 403",
        );
    }

    /// Counterweight: a `metar_client` that ignored its field would otherwise pass.
    #[test]
    fn flipping_the_preflight_rule_changes_the_client() {
        let t = std::time::Duration::from_secs(1);
        let metar = DataSources {
            metar_sends_user_agent: true,
            ..DataSources::production()
        };
        let spc = DataSources {
            spc_sends_user_agent: true,
            ..DataSources::production()
        };
        assert!(
            crate::tls::sends_user_agent(&metar.metar_client(t).build().expect("client")),
            "metar_client does not read metar_sends_user_agent",
        );
        assert!(
            crate::tls::sends_user_agent(&spc.spc_client(t).build().expect("client")),
            "spc_client does not read spc_sends_user_agent",
        );
    }

    /// Overriding one field must not disturb the others.
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
