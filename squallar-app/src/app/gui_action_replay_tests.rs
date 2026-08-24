use super::tests::{empty_scan, n_pane_app};
use squallar_egui::actions::GuiAction;
use squallar_egui::shell_api::GuiEvent;
use squallar_radar::types::ScanInfo;

fn seeded_utc() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2024, 1, 1)
        .expect("a real date")
        .and_hms_opt(0, 0, 0)
        .expect("a real time")
}

#[test]
fn a_scripted_action_batch_lands_through_the_seam() {
    let mut app = n_pane_app(2, "KTLX");

    for pane_idx in 0..2 {
        app.gui.apply(GuiEvent::ScanInfoForPane {
            pane_idx,
            info: ScanInfo::from_scan(&empty_scan(), "KTLX", seeded_utc(), None),
        });
    }
    app.user_gps = Some((
        squallar_location::Fix::from_lat_lon(35.25, -97.5),
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
        app.gui.selected_timestamp(),
        expected_local,
        "the radar config does not carry the scripted step's timestamp"
    );

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

/// **A forward step past `now` is "live" only on a transport that ends at
/// `now`** (WI-10). Two arms through the same seam, differing only in the
/// pane's transport layer:
///
/// - radar (control): the step clamps to the wall clock and reports the pane
///   live — byte-identical to what it always did;
/// - forecast: the target is a real instant on the transport's own axis, so
///   it is neither clamped nor reported live. The old code set
///   `viewing_live = true` here, which painted a pane depicting the future as
///   depicting now.
#[test]
fn a_forward_step_past_now_is_live_only_on_a_transport_that_ends_at_now() {
    use squallar_source::id::known;

    let mut app = n_pane_app(2, "KTLX");
    let scan_stamp = chrono::Utc::now().naive_utc() - chrono::Duration::seconds(60);
    for pane_idx in 0..2 {
        app.gui.apply(GuiEvent::ScanInfoForPane {
            pane_idx,
            info: ScanInfo::from_scan(&empty_scan(), "KTLX", scan_stamp, None),
        });
    }
    app.gui
        .pane_mut(1)
        .expect("pane 1")
        .set_transport_layer(known::MODEL_DATA);

    // -- Radar arm (control): clamped to now, and live -------------------
    app.process_gui_actions(vec![GuiAction::NavigateTime {
        pane_idx: 0,
        step_secs: 12 * 3600,
    }]);
    assert!(
        app.gui.pane(0).is_some_and(|p| p.viewing_live),
        "a radar pane stepped past now must clamp back to live, exactly as \
         it always did"
    );

    // -- Forecast arm: a real target, and not live -----------------------
    app.process_gui_actions(vec![GuiAction::NavigateTime {
        pane_idx: 1,
        step_secs: 12 * 3600,
    }]);
    assert!(
        !app.gui.pane(1).is_some_and(|p| p.viewing_live),
        "stepping onto a forecast instant reported the pane live: a pane \
         depicting twelve hours from now is not depicting now"
    );
    let target = scan_stamp + chrono::Duration::hours(12);
    let expected_local = chrono::TimeZone::from_utc_datetime(&chrono::Local, &target).naive_local();
    assert_eq!(
        app.gui.selected_timestamp(),
        expected_local,
        "the forecast step's target was clamped back to the wall clock \
         instead of naming the instant on the transport's own axis"
    );
}
