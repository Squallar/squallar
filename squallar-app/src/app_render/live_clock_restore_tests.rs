//! **A pane flagged live depicts now once no loop owns its clock — after its
//! loop stops, after a reopen, from a file an older build wrote — and a paused
//! loop's chosen frame comes back through a re-arm and a reopen.**
//!
//! The report, 2026-09-12, on web, mobile and desktop alike: "I have to go set
//! the time to now, then click the live button for MDs and NWS alerts and other
//! sources to show up" — seen right after opening the app.
//!
//! The state was `TimeMode::AsOf(t)` on a pane whose `viewing_live` was still
//! `true`. `advance_loop_playback` writes it on every tick — the clock walks the
//! frames while the selection keeps following the live site — and nothing moved
//! the clock back when the loop ended, while `ui_config` persisted both halves.
//! Every layer with a past was asked about `t` through `as_of_for_layer` (SPC
//! discussions from the IEM archive instead of `spcmdrss.xml`, NWS alerts
//! filtered to the ones in force at `t`), `parked_panes` queued an archive seek
//! for `t` on a pane the live feed still served, and the Live button, reading
//! `viewing_live`, drew as live and ignored its click.
//!
//! What holds the answer is `PaneState::time_mode`: a live pane depicts now
//! unless its clock is the playhead of the loop it is running or waiting to
//! re-arm. These tests drive the app's own paths through it — the loop's end, a
//! re-arm through `EnableLoop`, the save and reopen, an older build's file, a
//! step back — and read what the layers and the startup radar seek are handed.
//!
//! **The non-vacuity rows** are the user's own parks and a paused loop's chosen
//! frame: kept, and still queuing their startup seek, so "always go live" fails
//! here as surely as the defect does.

use std::rc::Rc;

use squallar_egui::actions::GuiAction;
use squallar_egui::pane::{LayerTimeState, LoopPhase, TimeMode};
use squallar_egui::shell_api::GuiEvent;
use squallar_kv::KvStore;
use squallar_source::id::known;

use super::loop_playback_transport_tests::textured_frames;
use crate::app::tests::headless;
use crate::platform_double::TestBridge;

/// Frames well behind the thirty minutes after which the alerts and
/// discussions layers stop asking their live feeds, so what the reopened pane
/// asks for is unambiguous. A user who closed the app on a loop and came back
/// the next morning is many hours past it.
///
/// Whole seconds, because the config writes `as_of` to the second: a park that
/// comes back truncated is the file's precision, not the defect.
fn stale_stamps(now: chrono::NaiveDateTime) -> Vec<chrono::NaiveDateTime> {
    use chrono::Timelike;
    let now = now
        .with_nanosecond(0)
        .expect("zero nanoseconds is a valid time");
    let hour = chrono::Duration::hours(1);
    vec![now - hour * 4, now - hour * 3, now - hour * 2]
}

/// Give pane 0's radar transport `stamps` as rendered frames, ready to play —
/// what the listing, the downloads and the renders leave once they land.
fn frames_landed(app: &mut crate::app::App, ctx: &egui::Context, stamps: &[chrono::NaiveDateTime]) {
    let ls = app.gui.pane_mut(0).expect("pane 0").transport_state_mut();
    ls.frames = textured_frames(ctx, stamps);
    ls.phase = LoopPhase::Ready;
}

/// Arm a radar loop on pane 0 over `stamps` the way the app arms one — the
/// pane's lineage door, then the timeline — start it the way the frame pump
/// does, and play one tick with its interval already elapsed.
fn play_one_tick(app: &mut crate::app::App, ctx: &egui::Context, stamps: &[chrono::NaiveDateTime]) {
    {
        let pane = app.gui.pane_mut(0).expect("pane 0");
        pane.begin_or_continue_loop();
        *pane.transport_state_mut() = LayerTimeState::begin(
            3600,
            squallar_radar::types::RenderView::PlanView,
            Box::new(()),
        );
    }
    frames_landed(app, ctx, stamps);
    app.sync_loop_playback_start();
    app.gui
        .pane_mut(0)
        .expect("pane 0")
        .transport_state_mut()
        .last_advance = None;
    app.advance_loop_playback();
}

/// Put a radar scan on pane 0, stamped `at`, so a radar loop can anchor.
fn scan_on_pane_0(app: &mut crate::app::App, at: chrono::NaiveDateTime) {
    let site = app.gui.pane(0).expect("pane 0").site().to_string();
    app.gui.apply(GuiEvent::ScanInfoForPane {
        pane_idx: 0,
        info: squallar_radar::types::ScanInfo::from_scan(
            &crate::app::tests::empty_scan(),
            &site,
            at,
            None,
        ),
    });
}

/// The instant pane 0 of `app` asks `id`'s source about.
fn asked_instant(
    app: &crate::app::App,
    id: &squallar_source::id::LayerId,
) -> chrono::NaiveDateTime {
    crate::app::fetch::fetch_config_for_layer(&app.gui, 0, id, app.fetch_config()).as_of
}

/// The discussions and alerts layers on pane 0 are asked about now.
fn asks_about_now(app: &crate::app::App, why: &str) {
    let now = chrono::Utc::now().naive_utc();
    for id in [known::SPC_DISCUSSIONS, known::NWS_ALERTS] {
        let asked = asked_instant(app, &id);
        assert!(
            now - asked < chrono::Duration::minutes(1),
            "{why}: {} is asked about {asked}, {} minutes before now — the \
             archive, not the live feed",
            id.as_str(),
            (now - asked).num_minutes(),
        );
    }
}

/// **The in-session half: a live pane whose loop stops depicts now.** A pane
/// the user parked keeps its instant.
#[test]
fn stopping_playback_on_a_live_pane_returns_its_clock_to_live() {
    let ctx = egui::Context::default();
    let stamps = stale_stamps(chrono::Utc::now().naive_utc());

    for (viewing_live, expected, why) in [
        (true, TimeMode::Live, "a pane following live data"),
        (
            false,
            TimeMode::AsOf(stamps[0]),
            "a pane the user parked on an instant",
        ),
    ] {
        let mut app = headless(TestBridge::desktop());
        play_one_tick(&mut app, &ctx, &stamps);
        {
            let pane = app.gui.pane_mut(0).expect("pane 0");
            assert!(
                pane.transport_state().is_playing(),
                "premise: the loop is playing ({why})"
            );
            assert_eq!(
                pane.time_mode(),
                TimeMode::AsOf(stamps[0]),
                "premise: a playback tick parks the clock on the frame it shows ({why})"
            );
            assert!(
                pane.viewing_live(),
                "premise: the tick leaves the selection following live data ({why})"
            );
            pane.set_viewing_live(viewing_live);
        }

        app.handle_gui_action(GuiAction::DisableLoop { pane_idx: 0 }, None);

        let pane = app.gui.pane(0).expect("pane 0");
        assert!(
            !pane.transport_state().is_active(),
            "premise: the loop is off ({why})"
        );
        assert_eq!(
            pane.time_mode(),
            expected,
            "{why}: once the loop stops, the pane must depict {expected:?}. A live \
             pane left on the last frame's stamp hands every layer with a past \
             that stamp — discussions from the archive, alerts filtered to then",
        );
    }
}

/// **A playing live loop closed and reopened depicts now**, the discussions and
/// alerts layers ask their sources about now, and no startup radar seek is
/// queued at the frame that happened to be up. A pane the user parked reopens
/// parked, with its seek.
#[test]
fn a_live_pane_closed_while_playing_reopens_depicting_now() {
    let ctx = egui::Context::default();
    let stamps = stale_stamps(chrono::Utc::now().naive_utc());

    for (viewing_live, expect_live, why) in [
        (true, true, "a pane following live data"),
        (false, false, "a pane the user parked on an instant"),
    ] {
        let store = Rc::new(squallar_kv::MemoryKvStore::default());
        {
            let mut first = headless(TestBridge::desktop().with_store(Rc::clone(&store)));
            play_one_tick(&mut first, &ctx, &stamps);
            let pane = first.gui.pane_mut(0).expect("pane 0");
            assert_eq!(
                pane.time_mode(),
                TimeMode::AsOf(stamps[0]),
                "premise: the clock is on the loop's frame when the app closes ({why})"
            );
            pane.set_viewing_live(viewing_live);
            first.gui.save_ui_config(store.as_ref());
        }

        let reopened = headless(TestBridge::desktop().with_store(Rc::clone(&store)));
        let pane = reopened.gui.pane(0).expect("pane 0");
        assert_eq!(
            pane.viewing_live(),
            viewing_live,
            "premise: the selection posture survives the reopen ({why})"
        );

        if expect_live {
            assert_eq!(
                pane.time_mode(),
                TimeMode::Live,
                "{why}: reopened depicting the stamp the playing loop was on when \
                 the app closed, with its Live button painted live",
            );
            asks_about_now(&reopened, why);
            assert!(
                reopened.parked_fetch_pending.is_empty(),
                "{why}: a startup radar seek was queued at {:?} for a pane the live \
                 feed serves",
                reopened.parked_fetch_pending,
            );
        } else {
            assert_eq!(
                pane.time_mode(),
                TimeMode::AsOf(stamps[0]),
                "{why}: a park the user made must survive the reopen",
            );
            assert_eq!(
                asked_instant(&reopened, &known::SPC_DISCUSSIONS),
                stamps[0],
                "{why}: and a parked pane's discussions are the ones valid then",
            );
            assert_eq!(
                reopened
                    .parked_fetch_pending
                    .iter()
                    .map(|(idx, _, at)| (*idx, *at))
                    .collect::<Vec<_>>(),
                vec![(0, stamps[0])],
                "{why}: and its startup seek fetches the volume it is parked on",
            );
        }
    }
}

/// **A live loop paused on a chosen frame keeps it through a re-arm and a
/// reopen** — reopen is exactly 1:1, and a lookback change is not a reason to
/// lose the frame either.
///
/// Played, paused and stepped through the real transport actions. The re-arm is
/// the real `EnableLoop`, which replaces the transport's timeline through
/// `handle_enable_loop`; the reopen is the real save and restore, drained by
/// `hydrate_parked_panes` as the first redraw does. After each, the frames land
/// and playback's start runs: the loop must still be on the chosen frame, and
/// the reopen must still queue the startup seek to it that main queues.
#[test]
fn a_paused_live_loops_chosen_frame_survives_a_re_arm_and_a_reopen() {
    let ctx = egui::Context::default();
    let stamps = stale_stamps(chrono::Utc::now().naive_utc());
    let chosen = stamps[1];
    let store = Rc::new(squallar_kv::MemoryKvStore::default());

    let mut app = headless(TestBridge::desktop().with_store(Rc::clone(&store)));
    play_one_tick(&mut app, &ctx, &stamps);
    app.handle_gui_action(GuiAction::ToggleLoopPlayback { pane_idx: 0 }, None);
    app.handle_gui_action(
        GuiAction::StepLoopFrame {
            pane_idx: 0,
            forward: true,
        },
        None,
    );
    {
        let pane = app.gui.pane(0).expect("pane 0");
        assert!(
            !pane.transport_state().is_playing() && pane.viewing_live(),
            "premise: a live pane with its loop paused"
        );
        assert_eq!(
            pane.time_mode(),
            TimeMode::AsOf(chosen),
            "premise: the user stepped the paused loop to its second frame"
        );
    }
    app.gui.save_ui_config(store.as_ref());

    // ── The re-arm in place: a lookback change emits EnableLoop ───────────────
    scan_on_pane_0(&mut app, stamps[2]);
    app.handle_gui_action(
        GuiAction::EnableLoop {
            pane_idx: 0,
            lookback_secs: 3600,
        },
        None,
    );
    {
        let pane = app.gui.pane(0).expect("pane 0");
        assert_eq!(
            pane.transport_state().phase,
            LoopPhase::FetchingScanList,
            "premise: EnableLoop replaced the transport's timeline with a new listing"
        );
        assert_eq!(
            pane.time_mode(),
            TimeMode::AsOf(chosen),
            "the re-arm lost the frame the user paused on"
        );
    }
    frames_landed(&mut app, &ctx, &stamps);
    app.sync_loop_playback_start();
    assert_eq!(
        app.gui.pane(0).expect("pane 0").time_mode(),
        TimeMode::AsOf(chosen),
        "the re-armed loop started somewhere other than the frame the user paused on"
    );

    // ── The reopen ─────────────────────────────────────────────────────────
    let mut reopened = headless(TestBridge::desktop().with_store(Rc::clone(&store)));
    {
        let pane = reopened.gui.pane(0).expect("pane 0");
        assert!(
            pane.viewing_live() && pane.loop_arm_pending.is_some_and(|arm| !arm.playing),
            "premise: the pane reopened following live data with a paused loop to arm"
        );
        assert_eq!(
            pane.time_mode(),
            TimeMode::AsOf(chosen),
            "the reopen lost the frame the user paused on"
        );
    }
    assert_eq!(
        reopened
            .parked_fetch_pending
            .iter()
            .map(|(idx, _, at)| (*idx, *at))
            .collect::<Vec<_>>(),
        vec![(0, chosen)],
        "and its startup seek must fetch the frame it reopened on",
    );
    scan_on_pane_0(&mut reopened, chosen);
    reopened.hydrate_parked_panes();
    {
        let pane = reopened.gui.pane(0).expect("pane 0");
        assert!(
            pane.transport_state().is_active() && pane.loop_arm_pending.is_none(),
            "premise: the restored loop armed"
        );
        assert_eq!(
            pane.time_mode(),
            TimeMode::AsOf(chosen),
            "arming the restored loop lost the frame the user paused on"
        );
    }
    frames_landed(&mut reopened, &ctx, &stamps);
    reopened.sync_loop_playback_start();
    assert_eq!(
        reopened.gui.pane(0).expect("pane 0").time_mode(),
        TimeMode::AsOf(chosen),
        "the restored loop started somewhere other than the frame the user paused on"
    );
}

/// **A step back from a pane depicting now steps back from now.**
///
/// A pane over a stored clock no loop owns depicts now. Back clears the flag
/// before its navigation lands, and `nav_instant` falls through to the pane's
/// clock when the pane holds no scan and no data time — so a flag cleared
/// without writing the depicted clock down stepped from the dead instant, ten
/// minutes before a clock hours old. Radar on and off, because radar's arm
/// takes the seek path and the other the plain selection.
#[test]
fn a_step_back_from_a_pane_depicting_now_steps_back_from_now() {
    let dead = stale_stamps(chrono::Utc::now().naive_utc())[0];

    for radar_on in [false, true] {
        let mut app = headless(TestBridge::desktop());
        {
            let pane = app.gui.pane_mut(0).expect("pane 0");
            pane.set_overlay_enabled(known::RADAR, radar_on);
            pane.set_time_mode(TimeMode::AsOf(dead));
            assert_eq!(
                pane.time_mode(),
                TimeMode::Live,
                "premise: the pane depicts now (radar {radar_on})"
            );
            assert!(
                pane.scan_info.is_none() && pane.data_time_on_screen().is_none(),
                "premise: nothing on the pane names an instant but its clock"
            );
            // What the Back button does before its navigation is handled.
            pane.set_viewing_live(false);
        }
        let before = chrono::Utc::now().naive_utc();
        app.handle_gui_action(
            GuiAction::NavigateTime {
                pane_idx: 0,
                step_secs: -600,
            },
            None,
        );
        let after = chrono::Utc::now().naive_utc();

        let Some(landed) = app.gui.pane(0).expect("pane 0").time_mode().as_of() else {
            panic!("premise: a step back parks the pane (radar {radar_on})");
        };
        let ten = chrono::Duration::minutes(10);
        assert!(
            landed >= before - ten && landed <= after - ten,
            "radar {radar_on}: a step back from a pane depicting now landed on \
             {landed}, not ten minutes before now — {} is ten minutes before the \
             dead clock",
            dead - ten,
        );
    }
}

/// **The file users already have: a live flag over a parked clock, no loop.**
///
/// What the deployed build writes for a live pane whose loop was switched off
/// before closing: `as_of` present, `viewing_live` absent (it serialises only
/// when false), `loop_playback` absent. The fix has to hold for that file as
/// it stands on disk, so this writes it the way the app does and then takes
/// away the one key an older build would not have written.
///
/// Opening it must depict now, ask the overlays about now, and queue **no**
/// startup radar seek at the stale instant — the seek the radar lane's probe
/// showed fetching the archive volume, installing it on the live-flagged pane
/// and leaving the live feed to move it forward minutes later. Run through
/// `hydrate_parked_panes` as the first redraw does, so the spinner and the
/// navigation guard are read after the drain, not before it.
///
/// **Non-vacuity**: the same file with `viewing_live: false` — a park the user
/// made — opens parked, queues its seek at that instant, and the drain raises
/// the spinner and the guard.
#[test]
fn an_older_config_with_a_live_flag_and_a_parked_clock_opens_live_and_seeks_nothing() {
    let stale = stale_stamps(chrono::Utc::now().naive_utc())[0];

    for (keep_live_key_false, why) in [
        (false, "an older file: parked clock, live flag, no loop"),
        (
            true,
            "a file the user parked: parked clock, viewing_live false",
        ),
    ] {
        let store = Rc::new(squallar_kv::MemoryKvStore::default());
        {
            let mut writer = squallar_egui::Gui::new();
            let pane = writer.pane_mut(0).expect("pane 0");
            pane.set_viewing_live(false);
            pane.set_time_mode(TimeMode::AsOf(stale));
            writer.save_ui_config(store.as_ref());
        }
        if !keep_live_key_false {
            let text = store
                .load(squallar_egui::UI_CONFIG_KEY)
                .expect("the writer saved a config");
            let mut json: serde_json::Value =
                serde_json::from_str(&text).expect("the saved config is JSON");
            let pane = json["panes"][0]
                .as_object_mut()
                .expect("the saved config has a pane object");
            assert_eq!(
                pane.remove("viewing_live"),
                Some(serde_json::Value::Bool(false)),
                "premise: the parked writer spelled the flag, so removing it is \
                 what turns this into the live-flagged file",
            );
            assert!(
                pane.contains_key("as_of"),
                "premise: the file carries the parked clock"
            );
            store
                .store(squallar_egui::UI_CONFIG_KEY, &json.to_string())
                .expect("the edited config stores");
        }

        let mut opened = headless(TestBridge::desktop().with_store(Rc::clone(&store)));
        let viewing_live = !keep_live_key_false;
        {
            let pane = opened.gui.pane(0).expect("pane 0");
            assert_eq!(
                pane.viewing_live(),
                viewing_live,
                "premise: the flag reads as the file spells it ({why})"
            );
            assert!(
                pane.is_overlay_enabled(&known::RADAR) && !pane.site().is_empty(),
                "premise: the pane draws radar on a site, so a parked restore would \
                 queue a seek ({why})"
            );
        }

        if viewing_live {
            assert_eq!(
                opened.gui.pane(0).expect("pane 0").time_mode(),
                TimeMode::Live,
                "{why}: the pane opened depicting {stale}, which is the report — \
                 overlays with a past missing until Set Time, then Live",
            );
            asks_about_now(&opened, why);
            assert!(
                opened.parked_fetch_pending.is_empty(),
                "{why}: a startup radar seek was queued at {:?} for a pane flagged \
                 live",
                opened.parked_fetch_pending,
            );
        } else {
            assert_eq!(
                opened.gui.pane(0).expect("pane 0").time_mode(),
                TimeMode::AsOf(stale),
                "{why}: the park must survive"
            );
            assert_eq!(
                opened
                    .parked_fetch_pending
                    .iter()
                    .map(|(idx, _, at)| (*idx, *at))
                    .collect::<Vec<_>>(),
                vec![(0, stale)],
                "{why}: its startup seek must fetch the instant it is parked on",
            );
        }

        opened.hydrate_parked_panes();
        assert_eq!(
            opened.manual_nav_pending,
            !viewing_live,
            "{why}: the startup drain {} guard the pane against the live poll",
            if viewing_live { "must not" } else { "must" },
        );
        assert_eq!(
            opened.gui.fetching(),
            !viewing_live,
            "{why}: the startup drain {} fetch a volume",
            if viewing_live { "must not" } else { "must" },
        );
    }
}

/// **The radar lane's startup probe, on this tree: a pane set live over a
/// parked clock queues no startup seek.**
///
/// The probe that lane ran set pane 0 to `viewing_live == true` over
/// `AsOf(2025-01-01)` directly, then followed the restore: `parked_panes` named
/// the pane at that instant, the drain set `manual_nav_pending` and asked for
/// the archive volume, the reply was installed on the live-flagged pane, and a
/// live volume closing later moved it forward — the radar showing a wrong image
/// and correcting itself minutes afterwards.
///
/// Here the same scene is built the same way and asserted at the first link:
/// `parked_panes` reads the clock through `PaneState::time_mode`, so a live
/// pane whose clock no loop owns is not parked at all, and draining the queue
/// asks for nothing. **Control**: the same pane with its live flag cleared — a
/// park the user made — is queued at that instant, and the drain raises the
/// guard and the spinner.
#[test]
fn a_pane_set_live_over_a_parked_clock_queues_no_startup_seek() {
    const SITE: &str = "KTLX";
    let parked_at = chrono::NaiveDate::from_ymd_opt(2025, 1, 1)
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .expect("a real instant");

    for (viewing_live, why) in [
        (true, "a pane set live over a parked clock, no loop"),
        (false, "a pane the user parked at that instant"),
    ] {
        let mut app = headless(TestBridge::desktop());
        {
            let pane = app.gui.pane_mut(0).expect("pane 0");
            pane.set_site(SITE.to_string());
            pane.set_overlay_enabled(known::RADAR, true);
            if !viewing_live {
                pane.set_viewing_live(false);
            }
            pane.set_time_mode(TimeMode::AsOf(parked_at));
        }

        let parked = crate::app::parked_panes(&app.gui);
        if viewing_live {
            assert!(
                parked.is_empty(),
                "{why}: the restore queued a seek at {parked:?} for a pane the live \
                 feed serves — the archive volume the radar lane saw installed over \
                 live data",
            );
        } else {
            assert_eq!(
                parked,
                vec![(0, SITE.to_string(), parked_at)],
                "{why}: a park the user made must still fetch its volume",
            );
        }

        app.parked_fetch_pending = parked;
        app.hydrate_parked_panes();
        assert_eq!(
            app.manual_nav_pending,
            !viewing_live,
            "{why}: the startup drain {} guard the pane against the live poll",
            if viewing_live { "must not" } else { "must" },
        );
        assert_eq!(
            app.gui.fetching(),
            !viewing_live,
            "{why}: the startup drain {} fetch a volume",
            if viewing_live { "must not" } else { "must" },
        );
    }
}
