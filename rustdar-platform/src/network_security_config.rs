//! Keeps the Android Network Security Config in step with the origins rustdar
//! fetches from.
//!
//! `.../res/xml/network_security_config.xml` denies cleartext per domain over a
//! `base-config` that *permits* it, because Android's own TrustManager fetches
//! the plain-HTTP CRL published by `api.weather.gov`'s Let's Encrypt chain. So
//! an origin missing from the per-domain list falls through to `base-config`
//! and is allowed to travel in the clear.
//!
//! The assertions run both ways: a live origin with no XML entry fails, and an
//! XML entry no origin needs fails too. One-way would have missed the stale
//! `aviationweather.gov` entry, and it was the stale entry that made the list
//! look maintained while the live METAR host went uncovered.
//!
//! Not in `rustdar-android`: that crate is `#![cfg(target_os = "android")]` and
//! compiles to nothing on the host, so a test in it never runs in CI.

#[cfg(test)]
const CONFIG_PATH: &str =
    "../rustdar-android/android/app/src/main/res/xml/network_security_config.xml";

/// One `<domain>` entry.
#[cfg(test)]
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct DomainRule {
    host: String,
    include_subdomains: bool,
}

#[cfg(test)]
impl DomainRule {
    /// Android's rule: exact match always, subdomains only with
    /// `includeSubdomains`.
    fn covers(&self, host: &str) -> bool {
        host == self.host || (self.include_subdomains && host.ends_with(&format!(".{}", self.host)))
    }
}

/// Pull every `<domain …>host</domain>` out of the config.
#[cfg(test)]
fn parse_domains(xml: &str) -> Vec<DomainRule> {
    let mut out = Vec::new();
    let mut rest = xml;

    // Strip comments first: they name hosts, which would parse as rules.
    let mut stripped = String::with_capacity(xml.len());
    while let Some(start) = rest.find("<!--") {
        stripped.push_str(&rest[..start]);
        let after = &rest[start + 4..];
        match after.find("-->") {
            Some(end) => rest = &after[end + 3..],
            None => {
                rest = "";
                break;
            }
        }
    }
    stripped.push_str(rest);

    let mut rest = stripped.as_str();
    while let Some(start) = rest.find("<domain") {
        let after = &rest[start + "<domain".len()..];
        // Guard against matching `<domain-config`.
        if after.starts_with('-') {
            rest = after;
            continue;
        }
        let Some(gt) = after.find('>') else { break };
        let attrs = &after[..gt];
        let body = &after[gt + 1..];
        let Some(close) = body.find("</domain>") else {
            break;
        };

        out.push(DomainRule {
            host: body[..close].trim().to_string(),
            include_subdomains: attrs.contains("includeSubdomains=\"true\""),
        });
        rest = &body[close..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdar_radar::sources::DataSources;
    use std::collections::BTreeSet;
    use walkers::sources::TileSource;

    fn config_xml() -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(CONFIG_PATH);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
    }

    fn host_of(url: &str) -> String {
        url.split_once("://")
            .map(|(_, rest)| rest)
            .unwrap_or(url)
            .split(['/', ':'])
            .next()
            .unwrap()
            .to_ascii_lowercase()
    }

    /// Every host the shipped app requests, derived from the declarations
    /// rather than restated — that is the whole point.
    fn live_hosts() -> BTreeSet<String> {
        let s = DataSources::production();
        let mut hosts = BTreeSet::new();

        for bucket in [
            &s.level2_bucket,
            &s.level2_chunks_bucket,
            &s.level3_bucket,
            &s.hrrr_bucket,
            &s.goes_east_bucket,
            &s.goes_west_bucket,
        ] {
            hosts.insert(host_of(&s.s3_object_url(bucket, "k")));
        }

        for base in [&s.nws_api_base, &s.spc_base, &s.iem_base, &s.sounding_base] {
            hosts.insert(host_of(base));
        }

        // `tile_url` picks a subdomain from `x % 4`, so walk all four.
        for style in [
            rustdar_egui::tiles::CartoDb::light(),
            rustdar_egui::tiles::CartoDb::dark(),
            rustdar_egui::tiles::CartoDb::light_labels(),
            rustdar_egui::tiles::CartoDb::dark_labels(),
        ] {
            for x in 0..4u32 {
                let id = walkers::TileId { x, y: 0, zoom: 4 };
                hosts.insert(host_of(&style.tile_url(id)));
            }
            hosts.insert(host_of(style.attribution().url));
        }

        hosts
    }

    #[test]
    fn parser_reads_hosts_and_the_include_subdomains_flag() {
        let rules = parse_domains(
            r#"<network-security-config>
                 <!-- a comment naming decoy.example.com -->
                 <domain-config cleartextTrafficPermitted="false">
                   <domain includeSubdomains="true">weather.gov</domain>
                   <domain>exact.example.com</domain>
                 </domain-config>
               </network-security-config>"#,
        );

        assert_eq!(
            rules,
            vec![
                DomainRule {
                    host: "weather.gov".into(),
                    include_subdomains: true
                },
                DomainRule {
                    host: "exact.example.com".into(),
                    include_subdomains: false
                },
            ],
            "a <domain-config> wrapper and a host named only in a comment must \
             not be read as rules"
        );
    }

    #[test]
    fn include_subdomains_matches_subdomains_but_not_a_suffix_of_the_label() {
        let wide = DomainRule {
            host: "weather.gov".into(),
            include_subdomains: true,
        };
        assert!(wide.covers("weather.gov"));
        assert!(wide.covers("api.weather.gov"));
        assert!(
            !wide.covers("notweather.gov"),
            "matching must be on label boundaries, not raw string suffix"
        );

        let exact = DomainRule {
            host: "mesonet.agron.iastate.edu".into(),
            include_subdomains: false,
        };
        assert!(exact.covers("mesonet.agron.iastate.edu"));
        assert!(!exact.covers("other.mesonet.agron.iastate.edu"));
    }

    #[test]
    fn every_live_origin_is_covered_by_the_network_security_config() {
        let rules = parse_domains(&config_xml());
        let missing: Vec<_> = live_hosts()
            .into_iter()
            .filter(|h| !rules.iter().any(|r| r.covers(h)))
            .collect();

        assert!(
            missing.is_empty(),
            "these origins are fetched but not listed in \
             network_security_config.xml, so cleartext to them falls back to \
             base-config (which permits it): {missing:?}"
        );
    }

    #[test]
    fn the_network_security_config_lists_nothing_unused() {
        let hosts = live_hosts();
        let unused: Vec<_> = parse_domains(&config_xml())
            .into_iter()
            .filter(|r| !hosts.iter().any(|h| r.covers(h)))
            .map(|r| r.host)
            .collect();

        assert!(
            unused.is_empty(),
            "network_security_config.xml lists domains no DataSources origin or \
             tile source resolves to; drop them or update DataSources: {unused:?}"
        );
    }

    /// Android Lint's `NetworkSecurityConfig` / "Missing includeSubdomains" is
    /// a *fatal* error: an entry without it fails `lintVitalRelease` and takes
    /// `assembleRelease` with it.
    #[test]
    fn every_domain_entry_sets_include_subdomains() {
        let narrow: Vec<_> = parse_domains(&config_xml())
            .into_iter()
            .filter(|r| !r.include_subdomains)
            .map(|r| r.host)
            .collect();

        assert!(
            narrow.is_empty(),
            "these <domain> entries omit includeSubdomains=\"true\", which is a \
             fatal Android Lint error under lintVitalRelease: {narrow:?}"
        );
    }

    /// Inverting either half silently breaks HTTPS to api.weather.gov or
    /// silently allows plaintext to rustdar's own origins.
    #[test]
    fn base_permits_cleartext_and_every_domain_block_denies_it() {
        let xml = config_xml();

        assert!(
            xml.contains(r#"<base-config cleartextTrafficPermitted="true">"#),
            "base-config must permit cleartext: Android's TrustManager fetches \
             api.weather.gov's CRL over plain HTTP, and blocking it app-wide \
             breaks certificate verification rather than hardening it"
        );
        assert_eq!(
            xml.matches("<domain-config").count(),
            xml.matches(r#"<domain-config cleartextTrafficPermitted="false">"#)
                .count(),
            "every <domain-config> must set cleartextTrafficPermitted=\"false\""
        );
    }
}
