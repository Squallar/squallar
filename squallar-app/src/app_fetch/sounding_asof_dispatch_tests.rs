//! **Which environmental sounding each loop frame is rendered against.**
//!
//! `spawn_loop_frame_render` renders every frame at its own `timestamp`, and asks
//! for that frame's melting layer and that frame's RPG storm motion by handing
//! `timestamp` to both. The environmental 0 °C / −20 °C pair is the one input on
//! that path that is asked for by site alone, so a loop spanning hours paints
//! every frame with whatever single pair the site last fetched.
//!
//! That pair is not decoration. `render::hail_sweep` clips every layer at `H₀` and
//! weights by `temp_weight(median, h0, hm20)`, so SHI — and POSH and MEHS with it —
//! move together with it, and `sounding.rs`'s own header names 500 m as the error
//! that moves every hail and HCA class at once while leaving each one plausible.
//! Measured at KOAX over 72 consecutive hourly rows, `|Δh0|` is 150 m at the median
//! over six hours and 450 m at the worst, so a six-hour loop on one pair reaches
//! that threshold on its own.

use super::melting_layer_dispatch_tests::{Recorder, dual_pol_scan};
use crate::platform_double::TestBridge;
use squallar_worker::offload::JobRequest;
use std::sync::{Arc, Mutex};

const SITE: &str = "KTLX";

/// A volume start on the hour, so two frames are a whole number of hourly
/// sounding rows apart.
fn volume_at(hour: u32) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 1, 15)
        .unwrap()
        .and_hms_opt(hour, 0, 0)
        .unwrap()
}

/// A sounding valid for `hour` on the fixture's day.
fn sounding_for(hour: u32, h0: f64) -> squallar_radar::sounding::EnvHeights {
    squallar_radar::sounding::EnvHeights {
        h0c_km_msl: h0,
        hm20c_km_msl: h0 + 3.0,
        valid_at: volume_at(hour).and_utc(),
        fetched_at: volume_at(hour).and_utc(),
    }
}

/// The `(0 °C, −20 °C)` pair each posted job carries, in dispatch order.
fn dispatched_env_heights(posted: &Arc<Mutex<Vec<Vec<u8>>>>) -> Vec<Option<(f64, f64)>> {
    posted
        .lock()
        .unwrap()
        .iter()
        .map(|bytes| {
            let job = JobRequest::from_bytes(bytes).expect("a job this build posted decodes");
            let plan = job
                .job
                .downcast_ref::<squallar_radar::jobs::RadarPlanJob>()
                .unwrap_or_else(|| panic!("expected a Level II render job, got {job:?}"));
            plan.input.env_heights_km_msl()
        })
        .collect()
}

/// Two loop frames six hours apart must not be rendered against the same
/// environmental sounding.
///
/// The app is seeded with one pair — one is all a site-keyed store can hold — and
/// the frames are dispatched at 12:00 and 18:00. A dispatch that asks for the
/// frame's own instant cannot answer both with that single pair; a dispatch that
/// asks by site alone answers both with it and this test is red.
#[test]
fn a_hail_loop_spanning_hours_is_not_painted_with_one_environmental_sounding() {
    let posted = Arc::new(Mutex::new(Vec::new()));
    let _worker =
        squallar_worker::offload::install_test_worker(Box::new(Recorder(Arc::clone(&posted))));

    let mut app = crate::app::tests::headless(TestBridge::desktop());
    let site = squallar_radar::sites::get_radar_site(SITE)
        .expect("KTLX is a real radar")
        .clone();
    app.gui.pane_mut(0).unwrap().set_site(SITE.to_string());
    app.render.ensure_pane_count(1);

    // The sounding the site holds, valid for the 12:00 frame's hour.
    app.render
        .set_env_heights(SITE, sounding_for(12, 3.0), &app.gui);

    let params = || crate::render_dispatch::RenderParams {
        product: squallar_radar::types::RadarProduct::HydrometeorClassification,
        elevation: 0.5,
        lat: site.lat,
        lon: site.lon,
    };
    let target = || squallar_egui::pane::RenderTarget {
        site: SITE.to_string(),
        product: squallar_radar::fields::known::HYDROMETEOR_CLASSIFICATION,
        elevation: 0.5,
    };

    // The loop dispatch reads each frame's volume out of the loop cache.
    for hour in [12, 18] {
        app.loop_mgr.cache_scan(
            SITE,
            volume_at(hour),
            (std::sync::Arc::new(dual_pol_scan()), Default::default()),
        );
    }

    assert!(
        app.spawn_loop_frame_render(0, volume_at(12), params(), target()),
        "the 12:00 frame must actually reach the loop dispatch",
    );
    assert!(
        app.spawn_loop_frame_render(0, volume_at(18), params(), target()),
        "the 18:00 frame must actually reach the loop dispatch",
    );

    let heights = dispatched_env_heights(&posted);
    assert_eq!(heights.len(), 2, "two loop frames were dispatched");
    assert_ne!(
        heights[0], heights[1],
        "a loop frame at 12:00 and a loop frame at 18:00 were rendered against \
         the same environmental sounding {:?}; the pair is asked for by site \
         alone, so one fetch's 0 °C height clips every layer and weights every \
         SHI in the loop regardless of when the frame is from",
        heights[0],
    );
}

/// The positive half: with a sounding in hand for each hour, each loop frame is
/// rendered against **its own** hour's atmosphere.
///
/// The two pairs differ by 700 m in `H₀` — inside the range a real day covers
/// (KOAX spanned 770 m over three days) and above the 500 m this module's header
/// names as the error that moves every hail and HCA class at once.
#[test]
fn each_loop_frame_is_rendered_against_its_own_hours_sounding() {
    let posted = Arc::new(Mutex::new(Vec::new()));
    let _worker =
        squallar_worker::offload::install_test_worker(Box::new(Recorder(Arc::clone(&posted))));

    let mut app = crate::app::tests::headless(TestBridge::desktop());
    let site = squallar_radar::sites::get_radar_site(SITE)
        .expect("KTLX is a real radar")
        .clone();
    app.gui.pane_mut(0).unwrap().set_site(SITE.to_string());
    app.render.ensure_pane_count(1);

    // One request over the loop's span lands every hour it covers.
    app.render.set_env_heights_series(
        SITE,
        [sounding_for(12, 3.0), sounding_for(18, 3.7)],
        &app.gui,
    );

    let params = || crate::render_dispatch::RenderParams {
        product: squallar_radar::types::RadarProduct::HydrometeorClassification,
        elevation: 0.5,
        lat: site.lat,
        lon: site.lon,
    };
    let target = || squallar_egui::pane::RenderTarget {
        site: SITE.to_string(),
        product: squallar_radar::fields::known::HYDROMETEOR_CLASSIFICATION,
        elevation: 0.5,
    };
    for hour in [12, 18] {
        app.loop_mgr.cache_scan(
            SITE,
            volume_at(hour),
            (std::sync::Arc::new(dual_pol_scan()), Default::default()),
        );
    }

    assert!(app.spawn_loop_frame_render(0, volume_at(12), params(), target()));
    assert!(app.spawn_loop_frame_render(0, volume_at(18), params(), target()));

    let heights = dispatched_env_heights(&posted);
    assert_eq!(
        heights,
        vec![Some((3.0, 6.0)), Some((3.7, 6.7))],
        "each frame must carry the sounding for its own hour",
    );
}

/// An hour with no sounding renders nothing rather than borrowing another hour's.
///
/// The substitution is the defect: a frame classified against an atmosphere that
/// was never over it looks exactly as plausible as a correct one.
#[test]
fn a_frame_whose_hour_has_no_sounding_borrows_no_other_hours() {
    let posted = Arc::new(Mutex::new(Vec::new()));
    let _worker =
        squallar_worker::offload::install_test_worker(Box::new(Recorder(Arc::clone(&posted))));

    let mut app = crate::app::tests::headless(TestBridge::desktop());
    let site = squallar_radar::sites::get_radar_site(SITE)
        .expect("KTLX is a real radar")
        .clone();
    app.gui.pane_mut(0).unwrap().set_site(SITE.to_string());
    app.render.ensure_pane_count(1);
    app.render
        .set_env_heights(SITE, sounding_for(12, 3.0), &app.gui);

    let params = || crate::render_dispatch::RenderParams {
        product: squallar_radar::types::RadarProduct::HydrometeorClassification,
        elevation: 0.5,
        lat: site.lat,
        lon: site.lon,
    };
    let target = || squallar_egui::pane::RenderTarget {
        site: SITE.to_string(),
        product: squallar_radar::fields::known::HYDROMETEOR_CLASSIFICATION,
        elevation: 0.5,
    };
    app.loop_mgr.cache_scan(
        SITE,
        volume_at(18),
        (std::sync::Arc::new(dual_pol_scan()), Default::default()),
    );
    assert!(app.spawn_loop_frame_render(0, volume_at(18), params(), target()));

    assert_eq!(
        dispatched_env_heights(&posted),
        vec![None],
        "the 18:00 frame took the 12:00 sounding",
    );
}

// ── The trigger ───────────────────────────────────────────────────────────

/// `spawn_level3_fetches` is keyed by site and two of its three callers have no
/// pane in scope, so the instants a sounding is owed for come from the panes on
/// that site. This is that fan-out.
mod span {
    use super::super::{SOUNDING_SPAN_CEILING_HOURS, sounding_span_for_site};
    use super::*;

    fn now() -> chrono::DateTime<chrono::Utc> {
        volume_at(12).and_utc()
    }

    fn app_on_site() -> crate::app::App {
        let mut app = crate::app::tests::headless(TestBridge::desktop());
        app.gui.pane_mut(0).unwrap().set_site(SITE.to_string());
        app.render.ensure_pane_count(1);
        app
    }

    /// A live pane with no lookback needs the current hour and nothing else.
    #[test]
    fn a_live_still_pane_asks_for_the_current_hour() {
        let mut app = app_on_site();
        app.gui.pane_mut(0).unwrap().time.span_secs = 0;
        assert_eq!(
            sounding_span_for_site(app.gui.panes(), SITE, now()),
            Some((now(), now())),
        );
    }

    /// **Defect 1's trigger.** A pane parked in the past must make the fetch ask
    /// about the past: the instant reaches the request, so the sounding that
    /// lands is the one that was over the site then.
    #[test]
    fn a_scrubbed_pane_moves_the_span_off_the_wall_clock() {
        let mut app = app_on_site();
        let pane = app.gui.pane_mut(0).unwrap();
        pane.time.span_secs = 0;
        pane.time.mode = squallar_egui::pane::TimeMode::AsOf(volume_at(3));
        let span =
            sounding_span_for_site(app.gui.panes(), SITE, now()).expect("a pane is on this site");
        assert_eq!(span, (volume_at(3).and_utc(), volume_at(3).and_utc()));
        assert_ne!(span.1, now(), "the span still ends at the wall clock");
    }

    /// A looping pane needs every hour its lookback covers, because each frame
    /// resolves its own.
    #[test]
    fn a_looping_pane_asks_for_its_whole_lookback() {
        let mut app = app_on_site();
        app.gui.pane_mut(0).unwrap().time.span_secs = 6 * 3600;
        assert_eq!(
            sounding_span_for_site(app.gui.panes(), SITE, now()),
            Some((now() - chrono::Duration::hours(6), now())),
        );
    }

    /// A lookback wider than the slider can produce is clamped, so one request
    /// can never become thousands of rows.
    #[test]
    fn a_pathological_lookback_is_clamped_to_the_ceiling() {
        let mut app = app_on_site();
        app.gui.pane_mut(0).unwrap().time.span_secs = 400 * 24 * 3600;
        let (from, to) =
            sounding_span_for_site(app.gui.panes(), SITE, now()).expect("a pane is on this site");
        assert_eq!(
            to.signed_duration_since(from).num_hours(),
            SOUNDING_SPAN_CEILING_HOURS,
        );
    }

    /// No pane on the site is no sounding owed.
    #[test]
    fn a_site_no_pane_shows_is_owed_nothing() {
        let app = app_on_site();
        assert_eq!(sounding_span_for_site(app.gui.panes(), "KOUN", now()), None);
    }
}
