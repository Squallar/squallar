use super::*;
use crate::platform_double::TestBridge;
use rustdar_kv::MemoryKvStore;
use std::rc::Rc;

/// A gate and the bridge it drives, with the clock in the test's hands.
///
/// `Instant` cannot be constructed from a number, so the clock is one
/// reading taken up front and offsets added to it. That is enough: every
/// decision the gate makes is on a *difference*.
struct Fixture {
    gate: LocationGate,
    bridge: TestBridge,
    t0: Instant,
    elapsed: Duration,
}

impl Fixture {
    fn new(bridge: TestBridge) -> Self {
        Self {
            gate: LocationGate::new(),
            bridge,
            t0: Instant::now(),
            elapsed: Duration::ZERO,
        }
    }

    /// Move the clock forward, as the app idling does.
    fn wait(&mut self, by: Duration) -> &mut Self {
        self.elapsed += by;
        self
    }

    /// One pass of the gate at the current clock.
    fn step(&mut self) -> LocationStep {
        let now = self.t0 + self.elapsed;
        self.gate.step_at(now, &mut self.bridge, false)
    }

    /// One pass with the settings window on screen.
    fn step_in_settings(&mut self) -> LocationStep {
        let now = self.t0 + self.elapsed;
        self.gate.step_at(now, &mut self.bridge, true)
    }

    /// Steps until the cadence has certainly let another query through.
    fn step_after_the_cadence(&mut self) -> LocationStep {
        self.wait(POLL_INTERVAL).step()
    }
}

/// A desktop bridge whose store the test keeps, so a "restart" can hand the
/// same blobs to a fresh gate.
fn desktop_with_store(store: Rc<MemoryKvStore>) -> TestBridge {
    TestBridge::desktop().with_store(store)
}

// ── Not asking ──────────────────────────────────────────────────────

/// The audited web defect, pinned. The browser prompted on first paint
/// because there was no way to say "I do not know yet" — and every platform
/// has that window at startup, so this is not a web-only guard.
#[test]
fn a_platform_that_has_not_answered_yet_is_never_prompted() {
    let mut f = Fixture::new(TestBridge::web().with_permission(LocationPermission::Unknown));

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

/// A platform with no location service must not be asked and must not be
/// polled forever: there is no sequence of events that changes the answer.
#[test]
fn a_platform_without_location_is_never_asked_and_never_polled() {
    let mut f =
        Fixture::new(TestBridge::desktop().with_permission(LocationPermission::Unavailable));

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

/// The user said no. Nothing this app does may produce another dialog.
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

    // A fresh run, same install.
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

// ── Asking ──────────────────────────────────────────────────────────

/// The startup ask, and the bound on it.
#[test]
fn a_first_run_asks_for_location_once_and_only_once() {
    let mut f = Fixture::new(TestBridge::desktop().with_permission(LocationPermission::Prompt));

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

/// The one thing the `bool` from `request_location` is worth. Android can
/// genuinely fail to reach `Activity.requestPermissions` — no Activity
/// stashed yet, JNI attach failed — and a user who was never asked must not
/// be recorded as having declined.
#[test]
fn an_ask_that_never_reached_the_os_is_tried_again_within_the_bound() {
    let mut f = Fixture::new(
        TestBridge::desktop()
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

/// **The regression the poll cadence exists for.**
///
/// The dialog is asynchronous everywhere. If polling stopped once the ask
/// was made, the user tapping Allow would be observed by nothing, no
/// stream would be started, and location would simply never work that
/// session — with `start_location_thread`'s 10 s self-healing loop being
/// removed, there is no second chance.
#[test]
fn a_grant_that_arrives_after_the_ask_still_starts_delivery() {
    let mut f = Fixture::new(TestBridge::desktop().with_permission(LocationPermission::Prompt));
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

/// A dialog dismissed without an answer leaves the permission on `Prompt`,
/// which is indistinguishable from never having asked. The memo is the only
/// thing that knows better, and re-prompting every launch is what trains
/// people to hit Deny.
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

/// Fix (3). Web with site data blocked, and desktop with no `HOME`, are
/// platforms where location itself works fine. Silently disabling it there
/// is worse than forgetting the answer.
#[test]
fn a_device_with_no_kv_is_still_asked() {
    let mut f = Fixture::new(
        TestBridge::web()
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

// ── Stopping ────────────────────────────────────────────────────────

/// Fix (4). Revoking in system settings restarts no process on any desktop
/// OS, so without this the reader keeps running and the dot keeps moving
/// from a position consent has been withdrawn for.
///
/// Observed through an open settings window, because a grant that is
/// actually delivering is a *terminal* state for the cadence — see
/// [`LocationGate::due`] — and the two things that re-open it are
/// [`resumed`](LocationGate::resumed) and somebody looking at the control
/// that reports it. Both are the routes a user who has just been in system
/// settings takes.
#[test]
fn a_revoked_permission_stops_delivery_and_clears_the_dot() {
    let mut f = Fixture::new(TestBridge::desktop().with_permission(LocationPermission::Granted));
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

/// The app's own switch, which is not the OS's. Turning it off must stop
/// the stream at the moment of the click, not at the next poll.
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

// ── The settings gesture ────────────────────────────────────────────

/// The button's job is to give a user who dismissed the dialog their
/// prompt back. Without this the bound is permanent and there is no way in.
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

/// The other half, and the one that matters more. A gesture resets the
/// *counter*; it cannot reset the OS's own refusal, and trying would be a
/// dialog the platform will not show anyway.
#[test]
fn enabling_location_in_settings_cannot_re_ask_after_a_remembered_denial() {
    let mut f = Fixture::new(TestBridge::desktop().with_permission(LocationPermission::Denied));
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

// ── Cadence ─────────────────────────────────────────────────────────

/// The other half of fix (1): keeping the poll alive must not mean polling
/// on every frame. On Android each of these is a JNI call.
#[test]
fn a_pending_permission_is_polled_on_a_bounded_cadence_not_every_frame() {
    let mut f = Fixture::new(TestBridge::desktop().with_permission(LocationPermission::Prompt));

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

/// A terminal state stops polling — except while the control that reports
/// it is on screen, where a revocation made in system settings has to show
/// up promptly.
#[test]
fn an_open_settings_window_keeps_watching_a_settled_permission() {
    let mut f = Fixture::new(TestBridge::desktop().with_permission(LocationPermission::Denied));
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

/// Coming back to the foreground is the one moment a change made outside
/// the app would otherwise never be noticed: the state is terminal, so the
/// cadence has stopped entirely.
#[test]
fn a_permission_changed_while_the_app_was_away_is_noticed_on_resume() {
    let mut f = Fixture::new(TestBridge::desktop().with_permission(LocationPermission::Denied));
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

// ── The memo ────────────────────────────────────────────────────────

/// A first run must behave exactly as the app behaved before this existed,
/// which a derived `Default` would silently change.
#[test]
fn a_default_memo_leaves_location_enabled() {
    assert!(LocationMemo::default().enabled);
    assert_eq!(LocationMemo::default().attempts, 0);
}

/// The memo is written the moment the decision is made, not on the autosave
/// timer — the dismiss-and-close case lands inside that window.
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
/// cleared: a user who re-allows location in system settings gets their
/// prompts back rather than inheriting a bound spent against a decision
/// they have since reversed.
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

/// The half of the memo that has to leave this crate.
///
/// Android cannot tell "never asked" from "permanently denied" on its own —
/// `shouldShowRequestPermissionRationale` is `false` at both ends — so the
/// persisted count is the only thing that separates them, and it is only
/// useful over there. [`LocationGate::set_attempts`] pushes it when it
/// *moves*, which covers every ask; a run that loads a spent counter and is
/// then correctly never asked again would push nothing at all, and that
/// run's bridge would spend the whole session reporting a permanent denial
/// as a fresh `Prompt`: a button that raises a dialog Android silently
/// refuses to show.
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

/// Nothing crashes, and nothing is silently treated as a decision, if the
/// blob is garbage — a half-written file or a `localStorage` key somebody
/// edited.
#[test]
fn an_unreadable_memo_falls_back_to_asking() {
    let store = Rc::new(MemoryKvStore::default());
    store.store(LOCATION_MEMO_KEY, "not json").unwrap();
    let mut f = Fixture::new(desktop_with_store(store).with_permission(LocationPermission::Prompt));

    f.step();

    assert_eq!(f.bridge.location_requests(), 1);
}
