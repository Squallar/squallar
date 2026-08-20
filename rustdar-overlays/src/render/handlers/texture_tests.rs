//! Invariants every `RenderMode::Texture` overlay has to satisfy, written over
//! `sources()` so a new one is covered the day it is registered.
//!
//! The walk below goes through `prepare_job` — the only dispatch a handler has
//! — and runs each described input through its own rasterizer exactly as
//! `offload::execute`'s overlay arm does, so it covers every place an
//! `AlphaMode` is written down. Calling a rasterizer directly checks the
//! *function*; the mode is attached one layer up, in the handler's input.

use std::collections::HashSet;

use crate::render::overlay_state::{PaneMut, PaneRef};
use rustdar_source::id::{LayerId, known};
use rustdar_source::job::DescribedJob;

use super::sources;
use crate::render::overlay_state::{FetchPayload, OverlayHandler, RasterizeContext, RenderMode};
use crate::render::rasterize::{
    self, AlphaMode, ModelDataInput, RadarSiteInfo, RasterizeOutput, rasterize_glm_strikes,
    rasterize_model_data, rasterize_nws_alerts, rasterize_radar_sites, rasterize_spc_discussions,
    rasterize_spc_outlooks, rasterize_storm_reports,
};
use crate::types::{HatchPattern, OverlayFeature};
use rustdar_geo::GeoBounds;

const W: u32 = 96;
const H: u32 = 96;

const BOUNDS: GeoBounds = GeoBounds {
    min_lat: 34.0,
    max_lat: 36.0,
    min_lon: -99.0,
    max_lon: -97.0,
};

/// The rasterize context a pane hands over: the light theme, in which the site
/// plate's fill is bright enough for the two alpha conventions to disagree.
/// `now` is a real clock read, taken **once** per test and handed to both paths.
fn rctx() -> RasterizeContext {
    RasterizeContext {
        device_scale: 1.0,
        is_dark: false,
        zoom: 7.0,
        now: chrono::Utc::now().naive_utc(),
    }
}

fn run_described(
    job: &DescribedJob,
    bounds: &GeoBounds,
    w: u32,
    h: u32,
) -> (LayerId, RasterizeOutput) {
    if let Some(input) = job.downcast_ref::<rasterize::AlertsInput>() {
        (known::NWS_ALERTS, rasterize_nws_alerts(input, bounds, w, h))
    } else if let Some(input) = job.downcast_ref::<rasterize::OutlooksInput>() {
        (
            known::SPC_OUTLOOK,
            rasterize_spc_outlooks(input, bounds, w, h),
        )
    } else if let Some(input) = job.downcast_ref::<rasterize::DiscussionsInput>() {
        (
            known::SPC_DISCUSSIONS,
            rasterize_spc_discussions(input, bounds, w, h),
        )
    } else if let Some(input) = job.downcast_ref::<rasterize::ReportsInput>() {
        (
            known::STORM_REPORTS,
            rasterize_storm_reports(input, bounds, w, h),
        )
    } else if let Some(input) = job.downcast_ref::<rasterize::GlmStrikesInput>() {
        (known::LIGHTNING, rasterize_glm_strikes(input, bounds, w, h))
    } else if let Some(input) = job.downcast_ref::<ModelDataInput>() {
        (known::MODEL_DATA, rasterize_model_data(input, bounds, w, h))
    } else {
        panic!("a handler described an input no handler-backed texture layer claims: {job:?}")
    }
}

fn ring() -> Vec<(f64, f64)> {
    vec![
        (34.2, -98.8),
        (34.2, -97.2),
        (35.8, -97.2),
        (35.8, -98.8),
        (34.2, -98.8),
    ]
}

fn feature() -> OverlayFeature {
    OverlayFeature::new(
        vec![vec![ring()]],
        [255, 0, 0, 128],
        [0, 0, 0, 0],
        "T".into(),
        String::new(),
        HatchPattern::None,
    )
}

fn alert_fixture() -> crate::nws::alert::NwsAlert {
    use crate::nws::alert::{AlertCategory, NwsAlert};
    NwsAlert {
        id: "urn:test".into(),
        event: "Tornado Warning".into(),
        category: AlertCategory::from_event("Tornado Warning"),
        severity: "Severe".parse().unwrap(),
        urgency: "Immediate".parse().unwrap(),
        certainty: "Observed".parse().unwrap(),
        headline: None,
        description: String::new(),
        instruction: None,
        area_desc: String::new(),
        sender_name: String::new(),
        effective: String::new(),
        expires: String::new(),
        onset: None,
        ends: None,
        affected_zones: Vec::new(),
        features: std::sync::Arc::new(vec![feature()]),
    }
}

fn discussion_fixture() -> crate::spc::discussion::SpcDiscussion {
    use crate::spc::discussion::{MdType, SpcDiscussion};
    SpcDiscussion {
        number: 1,
        title: "Mesoscale Discussion #0001".into(),
        text: String::new(),
        link: String::new(),
        md_type: MdType::Convective,
        polygon: vec![ring()],
        feature: feature(),
        concerning: None,
    }
}

fn outlook_fixture() -> crate::spc::outlook::SpcOutlook {
    use crate::spc::outlook::{OutlookDay, OutlookProduct, SpcOutlook};
    SpcOutlook {
        day: OutlookDay::Day1,
        product: OutlookProduct::Categorical,
        valid: None,
        expire: None,
        features: vec![feature()],
    }
}

fn report_fixture() -> crate::spc::reports::StormReport {
    use crate::spc::reports::{StormReport, StormReportKind};
    StormReport {
        kind: StormReportKind::Hail,
        time: "2015".into(),
        magnitude: Some(175.0),
        location: "NORMAN".into(),
        county: "CLEVELAND".into(),
        state: "OK".into(),
        lat: 35.0,
        lon: -98.0,
        comments: String::new(),
    }
}

/// Timestamped *now*: the GLM rasterizer fades a flash out over
/// `time_window_secs` and drops it past the window.
fn glm_fixture() -> crate::glm::GlmFlash {
    use crate::glm::{GlmDataLevel, GlmFlash, GlmSatellite};
    GlmFlash {
        lat: 35.0,
        lon: -98.0,
        energy: None,
        area: None,
        time: chrono::Utc::now().naive_utc(),
        satellite: GlmSatellite::GoesEast,
        level: GlmDataLevel::Flash,
    }
}

/// Uniform −100 J/kg of CIN over [`BOUNDS`] — the handler's own default
/// parameter. The palette entry that lands on is `[255, 165, 0, 160]`: bright,
/// and translucent at an alpha two of its channels clear.
fn cin_grid() -> crate::hrrr::HrrrGridData {
    use crate::hrrr::{GridCoords, HrrrGridData, ModelParameter};
    let parameter = ModelParameter::SurfaceBasedCin;
    let (ni, nj) = (4usize, 4usize);
    let values = vec![-100.0f32; ni * nj];
    let mut lats = Vec::with_capacity(ni * nj);
    let mut lons = Vec::with_capacity(ni * nj);
    for j in 0..nj {
        for i in 0..ni {
            lats.push(BOUNDS.max_lat - (BOUNDS.max_lat - BOUNDS.min_lat) * (j as f64 / 3.0));
            lons.push(BOUNDS.min_lon + (BOUNDS.max_lon - BOUNDS.min_lon) * (i as f64 / 3.0));
        }
    }
    let (visible_points, value_range) = crate::hrrr::summarize_values(&values, parameter);
    HrrrGridData {
        parameter,
        values,
        coords: GridCoords::Explicit { lats, lons },
        ni,
        nj,
        bounds: BOUNDS,
        ref_time: chrono::NaiveDate::from_ymd_opt(2026, 7, 25)
            .unwrap()
            .and_hms_opt(3, 0, 0)
            .unwrap(),
        forecast_hour: parameter.forecast_hour(),
        visible_points,
        value_range,
    }
}

fn whole(grid: crate::hrrr::HrrrGridData) -> ModelDataInput {
    ModelDataInput::Whole(std::sync::Arc::new(grid))
}

fn site_fixtures() -> rasterize::SitesInput {
    rasterize::SitesInput {
        sites: vec![RadarSiteInfo {
            name: "KTLX".into(),
            lat: 35.0,
            lon: -98.0,
            is_current: false,
            is_loading: false,
        }],
        zoom: 7.0,
        is_dark: false,
        device_scale: 1.0,
    }
}

/// Give `handler` the smallest data it will actually draw, and turn it on.
/// `false` for a texture kind that never takes a fetch and never answers
/// `prepare_job`: `RadarSites` and `Radar`.
pub(super) fn seed(handler: &mut dyn OverlayHandler) -> bool {
    use crate::glm::{GlmFetchOutcome, GlmFetchResult};
    use crate::hrrr::HrrrFetchResult;
    use crate::spc::outlook::{OutlookDay, OutlookProduct};

    // Outlook's "enabled" *is* its product set, and the set is what its data is
    // keyed by, so the toggle has to precede the payload.
    handler.set_enabled(true, &mut PaneMut::bare(0));

    let payload: FetchPayload = match &handler.id() {
        id if *id == known::NWS_ALERTS => Box::new(super::alert::NwsAlertFetchResult(Ok(
            crate::nws::fetch::ActiveAlerts::whole(vec![alert_fixture()]),
        ))),
        id if *id == known::SPC_DISCUSSIONS => {
            Box::new(super::discussion::SpcDiscussionFetchResult(Ok(vec![
                discussion_fixture(),
            ])))
        }
        id if *id == known::SPC_OUTLOOK => Box::new(super::outlook::SpcOutlookFetchResult {
            day: OutlookDay::Day1,
            product: OutlookProduct::Categorical,
            result: Ok(outlook_fixture()),
        }),
        id if *id == known::STORM_REPORTS => Box::new(super::reports::StormReportsFetchResult(Ok(
            crate::spc::reports::StormReportRound {
                reports: vec![
                    report_fixture(),
                    crate::spc::reports::StormReport {
                        kind: crate::spc::reports::StormReportKind::Tornado,
                        lat: 34.4,
                        lon: -98.6,
                        ..report_fixture()
                    },
                    crate::spc::reports::StormReport {
                        kind: crate::spc::reports::StormReportKind::Wind,
                        lat: 35.6,
                        lon: -97.4,
                        ..report_fixture()
                    },
                ],
                failed_kinds: Vec::new(),
            },
        ))),
        id if *id == known::LIGHTNING => Box::new(GlmFetchResult(Ok(GlmFetchOutcome {
            flashes: vec![
                glm_fixture(),
                crate::glm::GlmFlash {
                    lat: 34.4,
                    lon: -98.6,
                    ..glm_fixture()
                },
                crate::glm::GlmFlash {
                    lat: 35.6,
                    lon: -97.4,
                    ..glm_fixture()
                },
            ],
            dead_feeds: Vec::new(),
            queried: Vec::new(),
            parse_failures: None,
            transport_failures: None,
            level_failures: Vec::new(),
            evaluated_levels: Vec::new(),
            listing_failures: Vec::new(),
            window_gaps: Vec::new(),
            record_drops: crate::glm::RecordDrops::default(),
        }))),
        id if *id == known::MODEL_DATA => Box::new(HrrrFetchResult(Ok(cin_grid()))),
        id if *id == known::RADAR_SITES => return false,
        other => panic!(
            "{} is a texture overlay this fixture does not know how to \
             seed. Add it here — the walks in this file are what stop a new \
             layer from arriving with an unpinned alpha convention.",
            other.as_str()
        ),
    };
    handler.apply_fetch_result(payload, &PaneRef::across(&[]));
    true
}

fn drawn(rgba: &[u8]) -> Vec<[u8; 4]> {
    rgba.chunks_exact(4)
        .filter(|p| p[3] > 0)
        .map(|p| [p[0], p[1], p[2], p[3]])
        .collect()
}

/// The declared mode against an invariant of the bytes that only that mode can
/// satisfy: premultiplied RGB is `round(c · a / 255)`, so **no channel can
/// exceed alpha**, while a bright translucent straight entry has channels far
/// above it.
fn assert_alpha_matches_bytes(what: &str, out: &RasterizeOutput) {
    let pixels = drawn(&out.rgba);
    assert!(
        !pixels.is_empty(),
        "{what}: the fixture drew nothing, so neither invariant below says \
         anything about it",
    );
    let above: Vec<[u8; 4]> = pixels
        .iter()
        .copied()
        .filter(|p| p[0] > p[3] || p[1] > p[3] || p[2] > p[3])
        .collect();
    match out.alpha {
        AlphaMode::Premultiplied => assert!(
            above.is_empty(),
            "{what} declares premultiplied, but {} of its pixels have a colour \
             channel above their alpha, e.g. {:?} — which premultiplied RGBA \
             cannot represent. Either the bytes are straight and the \
             declaration is wrong, or an un-premultiply is back in the path.",
            above.len(),
            &above[..above.len().min(4)],
        ),
        AlphaMode::Straight => assert!(
            !above.is_empty(),
            "{what} declares straight alpha, but not one of its {} drawn \
             pixels has a colour channel above its alpha — which is exactly \
             what a premultiplied buffer looks like. `from_rgba_unmultiplied` \
             will multiply these bytes a second time and darken every \
             translucent pixel of this layer.",
            pixels.len(),
        ),
    }
}

/// **The unpinned declarations.** The `AlphaMode` a handler's described input
/// rasterizes to, checked against the bytes for every texture handler at once —
/// written over `prepare_job`, since `offload::execute`'s overlay arm reads
/// `RasterizeOutput::alpha` off exactly this output.
#[test]
fn every_texture_handler_declares_the_convention_its_own_bytes_are_in() {
    let ctx = rctx();
    let mut checked = 0;
    for handler in sources().iter_mut() {
        if handler.render_mode() != RenderMode::Texture {
            continue;
        }
        if !seed(handler.as_mut()) {
            continue;
        }
        let id = handler.id();
        let name = id.as_str();
        let input = handler
            .prepare_job(&ctx, &PaneRef::bare(0))
            .unwrap_or_else(|| {
                panic!("{name} was seeded with data it should draw and answered None")
            });
        let (named, out) = run_described(&input, &BOUNDS, W, H);
        assert_eq!(
            named, id,
            "{name}'s `prepare_job` answered another layer's input variant, \
             so a worker would rasterize the wrong layer under its panes",
        );
        assert_alpha_matches_bytes(name, &out);
        assert!(
            drawn(&out.rgba).len() > 100,
            "{name}'s fixture painted almost nothing",
        );
        if id == known::STORM_REPORTS || id == known::LIGHTNING {
            let cells = out
                .hit_cells
                .as_ref()
                .expect("a hit-map kind answers cells on its drawing path");
            assert!(
                !cells.cells.is_empty(),
                "{name}'s fixture recorded no hit cells, so nothing about \
                 the id space is being exercised",
            );
        }
        checked += 1;
    }
    assert_eq!(
        checked, 6,
        "the six texture handlers that rasterize through `prepare_job` \
         must all be covered; a new one is not exempt, and a removed one \
         should be removed from this count deliberately",
    );

    // The seventh raster, and the one with no handler to speak for it:
    // `app_fetch` builds its input from the site catalogue itself.
    assert_alpha_matches_bytes(
        "rasterize_radar_sites",
        &rasterize_radar_sites(&site_fixtures(), &BOUNDS, W, H),
    );
}

/// The other three sites: each rasterizer's early returns, which hand back an
/// empty buffer and must still name the convention its drawing path does.
///
/// A zero-sided texture is the one branch reachable without a failed allocation
/// — `Pixmap::new` refuses it. `rasterize_model_data` has two of its own: an
/// empty grid, and a grid whose projection window misses the texture.
#[test]
fn the_degenerate_paths_declare_what_the_drawing_paths_do() {
    let now = chrono::Utc::now().naive_utc();
    let flash = glm_fixture();
    let glm_input = rasterize::GlmStrikesInput {
        flashes: vec![rasterize::FlashPaint {
            lat: flash.lat,
            lon: flash.lon,
            time: flash.time,
            energy: flash.energy,
        }],
        device_scale: 1.0,
        zoom: 7.0,
        is_dark: false,
        time_window_secs: 600.0,
        now,
    };
    let report = report_fixture();
    let reports_input = rasterize::ReportsInput {
        reports: vec![rasterize::ReportPaint {
            kind: report.kind,
            lat: report.lat,
            lon: report.lon,
        }],
        zoom: 7.0,
        is_dark: false,
        device_scale: 1.0,
    };

    assert_eq!(
        rasterize_radar_sites(&site_fixtures(), &BOUNDS, 0, 0).alpha,
        AlphaMode::Premultiplied,
    );
    assert_eq!(
        rasterize_storm_reports(&reports_input, &BOUNDS, 0, 0).alpha,
        AlphaMode::Premultiplied,
    );
    assert_eq!(
        rasterize_glm_strikes(&glm_input, &BOUNDS, 0, 0).alpha,
        AlphaMode::Premultiplied,
    );

    let mut empty = cin_grid();
    empty.values.clear();
    assert_eq!(
        rasterize_model_data(&whole(empty), &BOUNDS, W, H).alpha,
        AlphaMode::Straight,
    );

    // A viewport out over the Atlantic, so `projection_window` narrows the HRRR
    // domain to nothing. It takes a real Lambert grid to get there.
    let lambert = crate::render::rasterize::lambert_fixture::lambert_grid(64, 64, 0b0100_0000);
    let atlantic = GeoBounds {
        min_lat: 29.5,
        max_lat: 41.5,
        min_lon: -46.0,
        max_lon: -34.0,
    };
    assert_eq!(
        rasterize_model_data(&whole(lambert), &atlantic, W, H).alpha,
        AlphaMode::Straight,
    );
}

/// The fixture set is only worth what it discriminates: every seeded handler
/// has to draw pixels the two conventions actually disagree about.
#[test]
fn every_fixture_draws_pixels_the_two_conventions_disagree_about() {
    let ctx = rctx();
    let mut opaque_only: Vec<LayerId> = Vec::new();
    for handler in sources().iter_mut() {
        if handler.render_mode() != RenderMode::Texture || !seed(handler.as_mut()) {
            continue;
        }
        let id = handler.id();
        let input = handler
            .prepare_job(&ctx, &PaneRef::bare(0))
            .expect("seeded above, and the walk next door asserts this");
        let (_, out) = run_described(&input, &BOUNDS, W, H);
        let translucent: HashSet<u8> = drawn(&out.rgba)
            .iter()
            .map(|p| p[3])
            .filter(|&a| a < 255)
            .collect();
        if translucent.is_empty() {
            opaque_only.push(id);
        }
    }
    assert!(
        opaque_only.is_empty(),
        "{opaque_only:?} drew nothing translucent, so a flipped `AlphaMode` \
         would produce byte-identical pixels and the walk next door would \
         pass either way. Give the fixture a translucent fill.",
    );
}

/// **The permanent-wakeup guard.** For every texture handler,
/// `has_data() == prepare_job().is_some()`.
///
/// `ui_map_pane` reads `has_data` both to dispatch a `RenderOverlay` and to
/// decide whether a *settle* render is still owed, asking egui for a repaint
/// 100 ms out for as long as the answer is yes — so a handler that says it has
/// data and then declines to describe a job repaints for ever on an idle app.
/// `SpcOutlookHandler` was exactly that.
#[test]
fn every_texture_handler_agrees_with_its_own_rasterizer() {
    let ctx = rctx();
    let mut checked = 0;
    for handler in sources().iter_mut() {
        if handler.render_mode() != RenderMode::Texture {
            continue;
        }
        let id = handler.id();
        let name = id.as_str();
        if id == known::RADAR_SITES {
            // The one exempt kind: there is no `prepare_job` for its `has_data`
            // to agree *with*, and its dispatch cannot decline.
            assert!(
                handler.prepare_job(&ctx, &PaneRef::bare(0)).is_none(),
                "{name} grew a `prepare_job`; it now has this invariant \
                 to keep, so seed it in `seed` and drop it from this exemption",
            );
            continue;
        }

        let agree = |h: &dyn OverlayHandler, state: &str| {
            assert_eq!(
                h.has_data(&PaneRef::bare(0)),
                h.prepare_job(&ctx, &PaneRef::bare(0)).is_some(),
                "{name} disagrees with its own rasterizer while {state}. \
                 `ui_map_pane` gates both the render dispatch and the settle \
                 repaint on `has_data`, so `true` here with `None` there is a \
                 render asked for on every frame and abandoned on every frame, \
                 and a 100 ms repaint nothing can ever satisfy.",
            );
        };

        agree(handler.as_ref(), "empty");

        assert!(
            seed(handler.as_mut()),
            "{name} is not exempt above, so it must be seedable",
        );

        agree(handler.as_ref(), "seeded and enabled");

        // Off. For every kind but outlooks the master toggle is a `bool` the
        // rasterizer never reads, so both halves stay `true`.
        handler.set_enabled(false, &mut PaneMut::bare(0));
        agree(handler.as_ref(), "seeded, then switched off");

        handler.set_enabled(true, &mut PaneMut::bare(0));
        agree(handler.as_ref(), "seeded, switched off, switched back on");

        checked += 1;
    }
    assert_eq!(
        checked, 6,
        "the six texture handlers that rasterize through `prepare_job` \
         must all be covered",
    );
}

/// The other reachable route to the outlook divergence, which no walk over the
/// trait can reach: the day buttons.
///
/// Day 5 publishes only `Probabilistic`, so a pane holding Day 1's Categorical
/// tick and moving to Day 5 has a full `state.data` and nothing to draw — one
/// button press into a 10 Hz repaint that outlives the gesture.
#[test]
fn an_outlook_day_with_no_ticked_products_has_no_data_to_draw() {
    use crate::spc::outlook::{OutlookDay, OutlookProduct};

    let ctx = rctx();
    let mut handler = super::outlook::SpcOutlookHandler::new();
    assert!(seed(&mut handler), "the outlook handler takes a fetch");
    assert!(
        handler.has_data(&PaneRef::bare(0))
            && handler.prepare_job(&ctx, &PaneRef::bare(0)).is_some(),
        "fixture: Day 1 Categorical is both ticked and fetched",
    );

    handler.selected_day = OutlookDay::Day5;
    assert!(
        !OutlookDay::Day5
            .products()
            .contains(&OutlookProduct::Categorical),
        "fixture: Day 5 must not publish the product that is ticked",
    );
    assert!(
        !handler.state.data.is_empty(),
        "fixture: the layer still holds Day 1's outlook, which is what made \
         the old `!data.is_empty()` answer `true`",
    );

    assert!(
        handler.prepare_job(&ctx, &PaneRef::bare(0)).is_none(),
        "fixture: there is nothing on Day 5 to rasterize",
    );
    assert!(
        !handler.has_data(&PaneRef::bare(0)),
        "the pane would dispatch a render `spawn_overlay_render` abandons, and \
         ask for another frame 100 ms later, for as long as the app is open",
    );
}

/// Whether `id` resolves clicks through a hit map, and therefore must answer
/// [`OverlayHandler::hit_items`] exactly when it answers `prepare_job`.
fn has_hit_map(id: &LayerId) -> bool {
    *id == known::STORM_REPORTS || *id == known::LIGHTNING
}

/// **Every texture kind that renders through a handler has a described job**
/// and, for the hit-map kinds, `hit_items` agrees with `prepare_job` in every
/// reachable state. `spawn_overlay_render` routes by an explicit match on kind,
/// so it and the implementations here are two statements of one set.
#[test]
fn every_texture_kind_rasterizes_as_a_described_job() {
    let ctx = rctx();
    let mut described = 0;
    for handler in sources().iter_mut() {
        let id = handler.id();
        let name = id.as_str();
        let handler_backed =
            handler.render_mode() == RenderMode::Texture && id != known::RADAR_SITES;

        let agree = |h: &dyn OverlayHandler, state: &str| {
            if !handler_backed {
                assert!(
                    h.prepare_job(&ctx, &PaneRef::bare(0)).is_none(),
                    "{name} grew a `prepare_job` while {state}. The \
                     described set is stated twice — here and in \
                     `spawn_overlay_render`'s match — so add the layer to the \
                     dispatch's described arm and to the codec registry \
                     (`render::jobs::JOB_CODECS` + its `job_codec` row) \
                     together.",
                );
            }
            if has_hit_map(&id) {
                assert_eq!(
                    h.hit_items().is_some(),
                    h.prepare_job(&ctx, &PaneRef::bare(0)).is_some(),
                    "{name}'s `hit_items` disagrees with its `prepare_job` \
                     while {state}. The dispatch captures the two together, \
                     and rows without items is a layer whose every hover \
                     resolves to nothing.",
                );
                if let (Some(items), Some(_)) =
                    (h.hit_items(), h.prepare_job(&ctx, &PaneRef::bare(0)))
                {
                    assert_eq!(
                        items.len(),
                        h.item_count(&PaneRef::bare(0)),
                        "{name}'s `hit_items` while {state} does not cover \
                         its data one item per row; a shorter list truncates \
                         the id space the cells index into",
                    );
                }
            } else {
                assert!(
                    h.hit_items().is_none(),
                    "{name} grew a `hit_items` while {state}; only the \
                     hit-map layers have an id_map to capture, and a stray one \
                     here would make the dispatch zip cells of another layer",
                );
            }
        };

        agree(handler.as_ref(), "empty");
        if handler.render_mode() != RenderMode::Texture || !seed(handler.as_mut()) {
            continue;
        }
        assert!(
            handler.prepare_job(&ctx, &PaneRef::bare(0)).is_some(),
            "{name} is a seeded texture layer with no described job — the \
             closure path it would have ridden is deleted, so this layer \
             cannot render at all",
        );
        agree(handler.as_ref(), "seeded and enabled");
        handler.set_enabled(false, &mut PaneMut::bare(0));
        agree(handler.as_ref(), "seeded, then switched off");
        handler.set_enabled(true, &mut PaneMut::bare(0));
        agree(handler.as_ref(), "seeded, switched off, switched back on");
        described += 1;
    }
    assert_eq!(
        described, 6,
        "the three polygon kinds, the two hit-map kinds and the model grid \
         must all have been walked seeded; a kind that stopped seeding is a \
         kind whose described job was never tested",
    );
}

/// **The order-stability invariant at its source**: `hit_items()[i]` is the item
/// whose row `prepare_job` describes at position `i`, checked against the item's
/// **own** identity rather than against the list it came from.
///
/// The one place the check can be independent: the frontend's zip and probe
/// tests take `hit_items` as ground truth, so a handler that shuffled its items
/// would satisfy them while every hover named the wrong report.
#[test]
fn a_hit_map_kinds_items_align_with_its_described_rows() {
    let ctx = rctx();
    let mut checked = 0;
    for handler in sources().iter_mut() {
        let id = handler.id();
        let name = id.as_str();
        if !has_hit_map(&id) || !seed(handler.as_mut()) {
            continue;
        }
        let job = handler
            .prepare_job(&ctx, &PaneRef::bare(0))
            .expect("seeded, and the agreement walk pins this");
        let items = handler.hit_items().expect("seeded");
        if let Some(input) = job.downcast_ref::<rasterize::ReportsInput>() {
            assert_eq!(items.len(), input.reports.len(), "one item per row");
            for (i, (row, item)) in input.reports.iter().zip(&items).enumerate() {
                let item = item
                    .as_any()
                    .downcast_ref::<super::reports::StormReportItem>()
                    .expect("a reports handler captures report items");
                assert_eq!(
                    item.index, i,
                    "{name} item {i} carries another position's index",
                );
                assert_eq!(
                    (item.report.lat, item.report.lon),
                    (row.lat, row.lon),
                    "{name} row {i} and item {i} are different reports — \
                     a hover here would name the wrong one, and no \
                     downstream test can see it because they all zip with \
                     this very list",
                );
            }
        } else if let Some(input) = job.downcast_ref::<rasterize::GlmStrikesInput>() {
            assert_eq!(items.len(), input.flashes.len(), "one item per row");
            for (i, (row, item)) in input.flashes.iter().zip(&items).enumerate() {
                let item = item
                    .as_any()
                    .downcast_ref::<super::glm::GlmFlashItem>()
                    .expect("a GLM handler captures flash items");
                assert_eq!(
                    item.index, i,
                    "{name} item {i} carries another position's index",
                );
                assert_eq!(
                    (item.flash.lat, item.flash.lon),
                    (row.lat, row.lon),
                    "{name} row {i} and item {i} are different flashes — \
                     a hover here would name the wrong one",
                );
            }
        } else {
            panic!("{name} described {job:?}, another layer's input");
        }
        checked += 1;
    }
    assert_eq!(checked, 2, "both hit-map kinds must be walked seeded");
}

/// **The registry pairing gate, bidirectional.** Every texture handler that
/// rasterizes through the job boundary owns exactly one row of
/// [`crate::render::jobs::JOB_CODECS`], claimed exactly once, none unclaimed.
///
/// The kind → label pairing is spelled literally rather than derived, so a
/// *swapped* pair of registrations is caught by name. `RadarSites` claims its
/// row while its `prepare_job` stays `None`: the row states how the bytes cross,
/// not who builds them.
#[test]
fn every_texture_handler_owns_exactly_one_codec_row() {
    use crate::render::jobs::JOB_CODECS;

    // Deliberately spelled out. Do not regenerate this from the registry.
    let expected: [(LayerId, &str); 7] = [
        (known::RADAR_SITES, "overlay/sites"),
        (known::NWS_ALERTS, "overlay/alerts"),
        (known::SPC_OUTLOOK, "overlay/outlooks"),
        (known::SPC_DISCUSSIONS, "overlay/discussions"),
        (known::STORM_REPORTS, "overlay/reports"),
        (known::LIGHTNING, "overlay/glm"),
        (known::MODEL_DATA, "overlay/model"),
    ];

    let mut claimed: Vec<&'static str> = Vec::new();
    for handler in sources() {
        let id = handler.id();
        let name = id.as_str();
        if handler.render_mode() != RenderMode::Texture {
            assert!(
                handler.job_codec().is_none(),
                "{name} is not a texture layer and has no raster to frame, \
                 but it answers a codec row — the dispatch would label and \
                 encode a job this layer can never describe",
            );
            continue;
        }
        let row = handler.job_codec().unwrap_or_else(|| {
            panic!(
                "{name} is a texture handler with no codec row: its \
                 described job has no encode, decode or label, so the \
                 dispatch cannot frame it for a worker at all",
            )
        });
        let expected_label = expected
            .iter()
            .find(|(k, _)| *k == id)
            .map(|(_, label)| *label)
            .unwrap_or_else(|| {
                panic!(
                    "{name} answers a codec row but has no line in the \
                     `expected` table above — a new texture layer must claim \
                     its row here, deliberately",
                )
            });
        assert_eq!(
            row.label, expected_label,
            "{name} claims the row labelled {:?} where {expected_label:?} \
             is its own — a swapped registration frames one layer's job with \
             another layer's codec, and the timing log lies about which layer \
             was slow",
            row.label,
        );
        assert!(
            !claimed.contains(&row.label),
            "{name}'s row {:?} is already claimed by another handler; a \
             row claimed twice means two layers believe they own one wire \
             form",
            row.label,
        );
        claimed.push(row.label);
    }

    for row in JOB_CODECS {
        assert!(
            claimed.contains(&row.label),
            "the row labelled {:?} is claimed by no handler: a codec with \
             no owner is dead weight today and a mis-registration trap the \
             day someone reuses it",
            row.label,
        );
    }
    assert_eq!(
        claimed.len(),
        JOB_CODECS.len(),
        "the claimed set and the registry disagree in size even though \
         every row is claimed — a duplicate slipped through",
    );
}
