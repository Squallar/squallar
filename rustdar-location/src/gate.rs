//! The one thing that decides whether rustdar asks the OS where the user is.
//! Nothing else anywhere calls `LocationBridge::request_location`.
//!
//! Three inputs, none sufficient alone: the OS's answer, authoritative about
//! *now*; a small persisted memo, the only thing that remembers across restarts;
//! and whether the bridge is delivering — a granted permission with no stream is
//! the state a fresh process starts in on every desktop.
//!
//! Four traps, each marked in the code below:
//!
//! 1. A `wants_poll` flag has no correct setting: stop polling after the ask and
//!    Android never notices the user tapping Allow; poll every frame and
//!    `checkSelfPermission` is a JNI call at the refresh rate. Resolved with a
//!    bounded cadence and genuinely terminal states.
//! 2. A `previously_asked` bool loses Android's middle state.
//! 3. No `KvStore` must not mean "never ask" — degrade to "ask, don't remember".
//! 4. A revoked permission must stop the stream.
use crate::bridge::LocationBridge;
use crate::permission::LocationPermission;
use rustdar_kv::KvStore;
use std::time::Duration;
use web_time::Instant;

/// Key the memo is persisted under. Its own `KvStore` entry rather than a field
/// on `UiConfig`, whose autosave writes on a 3 s timer: turning location off and
/// then closing the app lands inside that window.
pub const LOCATION_MEMO_KEY: &str = "location";

/// How often the platform is asked what it thinks. Not per frame: on Android the
/// query is a JNI call. Not "once, after the ask" either — see trap (1).
const POLL_INTERVAL_MS: u64 = 750;
const POLL_INTERVAL: Duration = Duration::from_millis(POLL_INTERVAL_MS);

/// The cadence while the settings window is open, derived from the base.
const SETTINGS_POLL_INTERVAL: Duration = Duration::from_millis(POLL_INTERVAL_MS / 3);

/// How many times this install may prompt, ever. Two, preserving Android's own
/// `MAX_PERMISSION_REQUESTS: u32 = 2`; the second is spent only on an ask that
/// demonstrably never reached the OS. A user gesture resets the counter.
const MAX_ATTEMPTS: u8 = 2;

/// How long to wait before re-asking within one run. Only reachable when the
/// previous ask did not reach the OS at all.
const RETRY_INTERVAL: Duration = Duration::from_secs(5);

/// What rustdar remembers about location across restarts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct LocationMemo {
    /// Unprompted asks spent, bounded by [`MAX_ATTEMPTS`]. Reset when the OS
    /// gives a definite answer, and by the settings button.
    attempts: u8,
    /// rustdar's own switch, distinct from the permission.
    enabled: bool,
}

impl Default for LocationMemo {
    /// `#[derive(Default)]` would make a first run behave as though the user had
    /// already turned location off.
    fn default() -> Self {
        Self {
            attempts: 0,
            enabled: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LocationStep {
    pub changed: bool,
    /// Delivery was just stopped because consent went away. The position on
    /// screen came from a source the user has withdrawn consent for, so the
    /// caller clears the dot — but only if nothing else is feeding it.
    pub revoked: bool,
}

/// The gate. See the module note.
pub struct LocationGate {
    memo: LocationMemo,
    /// Where the memo is persisted, resolved once and never retried. Cached
    /// because a bridge's `kv` allocates a fresh boxed store per call and this
    /// sits on the frame path.
    store: Option<Box<dyn KvStore>>,
    /// Not `store.is_some()`: `None` is a legitimate resolved answer.
    store_resolved: bool,
    permission: LocationPermission,
    active: bool,
    /// `None` until the first step, which makes the first step always do work.
    last_query: Option<Instant>,
    /// Stamped **before** the call and unconditionally.
    last_attempt: Option<Instant>,
    /// Per run, never persisted — see [`LocationBridge::request_location`]'s
    /// honesty note.
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

    pub fn permission(&self) -> LocationPermission {
        self.permission
    }

    pub fn active(&self) -> bool {
        self.active
    }

    /// Look at the platform, and act if there is anything to do. Called once per
    /// frame; most calls return immediately on the cadence check. `settings_open`
    /// tightens the cadence rather than gating it — the blue dot is drawn on the
    /// map, not in the settings window.
    pub(crate) fn step(
        &mut self,
        platform: &mut dyn LocationBridge,
        settings_open: bool,
    ) -> LocationStep {
        self.step_at(Instant::now(), platform, settings_open)
    }

    /// [`step`](Self::step) against a caller-supplied clock, so the cadence and
    /// the retry bound can be tested without sleeping.
    pub(crate) fn step_at(
        &mut self,
        now: Instant,
        platform: &mut dyn LocationBridge,
        settings_open: bool,
    ) -> LocationStep {
        if !self.due(now, settings_open) {
            return LocationStep::default();
        }
        self.resolve_store(platform);
        self.last_query = Some(now);

        let permission = platform.location_permission();
        let mut outcome = LocationStep::default();

        // The user's own switch outranks the OS's, checked before the match so a
        // `Granted` arm cannot undo it.
        if !self.memo.enabled {
            if platform.location_active() {
                platform.stop_location();
                outcome.revoked = true;
            }
            outcome.changed |= self.record(permission, platform.location_active());
            return outcome;
        }

        match permission {
            // Nothing to conclude. Asking here is the audited web defect — a
            // prompt on first paint with no gesture — and giving up parks the UI
            // at "checking…" forever.
            LocationPermission::Unknown => {}

            // Terminal: no sequence of events turns this into a grant.
            LocationPermission::Unavailable => {}

            LocationPermission::Denied => {
                // Trap (4). A revocation in system settings restarts no process.
                if platform.location_active() {
                    platform.stop_location();
                    outcome.revoked = true;
                }
                // Clearing the attempt record means a user who later re-allows
                // location gets their prompts back rather than a spent counter.
                self.set_attempts(0, platform);
            }

            LocationPermission::Granted => {
                self.set_attempts(0, platform);
                // A grant is not a stream: every desktop process starts granted
                // and silent, and this is what turns it on (trap 1).
                if !platform.location_active() {
                    platform.request_location();
                }
            }

            LocationPermission::Prompt => {
                if self.may_ask() && self.retry_due(now) {
                    // Stamped first, and outside any conditional: assigning it
                    // after a successful call re-fired `requestPermissions` on
                    // every frame whenever the call reported a miss.
                    self.last_attempt = Some(now);
                    self.set_attempts(self.memo.attempts.saturating_add(1), platform);
                    self.ask_reached_os = platform.request_location();
                }
            }
        }

        outcome.changed |= self.record(permission, platform.location_active());
        outcome
    }

    /// Turn location on, or back on, at the user's request. Resets the attempt
    /// counter; does **not** itself prompt, since every `request_location` goes
    /// through [`step`](Self::step).
    pub(crate) fn enable(&mut self, platform: &mut dyn LocationBridge) {
        self.memo.enabled = true;
        self.memo.attempts = 0;
        self.persist();
        platform.set_location_attempts(self.memo.attempts);
        // Act at once rather than waiting out the cadence: a click that appears
        // to do nothing reads as a broken button.
        self.last_query = None;
        self.last_attempt = None;
        self.ask_reached_os = false;
    }

    /// Turn location off, and stop the stream now rather than at the next step —
    /// the dot continuing to move is the app disagreeing with its own control.
    pub(crate) fn disable(&mut self, platform: &mut dyn LocationBridge) {
        self.memo.enabled = false;
        self.persist();
        platform.stop_location();
        self.active = platform.location_active();
        self.last_query = None;
    }

    /// Re-read the platform on the next step, whatever the cadence says. On a
    /// terminal state polling has stopped entirely, so returning to the
    /// foreground is the one moment a revocation elsewhere would be noticed.
    pub fn resumed(&mut self) {
        self.last_query = None;
    }

    /// Whether this step should do any work at all.
    ///
    /// Trap (1). Terminal means "no reachable sequence of events changes the
    /// answer": no service, the user has refused, or the grant is in hand *and*
    /// the stream is running. Everything else keeps polling.
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
            // Someone is looking at the control, so poll even in a terminal state.
            (_, true) => SETTINGS_POLL_INTERVAL,
            (false, false) => POLL_INTERVAL,
            (true, false) => return false,
        };
        now.duration_since(last) >= interval
    }

    /// Whether an unprompted ask is still allowed.
    ///
    /// Trap (2), narrower than "two attempts" sounds. A run that has not asked
    /// may ask only if no earlier run has — a dismissed dialog leaves the
    /// permission on `Prompt`, indistinguishable from never having asked. A run
    /// that has asked may ask again only if the ask never reached the OS.
    fn may_ask(&self) -> bool {
        if self.memo.attempts >= MAX_ATTEMPTS {
            return false;
        }
        match self.last_attempt {
            None => self.memo.attempts == 0,
            Some(_) => !self.ask_reached_os,
        }
    }

    fn retry_due(&self, now: Instant) -> bool {
        self.last_attempt
            .is_none_or(|last| now.duration_since(last) >= RETRY_INTERVAL)
    }

    /// Write `attempts` through to the memo, the store and the bridge, if it
    /// moved. The bridge is told because Android cannot otherwise tell "never
    /// asked" from "permanently denied".
    fn set_attempts(&mut self, attempts: u8, platform: &mut dyn LocationBridge) {
        if self.memo.attempts == attempts {
            return;
        }
        self.memo.attempts = attempts;
        // Before the ask this number is counting, not after: a process killed
        // between the two would come back believing it had never asked.
        self.persist();
        platform.set_location_attempts(attempts);
    }

    fn record(&mut self, permission: LocationPermission, active: bool) -> bool {
        let changed = self.permission != permission || self.active != active;
        self.permission = permission;
        self.active = active;
        changed
    }

    /// Find the store, once, and load whatever is in it.
    ///
    /// Trap (3). A missing store degrades to "ask, but do not remember" — never
    /// to "never ask"; the live cases (site data blocked, no `XDG_CONFIG_HOME`)
    /// have a working permission and lose only the memory.
    ///
    /// The bridge is told here because [`set_attempts`](Self::set_attempts) only
    /// pushes a value that *moved*, so a run that loads `attempts: 1` and is
    /// never asked again would leave Android reporting a permanent denial as a
    /// `Prompt`.
    fn resolve_store(&mut self, platform: &mut dyn LocationBridge) {
        if self.store_resolved {
            return;
        }
        self.store_resolved = true;
        self.store = platform.kv();
        match self.store.as_ref().and_then(|s| s.load(LOCATION_MEMO_KEY)) {
            Some(raw) => match serde_json::from_str::<LocationMemo>(&raw) {
                Ok(memo) => self.memo = memo,
                // Silently defaulting means silently re-prompting.
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

    /// Write the memo now, not on the autosave timer: the ordinary
    /// [`KvStore::store`] hands the bytes to a writer thread, and a session that
    /// ends first loses them. Failure costs one extra prompt on the next run.
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
mod double;
#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use double::GateDouble;
