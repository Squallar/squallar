//! Keeps the Android Network Security Config in step with the origins squallar
//! fetches from.
//!
//! `.../res/xml/network_security_config.xml` denies cleartext per domain over
//! a `base-config` that *permits* it, because Android's own TrustManager
//! fetches the plain-HTTP CRL published by `api.weather.gov`'s Let's Encrypt
//! chain — so an origin missing from the per-domain list travels in the clear.
//!
//! The assertions run both ways: a live origin with no XML entry fails, and an
//! XML entry no origin needs fails too. Not under `cfg(android)`, because the
//! `android` module tree compiles to nothing on the host.

#[cfg(test)]
const CONFIG_PATH: &str = "../packaging/android/app/src/main/res/xml/network_security_config.xml";

#[cfg(test)]
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct DomainRule {
    host: String,
    include_subdomains: bool,
}

#[cfg(test)]
impl DomainRule {
    /// Exact match always, subdomains only with `includeSubdomains`.
    fn covers(&self, host: &str) -> bool {
        host == self.host || (self.include_subdomains && host.ends_with(&format!(".{}", self.host)))
    }
}

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
    use squallar_source::origins::DataSources;
    use std::collections::BTreeSet;

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

    /// Every host the shipped app requests, derived from the declarations.
    fn live_hosts() -> BTreeSet<String> {
        let s = DataSources::production();
        let mut hosts = BTreeSet::new();

        // The one enumeration of the data origins, shared with the service
        // worker's deny-list pair. Restating it here is what let the two drift.
        for url in s.origin_urls() {
            hosts.insert(host_of(&url));
        }

        // The basemap and terrain archives are this walker's own addition:
        // they are not data origins, and `sw.js` routes them "network" by its
        // own explicit rule. Read from the client's consts, so a regenerated
        // archive that moves hosts fails here rather than in the field.
        hosts.insert(host_of(squallar_egui::tiles::BASEMAP_ARCHIVE_URL));
        hosts.insert(host_of(squallar_egui::tiles::TERRAIN_ARCHIVE_URL));

        // The attribution link the map footer opens.
        hosts.insert(host_of(squallar_egui::tiles::ATTRIBUTION_URL));

        // The NWS zone pack, downloaded once from the web deploy and kept
        // beside the zone cache. Read from the entry points' const, so a
        // moved deploy fails here rather than in the field.
        hosts.insert(host_of(crate::platform::ZONE_PACK_URL));

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

    /// Android Lint's "Missing includeSubdomains" is a *fatal* error: an entry
    /// without it fails `lintVitalRelease`.
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

    /// Inverting either half breaks HTTPS to api.weather.gov or allows plaintext.
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
