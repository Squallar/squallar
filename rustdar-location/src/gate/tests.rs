use super::GateDouble;
use super::*;
use rustdar_kv::MemoryKvStore;
use std::rc::Rc;

/// A gate and the bridge it drives, with the clock in the test's hands: offsets
/// are added to one reading, since every decision is on a *difference*.
struct Fixture {
    gate: LocationGate,
    bridge: GateDouble,
    t0: Instant,
    elapsed: Duration,
}

impl Fixture {
    fn new(bridge: GateDouble) -> Self {
        Self {
            gate: LocationGate::new(),
            bridge,
            t0: Instant::now(),
            elapsed: Duration::ZERO,
        }
    }

    fn wait(&mut self, by: Duration) -> &mut Self {
        self.elapsed += by;
        self
    }

    fn step(&mut self) -> LocationStep {
        let now = self.t0 + self.elapsed;
        self.gate.step_at(now, &mut self.bridge, false)
    }

    fn step_in_settings(&mut self) -> LocationStep {
        let now = self.t0 + self.elapsed;
        self.gate.step_at(now, &mut self.bridge, true)
    }

    fn step_after_the_cadence(&mut self) -> LocationStep {
        self.wait(POLL_INTERVAL).step()
    }
}

/// A desktop bridge whose store the test keeps, so a "restart" can hand the same
/// blobs to a fresh gate.
fn desktop_with_store(store: Rc<MemoryKvStore>) -> GateDouble {
    GateDouble::desktop().with_store(store)
}

/// The audited web defect, pinned: the browser prompted on first paint because
/// there was no way to say "I do not know yet".
#[test]
fn a_platform_that_has_not_answered_yet_is_never_prompted() {
    let mut f = Fixture::new(GateDouble::web().with_permission(LocationPermission::Unknown));

    for _ in 0..5 {
        f.step_after_the_cadence();
    }

    assert_eq!(
        f.bridge.location_requests(),
        0,
        "the app asked for location before the platform had said whether \
             anyone had been asked"
    );
    assert_eq!(f.gate.permission(), LocationPermission::Unknown);
}

#[test]
fn a_platform_without_location_is_never_asked_and_never_polled() {
    let mut f =
        Fixture::new(GateDouble::desktop().with_permission(LocationPermission::Unavailable));

    f.step();
    let after_first = f.bridge.permission_queries();
    for _ in 0..5 {
        f.step_after_the_cadence();
    }

    assert_eq!(f.bridge.location_requests(), 0);
    assert_eq!(
        f.bridge.permission_queries(),
        after_first,
        "a platform with no location service is still being polled about it"
    );
}

#[test]
fn a_remembered_denial_is_never_prompted_again() {
    let store = Rc::new(MemoryKvStore::default());
    let mut f = Fixture::new(
        desktop_with_store(Rc::clone(&store)).with_permission(LocationPermission::Denied),
    );
    for _ in 0..3 {
        f.step_after_the_cadence();
    }
    assert_eq!(f.bridge.location_requests(), 0);

    let mut f = Fixture::new(desktop_with_store(store).with_permission(LocationPermission::Denied));
    for _ in 0..3 {
        f.step_after_the_cadence();
    }

    assert_eq!(
        f.bridge.location_requests(),
        0,
        "a denial the user already gave produced a fresh prompt"
    );
}

#[test]
fn a_first_run_asks_for_location_once_and_only_once() {
    let mut f = Fixture::new(GateDouble::desktop().with_permission(LocationPermission::Prompt));

    for _ in 0..20 {
        f.wait(RETRY_INTERVAL).step();
    }

    assert_eq!(
        f.bridge.location_requests(),
        1,
        "the startup ask repeated; on Android that is a dialog the user \
             has to dismiss over and over"
    );
}

/// The one thing the `bool` from `request_location` is worth: Android can
/// genuinely fail to reach `Activity.requestPermissions`.
#[test]
fn an_ask_that_never_reached_the_os_is_tried_again_within_the_bound() {
    let mut f = Fixture::new(
        GateDouble::desktop()
            .with_permission(LocationPermission::Prompt)
            .with_request_reaching_the_os(false),
    );

    for _ in 0..10 {
        f.wait(RETRY_INTERVAL).step();
    }

    assert_eq!(
        f.bridge.location_requests(),
        usize::from(MAX_ATTEMPTS),
        "a failing ask either gave up after one try or retried without bound"
    );
}

/// **The regression the poll cadence exists for.** If polling stopped once the
/// ask was made, the user tapping Allow would be observed by nothing.
#[test]
fn a_grant_that_arrives_after_the_ask_still_starts_delivery() {
    let mut f = Fixture::new(GateDouble::desktop().with_permission(LocationPermission::Prompt));
    let permission = f.bridge.permission_cell();

    f.step();
    assert_eq!(f.bridge.location_requests(), 1, "the ask never happened");
    assert!(!f.bridge.location_active());

    // The user taps Allow, seconds later and on another thread.
    permission.set(LocationPermission::Granted);
    f.step_after_the_cadence();

    assert!(
        f.bridge.location_active(),
        "the grant landed after the ask and nothing noticed, so the app is \
             permitted to read the location and never does"
    );
    assert_eq!(f.gate.permission(), LocationPermission::Granted);
}

/// A dismissed dialog leaves the permission on `Prompt`, indistinguishable from
/// never having asked; the memo is the only thing that knows better.
#[test]
fn a_prompt_the_user_walked_away_from_is_not_repeated_on_the_next_run() {
    let store = Rc::new(MemoryKvStore::default());

    let mut first = Fixture::new(
        desktop_with_store(Rc::clone(&store)).with_permission(LocationPermission::Prompt),
    );
    first.step();
    assert_eq!(first.bridge.location_requests(), 1);

    // The app is closed with the dialog still unanswered, and reopened.
    let mut second =
        Fixture::new(desktop_with_store(store).with_permission(LocationPermission::Prompt));
    for _ in 0..10 {
        second.wait(RETRY_INTERVAL).step();
    }

    assert_eq!(
        second.bridge.location_requests(),
        0,
        "every launch re-prompts a user who has already been asked once"
    );
}

/// Trap (3). Web with site data blocked, and desktop with no `HOME`, are
/// platforms where location itself works fine.
#[test]
fn a_device_with_no_kv_is_still_asked() {
    let mut f = Fixture::new(
        GateDouble::web()
            .without_kv()
            .with_permission(LocationPermission::Prompt),
    );

    f.step();

    assert_eq!(
        f.bridge.location_requests(),
        1,
        "a device that cannot remember the answer was never asked the \
             question"
    );
}

/// Trap (4). Observed through an open settings window, because a grant that is
/// actually delivering is a *terminal* state for the cadence.
#[test]
fn a_revoked_permission_stops_delivery_and_clears_the_dot() {
    let mut f = Fixture::new(GateDouble::desktop().with_permission(LocationPermission::Granted));
    let permission = f.bridge.permission_cell();
    f.step();
    assert!(f.bridge.location_active(), "delivery never started");

    permission.set(LocationPermission::Denied);
    let outcome = f.wait(POLL_INTERVAL).step_in_settings();

    assert!(
        !f.bridge.location_active(),
        "the permission was revoked and the location stream kept running"
    );
    assert!(
        outcome.revoked,
        "nothing told the caller to take the dot off the map"
    );
}

/// The app's own switch, which is not the OS's.
#[test]
fn turning_location_off_in_settings_survives_a_restart() {
    let store = Rc::new(MemoryKvStore::default());
    let mut f = Fixture::new(
        desktop_with_store(Rc::clone(&store)).with_permission(LocationPermission::Granted),
    );
    f.step();
    assert!(f.bridge.location_active());

    f.gate.disable(&mut f.bridge);
    assert!(
        !f.bridge.location_active(),
        "the off switch did not switch off"
    );

    let mut next_run =
        Fixture::new(desktop_with_store(store).with_permission(LocationPermission::Granted));
    for _ in 0..3 {
        next_run.step_after_the_cadence();
    }

    assert!(
        !next_run.bridge.location_active(),
        "location came back on by itself after the user turned it off"
    );
}

/// Without this the bound is permanent and there is no way in.
#[test]
fn enabling_location_in_settings_asks_again_after_a_dismissal() {
    let store = Rc::new(MemoryKvStore::default());
    let mut f = Fixture::new(
        desktop_with_store(Rc::clone(&store)).with_permission(LocationPermission::Prompt),
    );
    f.step();
    assert_eq!(f.bridge.location_requests(), 1);

    // A later run, where the startup ask is correctly suppressed.
    let mut f = Fixture::new(desktop_with_store(store).with_permission(LocationPermission::Prompt));
    f.step();
    assert_eq!(f.bridge.location_requests(), 0);

    f.gate.enable(&mut f.bridge);
    f.step();

    assert_eq!(
        f.bridge.location_requests(),
        1,
        "the user asked for the prompt back and did not get it"
    );
}

/// A gesture resets the *counter*; it cannot reset the OS's own refusal.
#[test]
fn enabling_location_in_settings_cannot_re_ask_after_a_remembered_denial() {
    let mut f = Fixture::new(GateDouble::desktop().with_permission(LocationPermission::Denied));
    f.step();

    f.gate.enable(&mut f.bridge);
    for _ in 0..5 {
        f.wait(RETRY_INTERVAL).step();
    }

    assert_eq!(
        f.bridge.location_requests(),
        0,
        "the settings button re-asked a platform that has already refused"
    );
}

/// The other half of trap (1). On Android each of these is a JNI call.
#[test]
fn a_pending_permission_is_polled_on_a_bounded_cadence_not_every_frame() {
    let mut f = Fixture::new(GateDouble::desktop().with_permission(LocationPermission::Prompt));

    f.step();
    assert_eq!(f.bridge.permission_queries(), 1);
    // Sixty frames inside one interval, i.e. a second of a 60 Hz display.
    for _ in 0..60 {
        f.wait(POLL_INTERVAL / 120).step();
    }

    assert!(
        f.bridge.permission_queries() <= 2,
        "the platform was queried {} times in one poll interval",
        f.bridge.permission_queries()
    );
}

#[test]
fn an_open_settings_window_keeps_watching_a_settled_permission() {
    let mut f = Fixture::new(GateDouble::desktop().with_permission(LocationPermission::Denied));
    f.step();
    let settled = f.bridge.permission_queries();

    f.wait(POLL_INTERVAL).step();
    assert_eq!(
        f.bridge.permission_queries(),
        settled,
        "a settled permission is still being polled with nothing on screen \
             that reports it"
    );

    f.wait(SETTINGS_POLL_INTERVAL).step_in_settings();
    assert!(
        f.bridge.permission_queries() > settled,
        "the settings window is open and the state it shows has stopped \
             being refreshed"
    );
}

/// The state is terminal, so the cadence has stopped entirely.
#[test]
fn a_permission_changed_while_the_app_was_away_is_noticed_on_resume() {
    let mut f = Fixture::new(GateDouble::desktop().with_permission(LocationPermission::Denied));
    f.step();
    let permission = f.bridge.permission_cell();

    // Granted in system settings while rustdar was in the background.
    permission.set(LocationPermission::Granted);
    f.step_after_the_cadence();
    assert_eq!(
        f.gate.permission(),
        LocationPermission::Denied,
        "the fixture is not actually terminal, so this proves nothing"
    );

    f.gate.resumed();
    f.step();

    assert_eq!(f.gate.permission(), LocationPermission::Granted);
    assert!(f.bridge.location_active(), "the stream was never restarted");
}

/// A derived `Default` would silently change how a first run behaves.
#[test]
fn a_default_memo_leaves_location_enabled() {
    assert!(LocationMemo::default().enabled);
    assert_eq!(LocationMemo::default().attempts, 0);
}

/// Written at the moment of the decision, not on the autosave timer — the
/// dismiss-and-close case lands inside that window.
#[test]
fn an_attempt_is_persisted_before_the_ask_rather_than_at_shutdown() {
    let store = Rc::new(MemoryKvStore::default());
    let mut f = Fixture::new(
        desktop_with_store(Rc::clone(&store)).with_permission(LocationPermission::Prompt),
    );

    f.step();

    let raw = store
        .load(LOCATION_MEMO_KEY)
        .expect("the attempt was never written down");
    let memo: LocationMemo = serde_json::from_str(&raw).expect("unreadable memo");
    assert_eq!(memo.attempts, 1);
    assert!(memo.enabled);
}

/// A definite answer from the OS makes the counter meaningless, so it is
/// cleared: a user who re-allows location gets their prompts back.
#[test]
fn a_definite_answer_clears_the_attempt_counter() {
    let store = Rc::new(MemoryKvStore::default());
    let mut f = Fixture::new(
        desktop_with_store(Rc::clone(&store)).with_permission(LocationPermission::Prompt),
    );
    f.step();

    f.bridge.permission_cell().set(LocationPermission::Denied);
    f.step_after_the_cadence();

    let raw = store.load(LOCATION_MEMO_KEY).expect("no memo");
    let memo: LocationMemo = serde_json::from_str(&raw).expect("unreadable memo");
    assert_eq!(memo.attempts, 0);
}

/// The half of the memo that has to leave this crate. Android cannot tell "never
/// asked" from "permanently denied" on its own, and
/// [`LocationGate::set_attempts`] pushes only a value that *moved*, so a run
/// that loads a spent counter and is never asked again would leave the bridge
/// reporting a permanent denial as a fresh `Prompt`.
#[test]
fn a_bridge_is_told_what_this_install_has_already_asked_before_it_is_queried() {
    let store = Rc::new(MemoryKvStore::default());
    store
        .store(LOCATION_MEMO_KEY, r#"{"attempts":1,"enabled":true}"#)
        .unwrap();
    let mut f = Fixture::new(desktop_with_store(store).with_permission(LocationPermission::Prompt));
    let record = f.bridge.location_record();
    assert_eq!(record.attempts.get(), None, "the fixture starts wired up");

    f.step();

    assert_eq!(
        record.attempts.get(),
        Some(1),
        "the bridge was queried without being told this install has \
             already asked, so Android reports a permanent denial as a prompt"
    );
}

#[test]
fn an_unreadable_memo_falls_back_to_asking() {
    let store = Rc::new(MemoryKvStore::default());
    store.store(LOCATION_MEMO_KEY, "not json").unwrap();
    let mut f = Fixture::new(desktop_with_store(store).with_permission(LocationPermission::Prompt));

    f.step();

    assert_eq!(f.bridge.location_requests(), 1);
}
