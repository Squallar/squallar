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
pub(crate) mod metar;
mod model;
mod mrms;
pub mod outlook;
pub mod reports;
pub mod sites;

#[cfg(test)]
mod texture_tests;

#[cfg(test)]
mod dispatch_walk_tests;

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
///
/// **The cache alone.** What a gridded handler holds *beside* the cache while a
/// loop of its layer runs is [`source_grid_staging_bytes`], and the two are
/// summed by the caller rather than folded together here — see that function
/// for why one figure cannot carry both.
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

/// **The host bytes a gridded layer's handler keeps BESIDE its cache while a
/// loop of the layer runs** — the second population
/// [`source_grid_budget_bytes`] does not reach, and which every gridded
/// handler's own `resident_source_bytes` already sums.
///
/// Two blocks per staging layer, each read off the constant that owns it:
///
/// * the handler's **frame-granule cache**, at its `FRAME_STAGING_BYTES`
///   budget — one granule staged at a time, however many frames the loop
///   holds, because a loop frame's storage is its texture and the granule is
///   only what one passes through on the way to being one;
/// * the **retained decode buffer** [`crate::staging::StagingPool`] parks
///   between granules, at the nominal shape that pool was declared with. It is
///   a second live block whatever the frame cache is holding: the slot is full
///   exactly when the cache is not.
///
/// The model layer stages **one grid and no pool**: its frame cache is
/// `model::MODEL_FRAME_STAGING_BYTES`, and it retains no decode buffer between
/// grids because a GRIB2 record is decoded once into a fresh values vector
/// rather than out of a recycled block. It answered zero until its loop frames
/// were staged, when a frame was a key in its live cache and the second
/// population genuinely did not exist.
///
/// **Why this is not a multiplier over [`source_grid_budget_bytes`].** MRMS's
/// grid is half its cache budget and GMGSI's is a quarter of its, so no
/// coefficient over `budget_bytes` is the same statement on both layers — and
/// one written where the budget system reads it would be a figure spelled as
/// arithmetic over this crate's private constants, which re-derives silently
/// the day one of them moves. Each term below names its own owner instead, so a
/// width or a shape that changes carries this figure with it.
pub fn source_grid_staging_bytes(id: &LayerId) -> u64 {
    if *id == known::MRMS {
        (crate::mrms::FRAME_STAGING_BYTES
            + crate::mrms::staging::STAGING_POINTS
                * crate::mrms::staging::StagingPool::ELEMENT_BYTES) as u64
    } else if *id == known::GMGSI {
        (gmgsi::FRAME_STAGING_BYTES
            + crate::gmgsi::staging::STAGING_POINTS
                * crate::gmgsi::staging::StagingPool::ELEMENT_BYTES) as u64
    } else if *id == known::MODEL_DATA {
        model::MODEL_FRAME_STAGING_BYTES as u64
    } else {
        0
    }
}

/// **Every layer that keeps a decoded source grid, with its budget** — the
/// three rows [`source_grid_budget_bytes`] answers non-zero for, enumerated
/// so a caller that has to price a layer nothing is showing yet can reach
/// them without walking a registry it does not own.
///
/// Kept beside that function on purpose: the two are the same three names,
/// and a fourth gridded layer that landed in one and not the other would make
/// the admission door blind to exactly the largest term it exists to price.
pub fn gridded_layers() -> [(LayerId, u64); 3] {
    [
        (known::MRMS, source_grid_budget_bytes(&known::MRMS)),
        (known::GMGSI, source_grid_budget_bytes(&known::GMGSI)),
        (
            known::MODEL_DATA,
            source_grid_budget_bytes(&known::MODEL_DATA),
        ),
    ]
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
        // **The tiled CEILING**, which is what a whole grid of this layer's
        // shape can cost since the mosaic stopped being a flat plane. The flat
        // figure would still divide this today — it is 99 % of it — and would
        // go on dividing it if the ceiling stopped covering a granule, which
        // is the failure this row is here to catch.
        assert!(mrms >= crate::mrms::CONUS_TILED_CEILING_BYTES as u64);
        assert_eq!(mrms % crate::mrms::CONUS_TILED_CEILING_BYTES as u64, 0);

        // **Measured in whole GRANULES**, not in point counts: a cache entry
        // is the codes plus the absent set beside them, and the two were one
        // constant while this row read `GLOBAL_GRID_BYTES` — so it could not
        // have failed on a budget four granules did not fit inside.
        let gmgsi = source_grid_budget_bytes(&known::GMGSI);
        assert_eq!(gmgsi, gmgsi::GRID_CACHE_BYTES as u64);
        assert!(gmgsi >= gmgsi::GLOBAL_GRANULE_BYTES as u64);
        assert_eq!(gmgsi % gmgsi::GLOBAL_GRANULE_BYTES as u64, 0);

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

    /// **The staging figure is each layer's own grids, and it is no ratio of
    /// that layer's cache budget.**
    ///
    /// The first half is the property: for the mosaics, one staged granule
    /// plus one retained decode buffer, each of the layer's own shape; for the
    /// model, one staged grid and no pool, because a GRIB2 record is decoded
    /// into a fresh values vector rather than out of a recycled block. The
    /// second half is the reason the figure has to exist separately at all —
    /// no one coefficient over `budget_bytes` produces all three, because the
    /// three layers budget two grids, four, and a pane set.
    #[test]
    fn a_staging_layer_answers_its_own_grids_and_no_ratio_of_its_budget() {
        let mrms = source_grid_staging_bytes(&known::MRMS);
        assert_eq!(mrms, 2 * crate::mrms::CONUS_GRID_BYTES as u64);

        // **GMGSI's two halves are two different figures**, and asserting
        // them apart is the point. The frame cache has to hold a whole
        // granule — codes plus the absent set beside them — while the pool
        // parks the codes `Vec` alone, because the absent set is a separate
        // allocation `staging` never receives. They were equal while
        // `GLOBAL_GRID_BYTES` stood in for both, so this row read
        // `2 * GLOBAL_GRID_BYTES` and could not have failed on a granule the
        // budget did not cover.
        let gmgsi_bytes = source_grid_staging_bytes(&known::GMGSI);
        assert_eq!(
            gmgsi_bytes,
            (gmgsi::GLOBAL_GRANULE_BYTES
                + crate::gmgsi::staging::STAGING_POINTS
                    * crate::gmgsi::staging::StagingPool::ELEMENT_BYTES) as u64,
            "the staged granule at its real size, plus the block the pool \
             parks — a sum of two unequal terms, not twice either of them. \
             That they ARE unequal is a `const _` beside \
             `GLOBAL_GRANULE_BYTES`: a runtime assertion over two constants is \
             one clippy can see cannot fail and a reader cannot",
        );
        // The two ratios differ, which is the whole reason for a second
        // function: on GMGSI the staged half is one granule of a four-granule
        // cache while the pool half is not a fraction of that budget at all,
        // and on MRMS the two figures are now in **different units** — the
        // staging pair is two flat PLANES, because a decode still reads one
        // and the frame cache still passes one through, while the cache
        // budget is two TILED grids. They were equal while both were the flat
        // plane, and a reader who took that for a rule would be reading a
        // coincidence.
        assert_ne!(mrms, source_grid_budget_bytes(&known::MRMS));
        assert_eq!(
            mrms,
            2 * crate::mrms::CONUS_GRID_BYTES as u64,
            "the staging pair is planes, and a plane is what a decode reads",
        );
        assert_eq!(
            gmgsi::GLOBAL_GRANULE_BYTES as u64 * 4,
            source_grid_budget_bytes(&known::GMGSI),
            "the GMGSI cache budget is four whole granules",
        );

        // The model stages ONE grid, not two: it retains no decode pool,
        // because a GRIB2 record is decoded into a fresh values vector rather
        // than out of a recycled block.
        //
        // Deliberately **not** also asserted against the model's own cache
        // budget. `staging < budget` reads like the interesting half and
        // cannot fail: the `const _` floor beside `MODEL_GRID_BUDGET_BYTES`
        // already holds every arm at six grids or more, so one grid is under
        // it on every target by construction. Tampered to confirm — setting
        // this row to the whole budget reddens the equality above and the
        // comparison never gets a turn. A conjunct that cannot fail is worse
        // than no conjunct, so the budget relation is left where it is
        // actually enforced.
        assert_eq!(
            source_grid_staging_bytes(&known::MODEL_DATA),
            model::HRRR_CONUS_GRID_BYTES as u64,
            "the model's staging area is not one CONUS grid. Two would mean \
             it had grown a retained pool like the mosaics'; zero means its \
             loop frames are charged to the live cache again",
        );

        for id in [
            known::RADAR,
            known::METAR,
            known::NWS_ALERTS,
            known::LIGHTNING,
            known::CITY_LABELS,
            known::SPC_OUTLOOK,
        ] {
            assert_eq!(source_grid_staging_bytes(&id), 0, "{}", id.as_str());
        }
    }

    /// **Nothing is priced as staging that the layer's own residency does not
    /// count**, and nothing a staging layer holds is left out of both figures:
    /// the two functions together are exactly the three blocks
    /// `resident_source_bytes` sums — the cache, the staged granule, the
    /// retained slot.
    #[test]
    fn the_two_figures_together_cover_every_block_a_handler_sums() {
        for (id, cache, staged, slot) in [
            (
                known::MRMS,
                crate::mrms::GRID_CACHE_BYTES,
                crate::mrms::FRAME_STAGING_BYTES,
                crate::mrms::staging::STAGING_POINTS
                    * crate::mrms::staging::StagingPool::ELEMENT_BYTES,
            ),
            (
                known::GMGSI,
                gmgsi::GRID_CACHE_BYTES,
                gmgsi::FRAME_STAGING_BYTES,
                crate::gmgsi::staging::STAGING_POINTS
                    * crate::gmgsi::staging::StagingPool::ELEMENT_BYTES,
            ),
        ] {
            assert_eq!(
                source_grid_budget_bytes(&id) + source_grid_staging_bytes(&id),
                (cache + staged + slot) as u64,
                "{}",
                id.as_str(),
            );
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

        // A layer that stages is a layer that caches: nothing may answer a
        // staging figure without a cache budget, or the sum the scene reads
        // would price a population with no home.
        let staging: Vec<LayerId> = sources()
            .iter()
            .map(|h| h.id().clone())
            .filter(|id| source_grid_staging_bytes(id) > 0)
            .collect();
        assert_eq!(
            staging,
            vec![known::MODEL_DATA, known::MRMS, known::GMGSI],
            "the model layer stages one grid per loop frame in transit; a zero \
             here means its frames are charged to the live cache again",
        );
    }
}
