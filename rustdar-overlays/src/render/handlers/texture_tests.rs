//! Invariants every `RenderMode::Texture` overlay has to satisfy, written over
//! `create_handlers()` so a new one is covered the day it is registered.
//!
//! `rasterize/alpha_tests` checks two rasterizers by calling them directly.
//! That is a check on the *function*, and the thing that reaches the uploader
//! is the [`RasterizeOutput`] a **handler** hands over — so a handler could
//! attach the wrong [`AlphaMode`] to a correct buffer and nothing there would
//! notice. `the_polygon_rasterizers_hand_over_premultiplied_pixels` calls
//! `rasterize_nws_alerts`, which returns a bare `Vec<u8>`; the mode is attached
//! one layer up, in `NwsAlertHandler::prepare_rasterize`, and that layer was
//! unpinned. Flipping it to `Straight` — which double-multiplies every
//! translucent pixel of the alert layer on screen — failed nothing.
//!
//! So the walk below goes through `prepare_rasterize`, and it covers every one
//! of the twelve places an `AlphaMode` is written down: three in the handlers
//! and nine in `render::rasterize`, counting each rasterizer's degenerate early
//! returns as well as its drawing path.

use std::collections::HashSet;

use super::create_handlers;
use crate::render::overlay_state::{
    FetchPayload, OverlayHandler, OverlayKind, RasterizeContext, RenderMode,
};
use crate::render::rasterize::{
    self, AlphaMode, RadarSiteInfo, RasterizeOutput, rasterize_glm_strikes, rasterize_model_data,
    rasterize_radar_sites, rasterize_storm_reports,
};
use crate::types::{GeoBounds, HatchPattern, OverlayFeature};

const W: u32 = 96;
const H: u32 = 96;

const BOUNDS: GeoBounds = GeoBounds {
    min_lat: 34.0,
    max_lat: 36.0,
    min_lon: -99.0,
    max_lon: -97.0,
};

/// The rasterize context a pane hands over: the light theme, because it is the
/// one in which the site plate's own fill is bright enough for the two alpha
/// conventions to disagree about it.
fn rctx() -> RasterizeContext {
    RasterizeContext {
        device_scale: 1.0,
        is_dark: false,
        zoom: 7.0,
    }
}

// ── Fixtures ─────────────────────────────────────────────────────────────

/// A square covering most of [`BOUNDS`], so a fill lands on plenty of pixels.
fn ring() -> Vec<(f64, f64)> {
    vec![
        (34.2, -98.8),
        (34.2, -97.2),
        (35.8, -97.2),
        (35.8, -98.8),
        (34.2, -98.8),
    ]
}

/// Saturated and translucent, so premultiplying it moves every channel.
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
        features: vec![feature()],
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

/// Day 1 Categorical, which is what [`SpcOutlookHandler::set_enabled`] turns on
/// from nothing — so the seeded product and the enabled one are the same.
///
/// [`SpcOutlookHandler::set_enabled`]: super::outlook::SpcOutlookHandler
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

/// Timestamped *now*, because the GLM rasterizer fades a flash out over
/// `time_window_secs` and drops it entirely once it is past the window.
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
/// parameter, so no control has to be driven to make it the selected one.
///
/// The palette entry that lands on is `[255, 165, 0, 160]`: bright, and
/// translucent at an alpha two of its channels clear. That is what makes the
/// straight declaration checkable against the bytes rather than assumed.
fn cin_grid() -> crate::hrrr::HrrrGridData {
    use crate::hrrr::{GridCoords, HrrrGridData, ModelParameter};
    let parameter = ModelParameter::SurfaceBasedCin;
    let (ni, nj) = (4usize, 4usize);
    let values = vec![-100.0f32; ni * nj];
    let mut lats = Vec::with_capacity(ni * nj);
    let mut lons = Vec::with_capacity(ni * nj);
    for j in 0..nj {
        for i in 0..ni {
            // Row 0 is the northern edge, matching the fixture grids next door.
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

fn site_fixtures() -> Vec<RadarSiteInfo> {
    vec![RadarSiteInfo {
        name: "KTLX".into(),
        lat: 35.0,
        lon: -98.0,
        is_current: false,
        is_loading: false,
    }]
}

/// Give `handler` the smallest data it will actually draw, and turn it on.
///
/// `false` for a texture kind that never takes a fetch and never answers
/// `prepare_rasterize`: `RadarSites`, whose raster `app_fetch` dispatches by
/// calling [`rasterize_radar_sites`] directly, and `Radar`, which `ui_map_pane`
/// skips outright because its renders are driven by product and elevation
/// rather than by the viewport.
pub(super) fn seed(handler: &mut dyn OverlayHandler) -> bool {
    use crate::glm::{GlmFetchOutcome, GlmFetchResult};
    use crate::hrrr::HrrrFetchResult;
    use crate::spc::outlook::{OutlookDay, OutlookProduct};

    // Outlook's "enabled" *is* its product set, and the set is what its data
    // is keyed by, so the toggle has to precede the payload for the two to meet.
    handler.set_enabled(true);

    let payload: FetchPayload = match handler.kind() {
        OverlayKind::NwsAlerts => Box::new(super::alert::NwsAlertFetchResult(Ok(
            crate::nws::fetch::ActiveAlerts::whole(vec![alert_fixture()]),
        ))),
        OverlayKind::SpcDiscussions => {
            Box::new(super::discussion::SpcDiscussionFetchResult(Ok(vec![
                discussion_fixture(),
            ])))
        }
        OverlayKind::SpcOutlook => Box::new(super::outlook::SpcOutlookFetchResult {
            day: OutlookDay::Day1,
            product: OutlookProduct::Categorical,
            result: Ok(outlook_fixture()),
        }),
        OverlayKind::StormReports => Box::new(super::reports::StormReportsFetchResult(Ok(
            crate::spc::reports::StormReportRound {
                reports: vec![report_fixture()],
                failed_kinds: Vec::new(),
            },
        ))),
        OverlayKind::Lightning => Box::new(GlmFetchResult(Ok(GlmFetchOutcome {
            flashes: vec![glm_fixture()],
            dead_feeds: Vec::new(),
            queried: Vec::new(),
            parse_failures: None,
            transport_failures: None,
            level_failures: Vec::new(),
            evaluated_levels: Vec::new(),
            listing_failures: Vec::new(),
        }))),
        OverlayKind::ModelData => Box::new(HrrrFetchResult(Ok(cin_grid()))),
        OverlayKind::RadarSites | OverlayKind::Radar => return false,
        other => panic!(
            "{other:?} is a texture overlay this fixture does not know how to \
             seed. Add it here — the walks in this file are what stop a new \
             layer from arriving with an unpinned alpha convention."
        ),
    };
    handler.apply_fetch_result(payload);
    true
}

// ── The alpha convention each handler hands over ─────────────────────────

/// Every pixel the raster actually drew — alpha above zero.
fn drawn(rgba: &[u8]) -> Vec<[u8; 4]> {
    rgba.chunks_exact(4)
        .filter(|p| p[3] > 0)
        .map(|p| [p[0], p[1], p[2], p[3]])
        .collect()
}

/// The declared mode against an invariant of the bytes that only that mode can
/// satisfy.
///
/// Premultiplied RGB is `round(c · a / 255)`, so **no channel can exceed
/// alpha**. Straight RGB is the colour table's own value, so a bright
/// translucent entry has channels far above it. That asymmetry is what makes
/// each half fail when the declaration is flipped: a premultiplied buffer can
/// never produce the channel-above-alpha the straight arm demands, and a
/// straight buffer of a bright colour always produces the one the
/// premultiplied arm forbids.
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

/// **The unpinned nine.** Twelve places write an `AlphaMode` down; before this,
/// flipping nine of them failed nothing at all.
///
/// Written over `prepare_rasterize` because that is the seam the uploader
/// reads: `App::overlay_color_image` picks its egui constructor from
/// `RasterizeOutput::alpha` and cannot tell the two apart by looking, so a
/// wrong declaration is silent all the way to the screen — where it shows up as
/// every translucent pixel of one layer being the wrong colour, which is a
/// thing nobody diffs.
#[test]
fn every_texture_handler_declares_the_convention_its_own_bytes_are_in() {
    let ctx = rctx();
    let mut checked = 0;
    for handler in create_handlers().iter_mut() {
        if handler.render_mode() != RenderMode::Texture {
            continue;
        }
        if !seed(handler.as_mut()) {
            continue;
        }
        let kind = handler.kind();
        let rasterize = handler.prepare_rasterize(&ctx).unwrap_or_else(|| {
            panic!("{kind:?} was seeded with data it should draw and answered None")
        });
        assert_alpha_matches_bytes(&format!("{kind:?}"), &rasterize(&BOUNDS, W, H));
        checked += 1;
    }
    assert_eq!(
        checked, 6,
        "the six texture handlers that rasterize through `prepare_rasterize` \
         must all be covered; a new one is not exempt, and a removed one \
         should be removed from this count deliberately",
    );

    // The seventh raster, and the one with no handler to speak for it:
    // `app_fetch` calls this directly, which is why it returns a
    // `RasterizeOutput` rather than a bare buffer.
    assert_alpha_matches_bytes(
        "rasterize_radar_sites",
        &rasterize_radar_sites(&site_fixtures(), &BOUNDS, W, H, 7.0, false, 1.0),
    );
}

/// The other three sites: each rasterizer's early returns, which hand back an
/// empty buffer and must still name the convention its drawing path does.
///
/// A zero-sided texture is the one branch reachable without a failed
/// allocation — `Pixmap::new` refuses it — and it is what the tiny-skia
/// rasterizers fall back through. `rasterize_model_data` has two of its own:
/// an empty grid, and a grid whose projection window misses the texture.
#[test]
fn the_degenerate_paths_declare_what_the_drawing_paths_do() {
    let now = chrono::Utc::now().naive_utc();
    let glm_params = rasterize::GlmRenderParams {
        device_scale: 1.0,
        zoom: 7.0,
        is_dark: false,
        time_window_secs: 600.0,
        now,
    };

    assert_eq!(
        rasterize_radar_sites(&site_fixtures(), &BOUNDS, 0, 0, 7.0, false, 1.0).alpha,
        AlphaMode::Premultiplied,
    );
    assert_eq!(
        rasterize_storm_reports(&[report_fixture()], &[], &BOUNDS, 0, 0, 7.0, false, 1.0).alpha,
        AlphaMode::Premultiplied,
    );
    assert_eq!(
        rasterize_glm_strikes(&[glm_fixture()], &[], &BOUNDS, 0, 0, &glm_params).alpha,
        AlphaMode::Premultiplied,
    );

    let mut empty = cin_grid();
    empty.values.clear();
    assert_eq!(
        rasterize_model_data(&empty, &BOUNDS, W, H).alpha,
        AlphaMode::Straight,
    );

    // A viewport out over the Atlantic, so `projection_window` narrows the
    // HRRR domain to nothing. It takes a real Lambert grid to get there: an
    // explicit-coordinate grid cannot name an index range at all and answers
    // with the whole of itself, which is never empty.
    let lambert = crate::render::rasterize::lambert_fixture::lambert_grid(64, 64, 0b0100_0000);
    let atlantic = GeoBounds {
        min_lat: 29.5,
        max_lat: 41.5,
        min_lon: -46.0,
        max_lon: -34.0,
    };
    assert_eq!(
        rasterize_model_data(&lambert, &atlantic, W, H).alpha,
        AlphaMode::Straight,
    );
}

/// The fixture set is only worth what it discriminates: every seeded handler
/// has to draw pixels the two conventions actually disagree about, or the walk
/// above passes on a picture too dark to tell them apart.
#[test]
fn every_fixture_draws_pixels_the_two_conventions_disagree_about() {
    let ctx = rctx();
    let mut opaque_only: Vec<OverlayKind> = Vec::new();
    for handler in create_handlers().iter_mut() {
        if handler.render_mode() != RenderMode::Texture || !seed(handler.as_mut()) {
            continue;
        }
        let kind = handler.kind();
        let rasterize = handler
            .prepare_rasterize(&ctx)
            .expect("seeded above, and the walk next door asserts this");
        let out = rasterize(&BOUNDS, W, H);
        // A pixel tells the conventions apart when it is translucent: at
        // `a == 255` the premultiply is the identity and both readings agree.
        let translucent: HashSet<u8> = drawn(&out.rgba)
            .iter()
            .map(|p| p[3])
            .filter(|&a| a < 255)
            .collect();
        if translucent.is_empty() {
            opaque_only.push(kind);
        }
    }
    assert!(
        opaque_only.is_empty(),
        "{opaque_only:?} drew nothing translucent, so a flipped `AlphaMode` \
         would produce byte-identical pixels and the walk next door would \
         pass either way. Give the fixture a translucent fill.",
    );
}

// ── `has_data` against the handler's own rasterizer ──────────────────────

/// **The permanent-wakeup guard.** For every texture handler,
/// `has_data() == prepare_rasterize().is_some()`.
///
/// `ui_map_pane` reads `has_data` for two decisions: whether to dispatch a
/// `RenderOverlay`, and whether a *settle* render is still owed — and the
/// second asks egui for a repaint 100 ms out for as long as the answer is yes.
/// A handler that says it has data and then declines to rasterize is therefore
/// not merely wasteful: the render is dispatched, `spawn_overlay_render` finds
/// no rasterizer and abandons it, the texture stays at the old zoom, and the
/// pane asks for another frame in 100 ms. For ever, on an idle app, with
/// nothing on screen to say why.
///
/// `SpcOutlookHandler` was exactly that. `has_data` was `!state.data.is_empty()`
/// while `prepare_rasterize` needs the *selected day* crossed with the *ticked
/// products* to yield a feature — so untick every SPC product, or move to a day
/// whose products are not ticked, and the two disagreed for ever.
///
/// The states below are the ones the layer stack can actually reach, and the
/// master toggle is the reachable route to the divergence: for a handler whose
/// "enabled" *is* its product set, switching it off empties the very set
/// `prepare_rasterize` looks the data up by.
#[test]
fn every_texture_handler_agrees_with_its_own_rasterizer() {
    let ctx = rctx();
    let mut checked = 0;
    for handler in create_handlers().iter_mut() {
        if handler.render_mode() != RenderMode::Texture {
            continue;
        }
        let kind = handler.kind();
        if matches!(kind, OverlayKind::RadarSites | OverlayKind::Radar) {
            // The two exempt kinds: there is no `prepare_rasterize` for their
            // `has_data` to agree *with*. Their `has_data` is an unconditional
            // `true` and their pixels come from elsewhere — `app_fetch` calls
            // `rasterize_radar_sites` directly and it always produces a buffer,
            // and `ui_map_pane` skips `Radar` outright. Neither dispatch can
            // decline, so neither can strand a settle.
            assert!(
                handler.prepare_rasterize(&ctx).is_none(),
                "{kind:?} grew a `prepare_rasterize`; it now has this invariant \
                 to keep, so seed it in `seed` and drop it from this exemption",
            );
            continue;
        }

        let agree = |h: &dyn OverlayHandler, state: &str| {
            assert_eq!(
                h.has_data(),
                h.prepare_rasterize(&ctx).is_some(),
                "{kind:?} disagrees with its own rasterizer while {state}. \
                 `ui_map_pane` gates both the render dispatch and the settle \
                 repaint on `has_data`, so `true` here with `None` there is a \
                 render asked for on every frame and abandoned on every frame, \
                 and a 100 ms repaint nothing can ever satisfy.",
            );
        };

        // Nothing fetched.
        agree(handler.as_ref(), "empty");

        assert!(
            seed(handler.as_mut()),
            "{kind:?} is not exempt above, so it must be seedable",
        );

        // Seeded and on.
        agree(handler.as_ref(), "seeded and enabled");

        // Off. For alerts, MDs, reports, GLM and HRRR the master toggle is a
        // `bool` the rasterizer never reads, so both halves stay `true`; for
        // outlooks it clears the product set, and both must go to `false`.
        handler.set_enabled(false);
        agree(handler.as_ref(), "seeded, then switched off");

        // And back, because `set_enabled(true)` restores a *default* selection
        // rather than the one that was there — which is another way for the two
        // halves to part company.
        handler.set_enabled(true);
        agree(handler.as_ref(), "seeded, switched off, switched back on");

        checked += 1;
    }
    assert_eq!(
        checked, 6,
        "the six texture handlers that rasterize through `prepare_rasterize` \
         must all be covered",
    );
}

/// The other reachable route to the outlook divergence, which no walk over the
/// trait can reach: the day buttons.
///
/// Day 5 publishes only `Probabilistic`, so a pane holding Day 1's Categorical
/// tick and moving to Day 5 has a full `state.data` and nothing at all to draw.
/// This is the state a user lands in by pressing one button, and before the fix
/// it was a 10 Hz repaint that outlived the gesture, the pane and the session.
#[test]
fn an_outlook_day_with_no_ticked_products_has_no_data_to_draw() {
    use crate::spc::outlook::{OutlookDay, OutlookProduct};

    let ctx = rctx();
    let mut handler = super::outlook::SpcOutlookHandler::new();
    assert!(seed(&mut handler), "the outlook handler takes a fetch");
    assert!(
        handler.has_data() && handler.prepare_rasterize(&ctx).is_some(),
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
        handler.prepare_rasterize(&ctx).is_none(),
        "fixture: there is nothing on Day 5 to rasterize",
    );
    assert!(
        !handler.has_data(),
        "the pane would dispatch a render `spawn_overlay_render` abandons, and \
         ask for another frame 100 ms later, for as long as the app is open",
    );
}
