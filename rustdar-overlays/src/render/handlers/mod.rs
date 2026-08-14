// The polygon-kind modules and `reports` are `pub` because their fetch-result
// types are: `rustdar-frontend`'s described-job dispatch tests seed a live
// registry through `apply_fetch_result`, and the payload type has to be
// nameable where the test constructs it. (`glm` needs no widening for the
// same tests: its payload type, `crate::glm::GlmFetchResult`, is already
// public where it lives.) Everything else in them keeps its own visibility.
pub mod alert;
mod colorscale;
pub mod discussion;
mod glm;
mod labels;
mod location;
mod metar;
mod model;
pub mod outlook;
mod radar;
pub mod reports;
mod sites;

/// Invariants over *every* texture handler at once. Here rather than beside one
/// of them because the thing being checked is the set: a new overlay that
/// declares the wrong alpha convention is the failure, and no single handler's
/// test module can see it.
#[cfg(test)]
mod texture_tests;

use super::overlay_state::OverlayHandler;

/// Create the default set of overlay handlers.
pub(crate) fn create_handlers() -> Vec<Box<dyn OverlayHandler>> {
    vec![
        Box::new(model::ModelDataHandler::new()),
        Box::new(radar::RadarHandler::new()),
        Box::new(outlook::SpcOutlookHandler::new()),
        Box::new(discussion::SpcDiscussionHandler::new()),
        Box::new(alert::NwsAlertHandler::new()),
        Box::new(reports::StormReportsHandler::new()),
        Box::new(glm::GlmHandler::new()),
        Box::new(metar::MetarHandler::new()),
        Box::new(labels::CityLabelsHandler::new()),
        Box::new(sites::RadarSitesHandler::new()),
        Box::new(location::UserLocationHandler::new()),
        Box::new(colorscale::ColorScaleHandler::new()),
    ]
}

/// The one step of the coverage guarantee the compiler cannot take on its own.
///
/// `OverlayState::downcast_round` unifies the round type's declared
/// [`RoundShape`](crate::fetch_policy::RoundShape) with the layer's, and that
/// unification is the link that makes an assembled round unable to reach
/// `set_data`. A handler that spells the downcast itself —
/// `result.downcast::<MyFetchResult>()`, which is exactly how all seven of
/// these were written before — steps around the link, keeps whatever shape it
/// felt like declaring, and is back to the silence with nothing complaining.
///
/// Not reachable by the type system: `FetchPayload` is a `Box<dyn Any>` and
/// `Any` will always downcast for anybody who asks. Sealing it is not available
/// either — the same alias carries pane state, which `rustdar-egui` downcasts
/// on its own side. So this is checked over the source of every handler
/// instead, which is small, exhaustive over the set, and fails loudly.
#[cfg(test)]
mod round_delivery_tests {
    /// Every handler file, whether or not it fetches today: the one that
    /// reintroduces this is by definition the one nobody has read yet.
    const HANDLER_SOURCES: [(&str, &str); 12] = [
        ("alert", include_str!("alert.rs")),
        ("colorscale", include_str!("colorscale.rs")),
        ("discussion", include_str!("discussion.rs")),
        ("glm", include_str!("glm.rs")),
        ("labels", include_str!("labels.rs")),
        ("location", include_str!("location.rs")),
        ("metar", include_str!("metar.rs")),
        ("model", include_str!("model.rs")),
        ("outlook", include_str!("outlook.rs")),
        ("radar", include_str!("radar.rs")),
        ("reports", include_str!("reports.rs")),
        ("sites", include_str!("sites.rs")),
    ];

    /// `apply_fetch_result`'s body, from its signature to the next item at the
    /// same indent, for a handler that **has** a round to take delivery of.
    ///
    /// `None` for the five layers that never fetch: their bodies are
    /// `fn apply_fetch_result(&mut self, _result: FetchPayload) {}`, and a
    /// payload bound to `_result` is a payload nothing is ever done with. That
    /// is the test's own way of counting which handlers are in scope, so it
    /// cannot be satisfied by a handler quietly leaving the set.
    ///
    /// Scoped to that one function rather than the whole file so a future
    /// handler downcasting its **pane state** — a different payload, with no
    /// coverage question in it — is not caught by this.
    fn round_delivery_body(src: &str) -> Option<&str> {
        let start = src.find("fn apply_fetch_result(&mut self, result: FetchPayload)")?;
        let rest = &src[start..];
        Some(match rest.find("\n    fn ") {
            Some(end) => &rest[..end],
            None => rest,
        })
    }

    /// A handler in a file nobody listed above is a handler nobody checked, so
    /// the list is checked against this module's own `mod` lines.
    ///
    /// Reading `mod.rs` back out of itself rather than trusting a hand-kept
    /// number: adding a handler means adding a `mod`, and the whole point of
    /// this test module is that the case which reintroduces the defect is the
    /// one written after everybody stopped looking.
    #[test]
    fn every_handler_module_is_on_the_delivery_list() {
        let src = include_str!("mod.rs");
        let declarations = src
            .split("#[cfg(test)]")
            .next()
            .expect("split always yields a first piece")
            .lines()
            .filter(|line| {
                line.starts_with("mod ")
                    || line.starts_with("pub(crate) mod ")
                    || line.starts_with("pub mod ")
            })
            .count();
        assert_eq!(
            declarations,
            HANDLER_SOURCES.len(),
            "a handler module is declared here and not listed in \
             HANDLER_SOURCES, so nothing checks how it takes delivery of its \
             round",
        );
    }

    #[test]
    fn no_handler_takes_delivery_of_its_round_by_hand() {
        let mut checked = 0;
        for (name, src) in HANDLER_SOURCES {
            let Some(body) = round_delivery_body(src) else {
                continue;
            };
            checked += 1;
            for spelling in [".downcast::<", ".downcast_ref::<", ".downcast_mut::<"] {
                assert!(
                    !body.contains(spelling),
                    "the {name} handler reaches for `{spelling}` on its own \
                     fetch result. That skips `OverlayState::downcast_round`, \
                     which is the only place the round type's declared shape is \
                     checked against the layer's — and skipping it is how a \
                     round assembled from several requests gets its `set_data` \
                     back",
                );
            }
            assert!(
                body.contains("downcast_round::<"),
                "the {name} handler has an `apply_fetch_result` that takes \
                 delivery of its round some other way",
            );
        }
        assert_eq!(
            checked, 7,
            "seven handlers fetch; a handler that started or stopped fetching \
             must be accounted for here rather than silently skipped",
        );
    }
}
