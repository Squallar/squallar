//! When a failing overlay fetch is allowed to try again.
//!
//! # The storm this exists for
//!
//! Every auto-polling overlay decided "is a fetch due?" from
//! [`OverlayState::fetch_time`] alone, and that stamp is written **only on
//! success** ([`OverlayState::set_data`]). A failed fetch logged, cleared
//! `fetching`, and left the stamp untouched — so the next frame found the layer
//! due again and started another fetch, and the frame after that, forever.
//!
//! Measured in headless Chromium driving the real web build against a failing
//! SPC Mesoscale Discussion feed: **3089 `SPC MD fetch failed` lines in 105 s**
//! — 29.4 requests a second, one per animation frame, from every open tab, for
//! as long as the app stayed open. Native floors the same loop at 1 Hz through
//! `MIN_WAKE` in `App::auto_poll_delay`, which is why it read as a slow leak
//! there and only became a storm on web.
//!
//! Under the policy below the same 105 s window costs **6 attempts**.
//!
//! # The ceiling is the layer's own poll interval, not a constant
//!
//! A layer's [`auto_poll_interval`] is already the cadence at which fresh data
//! is worth having: 120 s for SPC discussions, a product SPC issues tens of
//! minutes apart. A retry *faster* than that interval cannot deliver anything
//! the next ordinary poll would not — succeeding at t+4 s instead of t+120 s
//! buys data published at most four seconds earlier, which against a ~30-minute
//! issue cadence is nothing at all. Retrying a failure faster than we poll a
//! success is the failure path being more aggressive than the healthy path,
//! which is backwards.
//!
//! So the ladder climbs to the handler's own interval and stops. At the ceiling
//! a failing layer costs exactly what a healthy one costs, which is the right
//! steady state and needs no per-handler tuning — a new overlay inherits a
//! correct ceiling from the interval it already had to declare.
//!
//! The floor is [`FIRST_RETRY_SECS`]: one dropped packet or a wifi handover
//! should recover before the user notices, and two seconds is under the time it
//! takes to look at the layer stack and wonder.
//!
//! # Failure kinds are not all worth retrying
//!
//! See [`FetchFailure`]. The short version: a 404 for a product that is simply
//! not published right now is a *normal answer* and is not a failure at all; a
//! network error is worth waiting out; and a request the origin refused outright
//! will not start working because we asked again, so it is recorded as broken
//! rather than retried forever at a slow cadence.
//!
//! # A user action never waits out a backoff
//!
//! The ladder governs the **automatic** poll and nothing else. Every
//! user-driven fetch — the Refresh button, switching a layer on, changing an
//! outlook day or a model parameter — goes through `push_user_overlay_fetch`,
//! which calls [`FetchRetry::clear`] before queueing. That makes "a user action
//! is never made to wait" true by construction rather than by remembering to
//! clear the ledger at four call sites; the auto-poll gate is the only caller
//! that consults it.
//!
//! [`OverlayState::fetch_time`]: crate::render::overlay_state::OverlayState::fetch_time
//! [`OverlayState::set_data`]: crate::render::overlay_state::OverlayState::set_data
//! [`auto_poll_interval`]: crate::render::overlay_state::OverlayHandler::auto_poll_interval

use std::time::Duration;

/// The first retry after a transient failure. Doubles from here, clamped to the
/// layer's own poll interval — see the module docs for why that is the ceiling.
pub const FIRST_RETRY_SECS: u64 = 2;

/// Caps the doubling before it can overflow the shift. Any real interval
/// clamps the result long before this bites: at `FIRST_RETRY_SECS` = 2 the
/// 32nd step is already ~272 years.
const MAX_LADDER_STEPS: u32 = 32;

/// What a failed fetch tells us about whether trying again could work.
///
/// Never merged into one "it failed": the three call for opposite schedules,
/// and collapsing them is how a product that is merely out of season ends up
/// hammered at the same rate as one that is down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchFailure {
    /// The origin answered, and the answer was that the product is not there
    /// right now (404/410 from an endpoint that is published on a schedule).
    ///
    /// **Not a failure.** SPC does not keep a Day 4-8 probabilistic outlook up
    /// at every hour of the day, and asking again in four seconds will get the
    /// same 404. Treated as a good answer that happens to carry no data: the
    /// ladder resets and the layer goes back to its ordinary interval, and the
    /// UI says "not published right now" rather than reporting a fault.
    Absent,
    /// The request did not complete, or the origin failed to serve it: a
    /// timeout, a connection error, a 5xx, a 429, a body that would not read.
    /// Worth waiting out, on the ladder.
    ///
    /// A body that arrives and will not parse also lands here, deliberately.
    /// A truncated response and a changed product schema are indistinguishable
    /// from one sample, so claiming permanence would be claiming more than the
    /// code can prove — and at the ceiling a retried parse failure costs
    /// exactly what a healthy poll costs.
    Transient,
    /// Repetition cannot help: the origin understood the request and refused it
    /// (a 4xx that is not 404/408/429), or the request could not be built at
    /// all — a client that will not construct, or a handler that cannot produce
    /// a fetch task. The same code will do the same thing next time.
    ///
    /// Recorded in the state and **not retried automatically**. Only a user
    /// action revives it, because a user may know something we do not.
    Permanent,
}

/// What a 404 means *for a particular endpoint*, which is not a global fact.
///
/// SPC removes an outlook when it expires, so a 404 there is routine. The MD
/// RSS feed is supposed to exist at all times — a 404 from it means the path
/// moved, which no amount of retrying fixes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotFound {
    /// This product is published on a schedule and is simply not up right now.
    IsRoutine,
    /// This path should always exist; its absence is a product change.
    IsBroken,
}

/// A fetch failure with the verdict attached, so the scheduler does not have to
/// guess it back out of a message string.
///
/// `Display`s as the bare message, so existing `format!("{e}")` log sites read
/// exactly as they did before this type existed.
#[derive(Debug, Clone)]
pub struct FetchError {
    pub failure: FetchFailure,
    pub message: String,
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl FetchError {
    pub fn absent(message: impl Into<String>) -> Self {
        Self {
            failure: FetchFailure::Absent,
            message: message.into(),
        }
    }

    pub fn transient(message: impl Into<String>) -> Self {
        Self {
            failure: FetchFailure::Transient,
            message: message.into(),
        }
    }

    pub fn permanent(message: impl Into<String>) -> Self {
        Self {
            failure: FetchFailure::Permanent,
            message: message.into(),
        }
    }

    /// The verdict for a status the origin actually returned.
    ///
    /// 408 (timeout) and 429 (rate limited) are 4xx that explicitly invite a
    /// later retry, so they are transient despite the class. Every other 4xx is
    /// the origin saying it understood and refused.
    pub fn from_status(
        status: reqwest::StatusCode,
        not_found: NotFound,
        message: impl Into<String>,
    ) -> Self {
        let message = message.into();
        if (status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::GONE)
            && not_found == NotFound::IsRoutine
        {
            return Self::absent(message);
        }
        let failure = if status == reqwest::StatusCode::REQUEST_TIMEOUT
            || status == reqwest::StatusCode::TOO_MANY_REQUESTS
            || status.is_server_error()
        {
            FetchFailure::Transient
        } else if status.is_client_error() {
            FetchFailure::Permanent
        } else {
            // A 1xx/3xx that reached an `is_success` check and failed it: not a
            // refusal, so do not condemn the layer over it.
            FetchFailure::Transient
        };
        Self { failure, message }
    }

    /// The verdict for a `reqwest` error — a request that never produced a
    /// status.
    ///
    /// **Honest limitation on web**: a CORS rejection and a dead network are
    /// the same opaque `TypeError: Failed to fetch` by the browser's deliberate
    /// design, and no amount of inspection here recovers the difference. A CORS
    /// failure is therefore retried as transient rather than condemned. It costs
    /// the ceiling rate — one request per poll interval, the same as a healthy
    /// layer — so mistaking one for the other is bounded. The CORS failure this
    /// codebase actually had (a `User-Agent` turning SPC's `GET` into a
    /// preflight) is prevented at the client, not diagnosed here; see
    /// [`crate::spc::fetch`].
    pub fn from_transport(err: &reqwest::Error, message: impl Into<String>) -> Self {
        let message = message.into();
        if let Some(status) = err.status() {
            // `error_for_status()` funnels here; a status is a status.
            return Self::from_status(status, NotFound::IsBroken, message);
        }
        if err.is_builder() {
            return Self::permanent(message);
        }
        Self::transient(message)
    }
}

/// What the last fetch said, in the terms the options panel needs.
///
/// Distinguishing [`Absent`](FetchHealth::Absent) from
/// [`Failing`](FetchHealth::Failing) is the whole point: "no discussion right
/// now" and "we cannot reach the SPC" produce the same empty map, and a user
/// who cannot tell them apart cannot tell whether the quiet is the weather or
/// the app.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum FetchHealth {
    /// Nothing has failed since the last good answer.
    #[default]
    Ok,
    /// The origin says this product is not published right now.
    Absent,
    /// Failing, but a retry could still work.
    Failing { message: String, attempts: u32 },
    /// Will not succeed by repetition; waiting changes nothing.
    Broken { message: String },
}

/// The per-layer record of what the last fetch did and what the next automatic
/// one is allowed to do.
///
/// Holds **facts** — how many consecutive failures, when the last one landed —
/// and not the schedule derived from them. The ladder is applied at read time
/// by [`backoff_remaining`](FetchRetry::backoff_remaining), where the caller's
/// poll interval (the ceiling) is in scope. That keeps one copy of the policy
/// and means a handler recording a failure does not have to know its own
/// interval to do it.
#[derive(Debug, Clone, Default)]
pub struct FetchRetry {
    /// Consecutive failures since the last good answer.
    failures: u32,
    /// When the most recent failure landed.
    last_failure: Option<web_time::Instant>,
    health: FetchHealth,
}

impl FetchRetry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn health(&self) -> &FetchHealth {
        &self.health
    }

    /// Consecutive failures since the last good answer. Test seam and panel
    /// copy; the schedule reads it through [`backoff_remaining`].
    ///
    /// [`backoff_remaining`]: FetchRetry::backoff_remaining
    pub fn failures(&self) -> u32 {
        self.failures
    }

    /// Nothing automatic will run again until a user asks.
    pub fn is_broken(&self) -> bool {
        matches!(self.health, FetchHealth::Broken { .. })
    }

    /// A good answer: the ladder resets and the layer returns to its interval.
    pub fn record_success(&mut self) {
        self.failures = 0;
        self.last_failure = None;
        self.health = FetchHealth::Ok;
    }

    /// Wipe the ledger for a fetch the **user** asked for, so no user action is
    /// ever made to wait out a backoff — including one that had been given up
    /// on as [`FetchFailure::Permanent`]. A user pressing Refresh may know
    /// something we do not (they fixed their network; the product came back),
    /// and one request per press is bounded by the human pressing it.
    pub fn clear(&mut self) {
        self.record_success();
    }

    /// File a failure against the ladder. `Absent` resets it instead: the
    /// origin answered, and "not published right now" is an answer.
    pub fn record_failure(&mut self, error: &FetchError) {
        match error.failure {
            FetchFailure::Absent => {
                self.failures = 0;
                self.last_failure = None;
                self.health = FetchHealth::Absent;
            }
            FetchFailure::Transient => {
                self.failures = self.failures.saturating_add(1);
                self.last_failure = Some(web_time::Instant::now());
                self.health = FetchHealth::Failing {
                    message: error.message.clone(),
                    attempts: self.failures,
                };
            }
            FetchFailure::Permanent => {
                self.failures = self.failures.saturating_add(1);
                self.last_failure = Some(web_time::Instant::now());
                self.health = FetchHealth::Broken {
                    message: error.message.clone(),
                };
            }
        }
    }

    /// The gap the ladder demands after `failures` consecutive failures:
    /// [`FIRST_RETRY_SECS`] doubling, clamped to `interval`.
    ///
    /// `Duration::ZERO` with no failures on record, so a healthy layer is
    /// governed purely by its poll clock.
    pub fn backoff(&self, interval: Duration) -> Duration {
        if self.failures == 0 {
            return Duration::ZERO;
        }
        let steps = (self.failures - 1).min(MAX_LADDER_STEPS);
        let secs = FIRST_RETRY_SECS.saturating_mul(1u64 << steps);
        Duration::from_secs(secs).min(interval)
    }

    /// How much of the backoff is still outstanding, given the layer's own poll
    /// interval as the ceiling.
    pub fn backoff_remaining(&self, interval: Duration) -> Duration {
        match self.last_failure {
            None => Duration::ZERO,
            Some(at) => self.backoff(interval).saturating_sub(at.elapsed()),
        }
    }

    /// Age the ledger, as though `by` had passed since the last failure.
    ///
    /// A test seam. `FetchRetry` is written against `web_time::Instant` and has
    /// no injectable now — the same shape as `AutoPollState` next door, whose
    /// tests reach for `Instant::now() - ago` directly. Climbing a ladder whose
    /// upper rungs are minutes apart is not something a test can sit through,
    /// and asserting the arithmetic alone would leave the frame-level claim
    /// untested.
    #[doc(hidden)]
    pub fn rewind(&mut self, by: Duration) {
        if let Some(at) = self.last_failure {
            self.last_failure = Some(at - by);
        }
    }

    /// One line for the layer's options panel, or `None` while all is well.
    ///
    /// Phrased so the three cases cannot be confused for each other by someone
    /// glancing at a blank map.
    pub fn status_note(&self) -> Option<String> {
        match &self.health {
            FetchHealth::Ok => None,
            FetchHealth::Absent => Some("Not published right now".to_string()),
            FetchHealth::Failing { message, attempts } => Some(format!(
                "Cannot reach the source - retrying ({attempts} failed): {message}"
            )),
            FetchHealth::Broken { message } => Some(format!(
                "Will not load; use Refresh to try again: {message}"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transient() -> FetchError {
        FetchError::transient("network down")
    }

    const INTERVAL: Duration = Duration::from_secs(120);

    /// The ladder itself: 2 s doubling, clamped to the layer's interval and
    /// never past it. The clamp is the claim — a constant ceiling would keep
    /// climbing past 120 s here, and a missing one would climb forever.
    #[test]
    fn the_ladder_doubles_from_two_seconds_and_stops_at_the_poll_interval() {
        let mut retry = FetchRetry::new();
        assert_eq!(
            retry.backoff(INTERVAL),
            Duration::ZERO,
            "a layer with nothing on record waits only for its poll clock",
        );

        let expected = [2, 4, 8, 16, 32, 64, 120, 120, 120];
        for (i, secs) in expected.iter().enumerate() {
            retry.record_failure(&transient());
            assert_eq!(
                retry.backoff(INTERVAL),
                Duration::from_secs(*secs),
                "failure {} of the ladder",
                i + 1,
            );
        }
    }

    /// The number this whole module exists to move. 3089 attempts in 105 s
    /// measured; the ladder spends 6 in the same window.
    ///
    /// Counted by walking the schedule rather than by driving frames — the
    /// frame-level proof lives in `rustdar-egui`'s `overlay_retry_tests`, which
    /// drives the real poll gate. This one pins the arithmetic that gate reads.
    #[test]
    fn the_measured_storm_window_costs_six_attempts() {
        let mut retry = FetchRetry::new();
        let mut clock = Duration::ZERO;
        let mut attempts = 0;
        // The first attempt is the one that failed at t=0.
        retry.record_failure(&transient());
        attempts += 1;
        loop {
            clock += retry.backoff(INTERVAL);
            if clock > Duration::from_secs(105) {
                break;
            }
            retry.record_failure(&transient());
            attempts += 1;
        }
        assert_eq!(
            attempts, 6,
            "the 105 s that produced 3089 requests must now produce 6",
        );
    }

    /// A 404 from a product that is published on a schedule is an answer, not a
    /// fault: no ladder, and a state the panel can distinguish from a failure.
    #[test]
    fn an_absent_product_is_an_answer_rather_than_a_failure() {
        let mut retry = FetchRetry::new();
        retry.record_failure(&transient());
        retry.record_failure(&transient());
        assert_eq!(retry.failures(), 2);

        retry.record_failure(&FetchError::absent("HTTP 404"));
        assert_eq!(
            retry.failures(),
            0,
            "an absent product must reset the ladder, not climb it",
        );
        assert_eq!(retry.backoff(INTERVAL), Duration::ZERO);
        assert_eq!(retry.health(), &FetchHealth::Absent);
        assert_ne!(
            retry.status_note(),
            None,
            "the panel must be able to say 'not published right now'",
        );
    }

    /// The bar's own words: a permanent failure is said in the state rather
    /// than retried forever at a slow cadence.
    #[test]
    fn a_permanent_failure_stops_rather_than_slowing_down() {
        let mut retry = FetchRetry::new();
        retry.record_failure(&FetchError::permanent("HTTP 403"));
        assert!(retry.is_broken());
        assert!(
            retry.status_note().is_some_and(|n| n.contains("Refresh")),
            "a broken layer must tell the user what would revive it",
        );
    }

    /// A user action never waits: Refresh clears the ladder even when the layer
    /// had been given up on entirely.
    #[test]
    fn a_user_refresh_clears_even_a_permanent_verdict() {
        let mut retry = FetchRetry::new();
        retry.record_failure(&FetchError::permanent("HTTP 403"));
        assert!(retry.is_broken());

        retry.clear();
        assert!(!retry.is_broken());
        assert_eq!(retry.failures(), 0);
        assert_eq!(retry.backoff(INTERVAL), Duration::ZERO);
        assert_eq!(retry.status_note(), None);
    }

    /// A success mid-ladder puts the layer back on its ordinary interval.
    #[test]
    fn a_success_resets_the_ladder() {
        let mut retry = FetchRetry::new();
        for _ in 0..4 {
            retry.record_failure(&transient());
        }
        assert_eq!(retry.backoff(INTERVAL), Duration::from_secs(16));
        retry.record_success();
        assert_eq!(retry.backoff(INTERVAL), Duration::ZERO);
        assert_eq!(retry.status_note(), None);
    }

    /// A layer left failing for a very long time must stay at its ceiling, not
    /// wrap to zero. `2u64 << 63` is 0, so a `checked_shl` ladder would put a
    /// week-old failure straight back into a per-frame retry.
    #[test]
    fn a_long_outage_stays_at_the_ceiling_rather_than_wrapping_to_zero() {
        let mut retry = FetchRetry::new();
        for _ in 0..10_000 {
            retry.record_failure(&transient());
        }
        assert_eq!(
            retry.backoff(INTERVAL),
            INTERVAL,
            "the ladder wrapped and re-armed the storm",
        );
    }

    /// Status classification, endpoint by endpoint. The 404 rows are the point:
    /// the same status is routine for one path and broken for another.
    #[test]
    fn statuses_are_classified_by_what_a_retry_could_do() {
        use reqwest::StatusCode;
        let cases = [
            (
                StatusCode::NOT_FOUND,
                NotFound::IsRoutine,
                FetchFailure::Absent,
            ),
            (StatusCode::GONE, NotFound::IsRoutine, FetchFailure::Absent),
            (
                StatusCode::NOT_FOUND,
                NotFound::IsBroken,
                FetchFailure::Permanent,
            ),
            (
                StatusCode::FORBIDDEN,
                NotFound::IsBroken,
                FetchFailure::Permanent,
            ),
            (
                StatusCode::BAD_REQUEST,
                NotFound::IsBroken,
                FetchFailure::Permanent,
            ),
            (
                StatusCode::REQUEST_TIMEOUT,
                NotFound::IsBroken,
                FetchFailure::Transient,
            ),
            (
                StatusCode::TOO_MANY_REQUESTS,
                NotFound::IsBroken,
                FetchFailure::Transient,
            ),
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                NotFound::IsBroken,
                FetchFailure::Transient,
            ),
            (
                StatusCode::BAD_GATEWAY,
                NotFound::IsBroken,
                FetchFailure::Transient,
            ),
            (
                StatusCode::SERVICE_UNAVAILABLE,
                NotFound::IsBroken,
                FetchFailure::Transient,
            ),
        ];
        for (status, not_found, expected) in cases {
            assert_eq!(
                FetchError::from_status(status, not_found, "x").failure,
                expected,
                "{status} with {not_found:?}",
            );
        }
    }

    /// The message survives classification, so the log line a user reports is
    /// still the origin's own words.
    #[test]
    fn the_message_reads_as_it_did_before_the_verdict_was_attached() {
        let e = FetchError::from_status(
            reqwest::StatusCode::FORBIDDEN,
            NotFound::IsBroken,
            "SPC returned HTTP 403 for MD RSS feed",
        );
        assert_eq!(format!("{e}"), "SPC returned HTTP 403 for MD RSS feed");
    }
}
