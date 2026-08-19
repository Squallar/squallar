//! WO-E2 contract test 2 of 3: **GuiAction replay**. A scripted action batch
//! fed through `process_gui_actions` — the production dispatch — produces
//! state transitions that are visible *through the seam*: event-shaped
//! effects land via `Gui::apply` at the handler's own position, and
//! frame-composed facts land on the next `push_frame_inputs`, which the test
//! drives exactly where `setup_egui_frame` would (no renderer exists here to
//! run a frame).
//!
//! The plan scripted `FetchRadarScan` for the fetching/config assertions;
//! that handler has never written either — the *emitter* raises the spinner
//! (`check_auto_polls` / the refresh paths, inside `rustdar-egui`) and the
//! handler only spawns the download. `NavigateTime` and `JumpToLive` are the
//! action paths that really carry those transitions, so they carry the
//! assertions.

use super::tests::{empty_scan, n_pane_app};
use rustdar_egui::actions::GuiAction;
use rustdar_egui::shell_api::GuiEvent;
use rustdar_radar::types::ScanInfo;

/// The seeded scan time: a fixed instant so the stepped timestamp the config
/// must carry is exact, not approximate.
fn seeded_utc() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2024, 1, 1)
        .expect("a real date")
        .and_hms_opt(0, 0, 0)
        .expect("a real time")
}

#[test]
fn a_scripted_action_batch_lands_through_the_seam() {
    let mut app = n_pane_app(2, "KTLX");

    // Seed both panes' scan info through the seam itself — the same event the
    // scan drain applies — so NavigateTime has a current time to step from.
    for pane_idx in 0..2 {
        app.gui.apply(GuiEvent::ScanInfoForPane {
            pane_idx,
            info: ScanInfo::from_scan(&empty_scan(), "KTLX", seeded_utc(), None),
        });
    }
    // Seed a fix the way `poll_platform_state` records one: into the App's
    // own field, stamped at arrival, visible after the next compose.
    app.user_gps = Some((
        rustdar_location::Fix::from_lat_lon(35.25, -97.5),
        web_time::Instant::now(),
    ));
    app.push_frame_inputs();
    assert!(
        app.gui.gps_fix().is_some(),
        "precondition: the seeded fix reached the UI through the compose"
    );
    assert!(
        app.gui.pane(0).is_some_and(|p| p.viewing_live),
        "precondition: panes start live"
    );

    // The script, in one batch, in order:
    //  * NavigateTime pane 1 backwards — pane 1 leaves live;
    //  * JumpToLive pane 1 — pane 1 returns to live (a real false→true edge,
    //    since the step above just took it off);
    //  * NavigateTime pane 0 backwards — pane 0 leaves live, the spinner goes
    //    up, and the radar config carries the stepped timestamp (the batch's
    //    last config writer, so the expected value is exact);
    //  * StopLocation — the gate turns off and the fix's consent goes away.
    app.process_gui_actions(vec![
        GuiAction::NavigateTime {
            pane_idx: 1,
            step_secs: -300,
        },
        GuiAction::JumpToLive { pane_idx: 1 },
        GuiAction::NavigateTime {
            pane_idx: 0,
            step_secs: -300,
        },
        GuiAction::StopLocation,
    ]);

    // Event-shaped transitions are visible at once — they were applied at the
    // handlers' own control-flow positions via `Gui::apply`.
    assert!(
        !app.gui.pane(0).is_some_and(|p| p.viewing_live),
        "NavigateTime did not take pane 0 off live through the seam"
    );
    assert!(
        app.gui.pane(1).is_some_and(|p| p.viewing_live),
        "JumpToLive did not return pane 1 to live through the seam"
    );
    assert!(
        app.gui.fetching(),
        "the navigation did not raise the spinner through the seam"
    );
    let target = seeded_utc() - chrono::Duration::seconds(300);
    let expected_local = chrono::TimeZone::from_utc_datetime(&chrono::Local, &target).naive_local();
    assert_eq!(
        app.gui.get_radar_config().timestamp,
        expected_local,
        "the radar config does not carry the scripted step's timestamp"
    );

    // Frame-composed facts land on the next compose — driven here exactly
    // where `setup_egui_frame` drives it in production.
    app.push_frame_inputs();
    assert!(
        app.gui.gps_fix().is_none(),
        "StopLocation's cleared fix survived the next compose"
    );
    assert!(
        !app.gui.location_active(),
        "the location facts do not reflect the disabled gate"
    );
}
