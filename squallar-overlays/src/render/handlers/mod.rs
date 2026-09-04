// The polygon-kind modules, `reports` and `sites` are `pub` because their
// fetch-result types are: `squallar-app`'s described-job dispatch tests
// construct the payload type, and `sites` is the layer the frontend installs
// the radar table into. Everything else in them keeps its own visibility.
pub mod alert;
mod colorscale;
mod coverage;
pub mod discussion;
pub mod firewx;
// `pub(crate)` for one reason: the GLM poll's own test module drives
// `GlmHandler::create_fetch_tasks` against a loopback bucket, which is the only
// place the depicted instant is observed crossing from the render context into
// the fetch. Narrower than the `pub` rows above; nothing outside this crate.
pub(crate) mod glm;
mod gmgsi;
mod labels;
mod location;
mod metar;
mod model;
mod mrms;
pub mod outlook;
pub mod reports;
pub mod sites;

#[cfg(test)]
mod texture_tests;

use super::overlay_state::OverlayHandler;
use squallar_source::id::{LayerId, known};

/// **The host bytes a gridded layer's handler keeps its decoded source
/// under** — the key-space grid budget each handler states for itself on
/// this build's arm: MRMS's `GRID_CACHE_BYTES`, GMGSI's `GRID_CACHE_BYTES`,
/// the model layer's `MODEL_GRID_BUDGET_BYTES`. Zero for every other layer:
/// a few hundred parsed polygons or station reports do not move a figure
/// read in megabytes, which is the same claim `resident_source_bytes` makes
/// through its default.
///
/// A budget, not a residency — what a pane enabling the layer asks the heap
/// to be able to hold, so the budget system can price the layer before a
/// grid has arrived and without reaching the handler instance (which lives on
/// the UI layer's registry, behind a coupling ceiling). Keyed by id and spelled
/// here, beside the registrations, so a gridded layer added to [`sources`]
/// without a row here is a review question in one file.
pub fn source_grid_budget_bytes(id: &LayerId) -> u64 {
    if *id == known::MRMS {
        crate::mrms::GRID_CACHE_BYTES as u64
    } else if *id == known::GMGSI {
        gmgsi::GRID_CACHE_BYTES as u64
    } else if *id == known::MODEL_DATA {
        model::MODEL_GRID_BUDGET_BYTES as u64
    } else {
        0
    }
}

/// **This crate's layer registrations — fifteen rows, and the only place they
/// are named.**
///
/// Radar is not here: it lives in `squallar_radar::sources`, and the app's whole
/// layer set is `squallar_egui::sources::all`. Adding an overlay means one row
/// here plus a `known::` const and a `LAYER_ID_LEDGER` entry.
pub fn sources() -> Vec<Box<dyn OverlayHandler>> {
    vec![
        Box::new(model::ModelDataHandler::new()),
        Box::new(mrms::MrmsHandler::new()),
        Box::new(gmgsi::GmgsiHandler::new()),
        Box::new(outlook::SpcOutlookHandler::new()),
        Box::new(firewx::SpcFireOutlookHandler::new()),
        Box::new(discussion::SpcDiscussionHandler::new()),
        Box::new(alert::NwsAlertHandler::new()),
        Box::new(reports::StormReportsHandler::new()),
        Box::new(glm::GlmHandler::new()),
        Box::new(metar::MetarHandler::new()),
        Box::new(labels::CityLabelsHandler::new()),
        Box::new(coverage::RadarCoverageHandler::new()),
        Box::new(sites::RadarSitesHandler::new()),
        Box::new(location::UserLocationHandler::new()),
        Box::new(colorscale::ColorScaleHandler::new()),
    ]
}

/// The one step of the coverage guarantee the compiler cannot take on its own.
///
/// `OverlayState::downcast_round` unifies the round type's declared
/// [`RoundShape`](crate::fetch_policy::RoundShape) with the layer's, which is
/// what makes an assembled round unable to reach `set_data`. A handler that
/// spells the downcast itself steps around the link, and the type system cannot
/// stop it: `FetchPayload` is a `Box<dyn Any>`.
#[cfg(test)]
mod round_delivery_tests {
    /// Every handler file, whether or not it fetches today: the one that
    /// reintroduces this is by definition the one nobody has read yet.
    const HANDLER_SOURCES: [(&str, &str); 15] = [
        ("alert", include_str!("alert.rs")),
        ("colorscale", include_str!("colorscale.rs")),
        ("coverage", include_str!("coverage.rs")),
        ("discussion", include_str!("discussion.rs")),
        ("firewx", include_str!("firewx.rs")),
        ("glm", include_str!("glm.rs")),
        ("gmgsi", include_str!("gmgsi.rs")),
        ("labels", include_str!("labels.rs")),
        ("location", include_str!("location.rs")),
        ("metar", include_str!("metar.rs")),
        ("model", include_str!("model.rs")),
        ("mrms", include_str!("mrms.rs")),
        ("outlook", include_str!("outlook.rs")),
        ("reports", include_str!("reports.rs")),
        ("sites", include_str!("sites.rs")),
    ];

    /// `apply_fetch_result`'s body, from its signature to the next item at the
    /// same indent, for a handler that **has** a round to take delivery of.
    ///
    /// `None` for the five layers that never fetch: their bodies bind the payload
    /// to `_result`. Scoped to that one function so a handler downcasting its
    /// **pane state** is not caught by this.
    fn round_delivery_body(src: &str) -> Option<&str> {
        let start = src.find("fn apply_fetch_result(&mut self, result: FetchPayload")?;
        let rest = &src[start..];
        Some(match rest.find("\n    fn ") {
            Some(end) => &rest[..end],
            None => rest,
        })
    }

    /// A handler in a file nobody listed above is a handler nobody checked, so
    /// the list is checked against this module's own `mod` lines.
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
        // **Twelve**, of which `sites` and `coverage` are the odd two: neither
        // builds a fetch task at all, but the frontend installs the radar table
        // through the same arrival door and hands it to both, so each takes
        // delivery of a round like the ten that do fetch. A handler that started
        // or stopped taking delivery must be accounted for here rather than
        // silently skipped.
        assert_eq!(
            checked, 12,
            "twelve handlers take delivery of a round; a handler that started \
             or stopped must be accounted for here rather than silently \
             skipped",
        );
    }
}

#[cfg(test)]
mod grid_budget_tests {
    use super::*;

    /// **Each gridded layer answers the constant its handler holds its cache
    /// to, and nothing else answers at all.** The three figures are whole
    /// grids of the layer's own shape — the same property the handlers'
    /// compile-time assertions hold — so a pane pricing a layer prices what
    /// the handler would actually hold at its key space.
    #[test]
    fn a_gridded_layer_answers_its_handlers_budget_and_the_rest_answer_zero() {
        let mrms = source_grid_budget_bytes(&known::MRMS);
        assert_eq!(mrms, crate::mrms::GRID_CACHE_BYTES as u64);
        assert!(mrms >= crate::mrms::CONUS_GRID_BYTES as u64);
        assert_eq!(mrms % crate::mrms::CONUS_GRID_BYTES as u64, 0);

        let gmgsi = source_grid_budget_bytes(&known::GMGSI);
        assert_eq!(gmgsi, gmgsi::GRID_CACHE_BYTES as u64);
        assert!(gmgsi >= gmgsi::GLOBAL_GRID_BYTES as u64);
        assert_eq!(gmgsi % gmgsi::GLOBAL_GRID_BYTES as u64, 0);

        let hrrr = source_grid_budget_bytes(&known::MODEL_DATA);
        assert_eq!(hrrr, model::MODEL_GRID_BUDGET_BYTES as u64);
        assert!(hrrr >= model::HRRR_CONUS_GRID_BYTES as u64);

        for id in [
            known::RADAR,
            known::METAR,
            known::NWS_ALERTS,
            known::LIGHTNING,
            known::CITY_LABELS,
            known::SPC_OUTLOOK,
        ] {
            assert_eq!(source_grid_budget_bytes(&id), 0, "{}", id.as_str());
        }
    }

    /// Every registered handler whose source is a grid has a row above: the
    /// handlers that report source bytes are exactly the ones priced here.
    /// A gridded layer registered without a row would be priced at one
    /// picture, which is the undercount this function exists to end.
    #[test]
    fn every_registered_gridded_layer_is_priced() {
        let priced: Vec<LayerId> = sources()
            .iter()
            .map(|h| h.id().clone())
            .filter(|id| source_grid_budget_bytes(id) > 0)
            .collect();
        assert_eq!(priced, vec![known::MODEL_DATA, known::MRMS, known::GMGSI]);
    }
}
