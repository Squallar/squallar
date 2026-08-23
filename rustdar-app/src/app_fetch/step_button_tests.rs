//! **WO-T3.10: the step buttons move a pane that holds no radar scan.**
//!
//! `handle_navigate_time` and `handle_navigate_one_scan` both read
//! `pane.scan_info` for the instant to step from and for the site to ask, and
//! **returned early without one** — so pressing either arrow on a
//! satellite-only or model-only pane did nothing whatsoever, while the step
//! dropdown offers `TimeStep::OneFrame` to any pane carrying a frame-series
//! layer (`Gui::pane_has_frame_series_layer`).
//!
//! Two halves had to change together, and the second is the one that makes a
//! press visible:
//!
//! 1. the instant to step *from* comes from `nav_instant`, which falls through
//!    to `PaneState::data_time_on_screen` — the transport playhead's stamp —
//!    for a pane with no scan;
//! 2. **the pane's clock moves.** `handle_navigate_time` pushed
//!    `ViewingLiveForPane` and `SelectedTime` and never touched
//!    `PaneState::set_time_mode`; the scrubber wrote the clock itself, in the
//!    UI, before emitting the same action. The three are one gesture now, as
//!    `GuiEvent::PaneTimeSelected`.
//!
//! The double is `reinit_active_tests`' `CoarseLayer`, staged with a resident
//! set: it is the same shape a satellite handler has and it reaches no bucket.

use super::reinit_active_tests::{Asked, CoarseLayer};
use super::*;
use crate::app::App;
use rustdar_egui::pane::TimeMode;
use rustdar_overlays::render::overlay_state::{OverlayHandler, OverlayRegistry};
use std::sync::Arc;

const SITE: &str = "KTLX";

fn satellite() -> LayerId {
    LayerId::new("test/coarse")
}

fn at(hour: u32, minute: u32) -> NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 4, 2)
        .expect("a real date")
        .and_hms_opt(hour, minute, 0)
        .expect("a real time")
}

/// The instants the satellite double is holding data for — three hours, so a
/// step has somewhere to land on either side of the middle one.
fn staged() -> Vec<NaiveDateTime> {
    vec![at(3, 0), at(4, 0), at(5, 0)]
}

/// A one-pane app whose only layer is the satellite double.
///
/// **No radar handler is registered and the pane holds no `ScanInfo`**, which
/// is exactly the pane the old early-returns refused: everything below is
/// about what a press does to it.
fn satellite_only_app() -> (App, Asked) {
    let asked: Asked = Default::default();
    let mut app = crate::app::tests::n_pane_app(1, SITE);
    app.gui.overlays = OverlayRegistry::with_handlers(vec![Box::new(CoarseLayer {
        id: satellite(),
        min_frames: 13,
        resident: staged(),
        asked: Arc::clone(&asked),
    }) as Box<dyn OverlayHandler>]);
    let pane = app.gui.pane_mut(0).expect("one pane");
    pane.set_transport_layer(satellite());
    pane.set_overlay_enabled(known::RADAR, false);
    pane.data_time = Some(at(4, 0));
    assert!(pane.scan_info.is_none(), "fixture: no radar scan");
    (app, asked)
}

fn clock(app: &App) -> TimeMode {
    app.gui.pane(0).expect("one pane").time.mode
}

/// **WO-T3.10's acceptance.** Both step buttons move a satellite-only pane.
///
/// Before the fix each handler's first statement was
/// `let Some(scan_info) = … else { return }`, so every assertion below reads
/// the fixture's own untouched state: the clock stays `Live`, `viewing_live`
/// stays true, and the Set Time dialog still shows whatever it showed.
///
/// **Floors** — (a) the relative step lands on the instant on *screen* minus
/// the step, not on the wall clock, so "it moved" is distinguishable from "it
/// jumped to now"; (b) the one-frame step lands on a stamp the transport
/// actually holds rather than on `current - typical_step`, which is what makes
/// it a *frame* step; (c) the frame step in each direction, so a fixed
/// direction cannot pass it.
#[test]
fn the_step_buttons_move_a_satellite_only_pane() {
    // ── The relative step: "back 10 min" ───────────────────────────────────
    let (mut app, _) = satellite_only_app();
    assert_eq!(
        clock(&app),
        TimeMode::Live,
        "precondition: a fresh pane's clock is live",
    );
    app.handle_gui_action(
        GuiAction::NavigateTime {
            pane_idx: 0,
            step_secs: -600,
        },
        None,
    );
    // The selection, and then the clock — asserted in that order because the
    // two are the halves of one gesture and a half-fix moves only the first.
    // (a) `at(4, 0)` is the pane's `data_time`, which is what
    // `data_time_on_screen` answers with no timeline armed.
    let want = at(4, 0) - chrono::Duration::seconds(600);
    assert_eq!(
        app.gui.selected_timestamp(),
        chrono::TimeZone::from_utc_datetime(&chrono::Local, &want).naive_local(),
        "a relative step did not move a satellite-only pane's SELECTION; the \
         handler returned before touching anything, which is the early return \
         on `scan_info`",
    );
    assert_eq!(
        clock(&app),
        TimeMode::AsOf(want),
        "a relative step moved the selection above and left the pane's CLOCK \
         where it was, so no layer on the pane can see the step — the half of \
         the gesture `GuiEvent::PaneTimeSelected` exists to keep together. \
         Anything near the wall clock instead is the step being taken from \
         `now` rather than from the instant on screen",
    );
    assert!(
        !app.gui.pane(0).expect("one pane").viewing_live,
        "the pane still claims to be following live data after a step back",
    );

    // ── The one-frame step, backward and forward ──────────────────────────
    for (forward, want) in [(false, at(3, 0)), (true, at(5, 0))] {
        let (mut app, _) = satellite_only_app();
        app.handle_gui_action(
            GuiAction::NavigateOneScan {
                pane_idx: 0,
                forward,
            },
            None,
        );
        assert_eq!(
            clock(&app),
            TimeMode::AsOf(want),
            "a one-frame step {} did not land on the stamp the transport \
             holds. `Live` is the untouched fixture; {} would be \
             `current - typical_step` rather than a frame the layer named",
            if forward { "forward" } else { "back" },
            if forward { at(5, 0) } else { at(3, 0) },
        );
    }

    // (b) Non-triviality: the staged stamps are an hour apart and the relative
    // step above is ten minutes, so no assertion here can be satisfied by the
    // other handler's arithmetic.
    assert_eq!(
        staged().len(),
        3,
        "fixture: a stamp on either side of the one on screen, or the two \
         directions above prove nothing",
    );
}

/// **A one-frame step with nowhere to go moves nothing**, rather than parking
/// the pane on an instant its transport cannot draw.
#[test]
fn a_one_frame_step_past_the_transports_own_edge_moves_nothing() {
    let (mut app, _) = satellite_only_app();
    app.gui.pane_mut(0).expect("one pane").data_time = Some(at(5, 0));

    app.handle_gui_action(
        GuiAction::NavigateOneScan {
            pane_idx: 0,
            forward: true,
        },
        None,
    );

    assert_eq!(
        clock(&app),
        TimeMode::Live,
        "the pane was parked past the newest frame its transport holds, where \
         it draws nothing at all",
    );
}

/// **The floor: a radar pane steps exactly as it did.**
///
/// Its base instant is still `scan_info.timestamp` and not the raster on
/// screen — which the fixture makes visible by setting the two an hour apart —
/// and the one-frame step is still the archive walk rather than a stamp read
/// off a frame list radar deliberately does not keep.
#[test]
fn a_radar_panes_step_is_unchanged() {
    let mut app = crate::app::tests::n_pane_app(1, SITE);
    let scan_at = at(6, 0);
    {
        let pane = app.gui.pane_mut(0).expect("one pane");
        assert_eq!(
            pane.transport_layer(),
            &known::RADAR,
            "precondition: a radar pane's transport addresses radar",
        );
        pane.scan_info = Some(rustdar_radar::types::ScanInfo::from_scan(
            &crate::app::tests::empty_scan(),
            SITE,
            scan_at,
            None,
        ));
        // An hour behind the selection: `data_time` is the raster ON SCREEN,
        // which lags the scan the pane has selected. Preferring it here would
        // step a radar pane from the wrong instant, which is why `nav_instant`
        // asks `scan_info` first.
        pane.data_time = Some(scan_at - chrono::Duration::hours(1));
    }

    app.handle_gui_action(
        GuiAction::NavigateTime {
            pane_idx: 0,
            step_secs: -600,
        },
        None,
    );

    let want = scan_at - chrono::Duration::seconds(600);
    assert_eq!(
        app.gui.selected_timestamp(),
        chrono::TimeZone::from_utc_datetime(&chrono::Local, &want).naive_local(),
        "a radar pane stepped from somewhere other than the scan it has \
         selected; one hour off is `data_time`, the raster on screen",
    );
    assert!(
        !app.gui.pane(0).expect("one pane").viewing_live,
        "a step back left the pane claiming to follow live data",
    );
}
