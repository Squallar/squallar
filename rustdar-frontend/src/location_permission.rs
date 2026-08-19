//! The one thing that decides whether rustdar asks the OS where the user is.
//!
//! Shaped like [`location_hint`](crate::location_hint): self-contained, one
//! call site. `App` owns a [`LocationGate`] and steps it from
//! `poll_platform_state`; nothing else in the crate calls
//! [`PlatformBridge::request_location`], and that is the property worth
//! keeping. A permission prompt is the most intrusive thing this application
//! can do to a person, so there is exactly one line of code that can do it and
//! every guard sits in front of that line.
//!
//! # What "decide" means here
//!
//! Three inputs, none of which is sufficient alone:
//!
//! * the OS's own answer ([`LocationPermission`]), which is authoritative about
//!   *now* and says nothing about history;
//! * a small persisted memo, which is the only thing that remembers across
//!   restarts and is deliberately tiny;
//! * whether the bridge is currently delivering, which is neither of the above
//!   — a granted permission with no stream is the state a fresh process starts
//!   in on every desktop.
//!
//! # The four things this got wrong first
//!
//! Each of the four is a comment in the code below, because each looks like an
//! obvious simplification until you ask what happens on a real device.
//!
//! 1. **A `wants_poll` flag has no correct setting.** Stop polling after the
//!    ask and Android never notices the user tapping Allow, so location never
//!    starts that session. Keep polling every frame and `checkSelfPermission`
//!    is a JNI call at the refresh rate. "Dialog on screen" versus "dialog
//!    dismissed" is a fourth state no platform exposes, so no flag can separate
//!    them. Resolved with a bounded *cadence* and genuinely terminal states.
//! 2. **A `previously_asked` bool loses Android's middle state** and hangs a
//!    permanent decision off a return value three bridges have to invent.
//!    Resolved with an attempt counter written before the call.
//! 3. **No `KvStore` must not mean "never ask".** Resolved by degrading to
//!    "ask, don't remember".
//! 4. **A revoked permission must stop the stream**, or a blue dot sits on the
//!    map at a position the user has withdrawn consent for.
//!
//! [`PlatformBridge::request_location`]: crate::platform::PlatformBridge::request_location

use crate::platform::PlatformBridge;
use rustdar_gps::LocationPermission;
use rustdar_kv::KvStore;
use std::time::Duration;
use web_time::Instant;

/// Key the memo is persisted under.
///
/// Its own `KvStore` entry rather than a field on `UiConfig`, and that is
/// not tidiness. `autosave_config` writes on a 3 s timer behind a JSON compare,
/// and the case that matters most — the user opens settings, turns location
/// off, and closes the app — lands inside that window and would be lost. This
/// is written synchronously at the moment of the decision instead.
pub const LOCATION_MEMO_KEY: &str = "location";

/// How often the platform is asked what it thinks.
///
/// Not per frame: on Android the query is a JNI call, and at 120 Hz that is 120
/// attachments a second for a value that changes at most a handful of times in
/// a session. Not "once, after the ask" either — see fix (1) in the module
/// note. ~1.3 Hz is fast enough that a user who taps Allow sees the dot appear
/// without noticing a delay, and slow enough to be free.
const POLL_INTERVAL_MS: u64 = 750;
const POLL_INTERVAL: Duration = Duration::from_millis(POLL_INTERVAL_MS);

/// The cadence while the settings window is open.
///
/// The Location control is on screen and is a claim about the *current* state,
/// so a revocation made in system settings has to show up on it promptly. A
/// third of the base interval for as long as somebody is looking at it, derived
/// rather than written out so the two cannot drift apart.
const SETTINGS_POLL_INTERVAL: Duration = Duration::from_millis(POLL_INTERVAL_MS / 3);

/// How many times this install may prompt, ever.
///
/// Two, preserving the bound Android's own `MAX_PERMISSION_REQUESTS: u32 = 2`
/// already provides. The second is not a second *opportunity to nag*: see
/// [`LocationGate::may_ask`], which spends it only on an ask that demonstrably
/// never reached the OS.
///
/// A user gesture resets the counter — that is what the settings button is
/// for — so this bounds unprompted asks, not asks.
const MAX_ATTEMPTS: u8 = 2;

/// How long to wait before re-asking within one run.
///
/// Only reachable when the previous ask did not reach the OS at all, which on
/// Android means the `Activity` was not stashed yet or the JNI attach failed —
/// both of which resolve in a second or two. Long enough that a bridge which
/// wrongly reports a miss cannot stack two dialogs on top of each other,
/// short enough that the retry still happens while the app is being opened.
const RETRY_INTERVAL: Duration = Duration::from_secs(5);

/// What rustdar remembers about location across restarts.
///
/// Deliberately two fields. Everything else the gate needs — when it last
/// asked, whether the last ask reached the OS, what the OS said — is either
/// re-derivable at startup or actively wrong to carry forward.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct LocationMemo {
    /// Unprompted asks spent, bounded by [`MAX_ATTEMPTS`]. Reset the moment the
    /// OS gives a definite answer, and by the settings button.
    attempts: u8,
    /// Whether the user wants location at all. Distinct from the permission:
    /// this is rustdar's own switch, and it is the only one the app owns.
    enabled: bool,
}

impl Default for LocationMemo {
    /// `enabled` is `true`, hand-written because `#[derive(Default)]` would
    /// make it `false` and a first run would then behave as though the user had
    /// already turned location off — silently changing the shipped behaviour of
    /// every platform that has location working today.
    fn default() -> Self {
        Self {
            attempts: 0,
            enabled: true,
        }
    }
}

/// What one [`LocationGate::step`] changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LocationStep {
    /// The reported state moved, so the UI's cached copy is stale and a frame
    /// is owed.
    pub changed: bool,
    /// Delivery was just stopped because consent went away.
    ///
    /// Distinct from `changed`: whatever position is on screen came from a
    /// source the user has withdrawn consent for, and leaving it there is worse
    /// than a stale label. The caller clears the dot — but only if nothing
    /// *else* is feeding it, since a serial dongle is not covered by this
    /// permission at all.
    pub revoked: bool,
}

/// The gate. See the module note.
pub struct LocationGate {
    memo: LocationMemo,
    /// Where the memo is persisted, resolved once.
    ///
    /// Cached because `PlatformBridge::kv` allocates a fresh
    /// `Box<FileKvStore>` on every call and this sits on the frame path.
    ///
    /// Resolved once and never retried, which is safe because every bridge that
    /// has a store has it before the first frame: Android's `set_config_dir`
    /// runs during `android_main`, ahead of `run_app`; desktop and iOS derive
    /// one in their constructors; `localStorage` is available to the browser
    /// from the start. The cases that answer `None` answer it permanently.
    store: Option<Box<dyn KvStore>>,
    /// Whether [`store`](Self::store) has been resolved yet. Not `store
    /// .is_some()`: `None` is a legitimate resolved answer.
    store_resolved: bool,
    /// The last state reported to the caller.
    permission: LocationPermission,
    /// Whether the bridge was delivering as of the last poll.
    active: bool,
    /// When the platform was last asked. `None` until the first step, which is
    /// what makes the first step always do the work.
    last_query: Option<Instant>,
    /// When this run last called `request_location`, stamped **before** the
    /// call and unconditionally.
    last_attempt: Option<Instant>,
    /// Whether the last ask is believed to have reached the OS.
    ///
    /// In memory, per run, never persisted — that restriction is the whole
    /// point. See [`PlatformBridge::request_location`]'s honesty note: this is
    /// the only thing allowed to hang off that return value.
    ///
    /// [`PlatformBridge::request_location`]: crate::platform::PlatformBridge::request_location
    ask_reached_os: bool,
}

impl Default for LocationGate {
    fn default() -> Self {
        Self::new()
    }
}

impl LocationGate {
    pub fn new() -> Self {
        Self {
            memo: LocationMemo::default(),
            store: None,
            store_resolved: false,
            permission: LocationPermission::default(),
            active: false,
            last_query: None,
            last_attempt: None,
            ask_reached_os: false,
        }
    }

    /// The state the settings pane renders, as of the last poll.
    pub fn permission(&self) -> LocationPermission {
        self.permission
    }

    /// Whether the bridge was delivering as of the last poll.
    pub fn active(&self) -> bool {
        self.active
    }

    /// Look at the platform, and act if there is anything to do.
    ///
    /// Called once per frame from `App::poll_platform_state`; most of those
    /// calls return immediately on the cadence check below.
    ///
    /// `settings_open` tightens the cadence rather than gating it, because the
    /// state has to be current whether or not anyone is looking — the blue dot
    /// is drawn on the map, not in the settings window.
    pub fn step(&mut self, platform: &mut dyn PlatformBridge, settings_open: bool) -> LocationStep {
        self.step_at(Instant::now(), platform, settings_open)
    }

    /// [`step`](Self::step) against a caller-supplied clock.
    ///
    /// The clock is a parameter so the cadence and the retry bound can be
    /// tested without sleeping. Nothing in production passes anything but
    /// `Instant::now()`.
    pub(crate) fn step_at(
        &mut self,
        now: Instant,
        platform: &mut dyn PlatformBridge,
        settings_open: bool,
    ) -> LocationStep {
        if !self.due(now, settings_open) {
            return LocationStep::default();
        }
        self.resolve_store(platform);
        self.last_query = Some(now);

        let permission = platform.location_permission();
        let mut outcome = LocationStep::default();

        // The user's own switch outranks the OS's. Checked before the match so
        // that turning location off cannot be undone by a `Granted` arm that
        // helpfully restarts delivery.
        if !self.memo.enabled {
            if platform.location_active() {
                platform.stop_location();
                outcome.revoked = true;
            }
            outcome.changed |= self.record(permission, platform.location_active());
            return outcome;
        }

        match permission {
            // Nobody has answered yet, so there is nothing to conclude. Asking
            // here is the audited web defect — a permission prompt on first
            // paint with no user gesture — and giving up here parks the UI at
            // "checking…" forever. Do neither; look again next tick.
            LocationPermission::Unknown => {}

            // No location service on this platform at all. Terminal: there is
            // no sequence of events that turns this into a grant, and the
            // cadence check above stops polling for it.
            LocationPermission::Unavailable => {}

            LocationPermission::Denied => {
                // Fix (4). A grant revoked in system settings does not restart
                // any process on any desktop OS, so without this the reader
                // keeps running and the dot keeps updating from a position the
                // user has just withdrawn consent for.
                if platform.location_active() {
                    platform.stop_location();
                    outcome.revoked = true;
                }
                // The OS has given a definite answer, so the record of how many
                // times we asked has done its job. Clearing it means that if
                // the user later re-allows location in system settings, this
                // install gets its prompts back rather than being stuck on a
                // counter spent against a decision they have since reversed.
                self.set_attempts(0, platform);
            }

            LocationPermission::Granted => {
                self.set_attempts(0, platform);
                // A grant is not a stream. Every desktop process starts granted
                // and silent, and this is what turns it on — including on the
                // frame after the user taps Allow, which is the case fix (1)
                // exists for.
                if !platform.location_active() {
                    platform.request_location();
                }
            }

            LocationPermission::Prompt => {
                if self.may_ask() && self.retry_due(now) {
                    // Stamped first, and outside any conditional. The version
                    // that assigned it after a successful call re-fired
                    // `requestPermissions` on every frame whenever the call
                    // reported a miss, which on Android is a dialog storm.
                    self.last_attempt = Some(now);
                    self.set_attempts(self.memo.attempts.saturating_add(1), platform);
                    self.ask_reached_os = platform.request_location();
                }
            }
        }

        outcome.changed |= self.record(permission, platform.location_active());
        outcome
    }

    /// Turn location on, or back on, at the user's request.
    ///
    /// The gesture resets the attempt counter — that is its job, and it is why
    /// the counter can be as small as it is. It does **not** itself prompt:
    /// every `request_location` call in this crate goes through
    /// [`step`](Self::step), so the same guards apply. In particular a user who
    /// has already been refused by the OS gets no new dialog out of this, and
    /// the settings pane does not offer them the button in the first place.
    pub fn enable(&mut self, platform: &mut dyn PlatformBridge) {
        self.memo.enabled = true;
        self.memo.attempts = 0;
        self.persist();
        platform.set_location_attempts(self.memo.attempts);
        // The next step acts at once rather than waiting out the cadence: this
        // came from a click, and a click that appears to do nothing for
        // three-quarters of a second reads as a broken button.
        self.last_query = None;
        self.last_attempt = None;
        self.ask_reached_os = false;
    }

    /// Turn location off at the user's request, and stop the stream now.
    ///
    /// Stopping here rather than leaving it to the next step is deliberate:
    /// this is the button that says "off", and up to [`POLL_INTERVAL`] of the
    /// dot continuing to move afterwards is the app disagreeing with its own
    /// control.
    pub fn disable(&mut self, platform: &mut dyn PlatformBridge) {
        self.memo.enabled = false;
        self.persist();
        platform.stop_location();
        self.active = platform.location_active();
        self.last_query = None;
    }

    /// Re-read the platform on the next step, whatever the cadence says.
    ///
    /// For `resumed`: a permission can be changed in system settings while the
    /// app is in the background, and on a terminal state the cadence has
    /// stopped polling entirely — so coming back to the foreground is the one
    /// moment a revocation made elsewhere would otherwise never be noticed.
    pub fn resumed(&mut self) {
        self.last_query = None;
    }

    /// Whether this step should do any work at all.
    ///
    /// Fix (1). Terminal means "no sequence of events reachable from here
    /// changes the answer": the platform has no service, the user has refused,
    /// or the grant is in hand *and* the stream is running. Everything else —
    /// including a `Prompt` whose bound is spent — keeps polling, because a
    /// grant arriving after the ask is exactly what the naive version could
    /// never see.
    fn due(&self, now: Instant, settings_open: bool) -> bool {
        let Some(last) = self.last_query else {
            return true;
        };
        let terminal = match self.permission {
            LocationPermission::Unavailable | LocationPermission::Denied => true,
            LocationPermission::Granted => self.active,
            LocationPermission::Unknown | LocationPermission::Prompt => false,
        };
        let interval = match (terminal, settings_open) {
            // Someone is looking at the control that reports this. Poll even in
            // a terminal state, so a permission changed in system settings and
            // then returned to shows up rather than sitting stale.
            (_, true) => SETTINGS_POLL_INTERVAL,
            (false, false) => POLL_INTERVAL,
            (true, false) => return false,
        };
        now.duration_since(last) >= interval
    }

    /// Whether an unprompted ask is still allowed.
    ///
    /// Fix (2), and the rule is narrower than "two attempts" sounds:
    ///
    /// * **A run that has not asked yet may ask, but only if no earlier run
    ///   has.** This is what makes a prompt the user walked away from stay
    ///   walked away from. A dismissed dialog leaves the permission on
    ///   `Prompt`, indistinguishable from never having asked, so the memo is
    ///   the only thing that knows — and re-prompting every launch is the
    ///   behaviour that trains people to hit Deny.
    /// * **A run that has asked may ask again only if the ask never reached the
    ///   OS.** That is the whole of what the `bool` from `request_location` is
    ///   worth, and it is why the second attempt exists at all: on Android the
    ///   ask genuinely can fail to land, and a user who was never actually
    ///   asked should not be recorded as having declined.
    ///
    /// The user can always re-arm this from the settings pane; see
    /// [`enable`](Self::enable).
    fn may_ask(&self) -> bool {
        if self.memo.attempts >= MAX_ATTEMPTS {
            return false;
        }
        match self.last_attempt {
            None => self.memo.attempts == 0,
            Some(_) => !self.ask_reached_os,
        }
    }

    /// Whether enough time has passed since this run's last ask.
    fn retry_due(&self, now: Instant) -> bool {
        self.last_attempt
            .is_none_or(|last| now.duration_since(last) >= RETRY_INTERVAL)
    }

    /// Write `attempts` through to the memo, the store and the bridge, if it
    /// moved.
    ///
    /// The bridge is told because Android cannot otherwise tell "never asked"
    /// from "permanently denied" — see
    /// [`PlatformBridge::set_location_attempts`].
    ///
    /// [`PlatformBridge::set_location_attempts`]: crate::platform::PlatformBridge::set_location_attempts
    fn set_attempts(&mut self, attempts: u8, platform: &mut dyn PlatformBridge) {
        if self.memo.attempts == attempts {
            return;
        }
        self.memo.attempts = attempts;
        // Before the ask that this number is counting, not after. A process
        // killed between the two would otherwise come back believing it had
        // never asked, and ask again.
        self.persist();
        platform.set_location_attempts(attempts);
    }

    /// Adopt a reading, and say whether it moved.
    fn record(&mut self, permission: LocationPermission, active: bool) -> bool {
        let changed = self.permission != permission || self.active != active;
        self.permission = permission;
        self.active = active;
        changed
    }

    /// Find the store, once, and load whatever is in it.
    ///
    /// Fix (3). A missing store degrades to "ask, but do not remember" — never
    /// to "never ask". The live cases are a browser with site data blocked and
    /// a desktop process with no `XDG_CONFIG_HOME`, `HOME` or `LOCALAPPDATA`
    /// (containers, systemd units), and on both of those the permission itself
    /// works perfectly well. Returning early there, as the first version did,
    /// left those users with no location, no prompt, no button and no
    /// explanation.
    ///
    /// What they lose is the memory: every run asks once more. That is the
    /// correct trade — the alternative is silently disabling a working feature
    /// — and it is bounded by [`MAX_ATTEMPTS`] within each run.
    ///
    /// # Why the bridge is told at the end of this
    ///
    /// [`set_attempts`](Self::set_attempts) only pushes a value that *moved*,
    /// so a run that loads `attempts: 1` out of the memo and is never asked
    /// again would never tell the bridge anything, and Android would spend the
    /// whole session believing this install had never asked. That is the
    /// difference between reporting a permanent denial as `Denied` — no button,
    /// honest advice — and reporting it as `Prompt`, which is a button that
    /// raises a dialog the framework silently refuses to show. Pushed here, on
    /// the one pass that reads the memo, so the bridge has it before the first
    /// query.
    fn resolve_store(&mut self, platform: &mut dyn PlatformBridge) {
        if self.store_resolved {
            return;
        }
        self.store_resolved = true;
        self.store = platform.kv();
        match self.store.as_ref().and_then(|s| s.load(LOCATION_MEMO_KEY)) {
            Some(raw) => match serde_json::from_str::<LocationMemo>(&raw) {
                Ok(memo) => self.memo = memo,
                // A corrupt memo is not worth failing over, but it is worth
                // saying: silently defaulting means silently re-prompting.
                Err(e) => log::warn!("ignoring unreadable location memo: {e}"),
            },
            None if self.store.is_none() => log::debug!(
                "no config store; location choices will be asked once per run \
                 and not remembered"
            ),
            None => {}
        }
        platform.set_location_attempts(self.memo.attempts);
    }

    /// Write the memo now, not on the autosave timer.
    ///
    /// [`KvStore::store_now`], because "now" is the claim: the ordinary
    /// `store` hands the bytes to a writer thread, and a session that ends
    /// before that thread runs loses the memo exactly as if it had waited for
    /// the timer this deliberately does not use.
    ///
    /// Failure is logged and dropped, like every other config write in this
    /// app: a full `localStorage` must not stop the map from working. The cost
    /// of losing it is one extra prompt on the next run.
    fn persist(&self) {
        let Some(store) = self.store.as_ref() else {
            return;
        };
        let json = match serde_json::to_string(&self.memo) {
            Ok(json) => json,
            Err(e) => {
                log::warn!("could not serialize the location memo: {e}");
                return;
            }
        };
        if let Err(e) = store.store_now(LOCATION_MEMO_KEY, &json) {
            log::warn!("could not persist the location memo: {e}");
        }
    }
}

#[cfg(test)]
mod tests;
