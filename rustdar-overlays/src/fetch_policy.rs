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
//! network error is worth waiting out; and a request the origin refuses over and
//! over will not start working because we ask once more at the same cadence, so
//! it is recorded as broken and dropped to a much slower one.
//!
//! # No single answer condemns a layer, and nothing is condemned for ever
//!
//! Two rules, both about the same failure: **a weather warning layer that goes
//! on painting a frozen alert set with nothing on screen to say so.**
//!
//! One 4xx used to be terminal. [`FetchError::from_status`] returned
//! [`Permanent`](FetchFailure::Permanent) on the first 403,
//! [`record_failure`](FetchRetry::record_failure) wrote
//! [`Broken`](FetchHealth::Broken), and `auto_fetch_delay` then returned `None`
//! for the rest of the session. That state was **absorbing by construction** —
//! no automatic fetch could run, so no success could ever clear it — and the
//! only thing that cleared it was a user action on a layer giving no sign it
//! needed one. One CDN hiccup on `api.weather.gov` was enough to freeze NWS
//! alerts at whatever they last said, with the options panel counting up
//! "Updated 47m ago" underneath.
//!
//! So a refusal must repeat [`REFUSALS_BEFORE_BROKEN`] times **in a row** before
//! it is believed, and even then the layer keeps a [`BROKEN_RETRY_SECS`]
//! heartbeat rather than stopping. Being wrong in this direction costs two
//! requests an hour against a genuinely dead endpoint — *less* than the ceiling
//! a healthy layer already spends. Being wrong in the other direction costs a
//! stale tornado warning on screen, indefinitely. The asymmetry is the whole
//! argument.
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

/// How many **consecutive** refusals it takes before a layer is called
/// [`Broken`](FetchHealth::Broken).
///
/// Two, because one is not evidence of anything. A refusal is the origin
/// saying it understood us and said no, but the origins here are public
/// unauthenticated services behind CDNs, and a single 4xx from one of those is
/// far more often a WAF rule, a rate limiter or a bad edge node than a real
/// change in what is published. Asking a second time separates the two at a
/// cost of exactly one extra request.
///
/// Counted separately from [`FetchRetry::failures`]: a transient failure in
/// among the refusals resets it, because "the origin refuses us" is a claim
/// about a *run* of refusals and a timeout in the middle of that run means we
/// do not have one.
pub const REFUSALS_BEFORE_BROKEN: u32 = 2;

/// What a [`Broken`](FetchHealth::Broken) layer waits before trying once more.
///
/// Not `None`. A broken layer used to be off the automatic poll entirely, which
/// made the state absorbing — the only thing that could clear it was a success,
/// and no fetch that could produce one would ever run. Half an hour is long
/// enough that a truly dead endpoint costs two requests an hour, and short
/// enough that a layer condemned by a transient WAF rule comes back on its own
/// while the storm it was drawing is still on the ground.
///
/// Floored at the layer's own interval by [`FetchRetry::backoff`], so this can
/// never make a broken layer poll *faster* than a healthy one.
pub const BROKEN_RETRY_SECS: u64 = 1800;

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
    /// Repetition is unlikely to help: the origin understood the request and
    /// refused it (a 4xx that is not 404/408/429 and not 401/403), or the
    /// request could not be built at all — a client that will not construct, or
    /// a handler that cannot produce a fetch task.
    ///
    /// **One of these is a suspicion, not a verdict.** It takes
    /// [`REFUSALS_BEFORE_BROKEN`] in a row for
    /// [`record_failure`](FetchRetry::record_failure) to write
    /// [`Broken`](FetchHealth::Broken); until then the layer climbs the ordinary
    /// ladder like any other failure. And [`Broken`](FetchHealth::Broken) is
    /// still not "never again" — see [`BROKEN_RETRY_SECS`].
    Permanent,
}

impl FetchFailure {
    /// The verdict for a round made of several requests — GLM's two satellites,
    /// METAR's per-state networks, HRRR's two candidate model runs.
    ///
    /// A round is refused only when **every** part of it was refused, and absent
    /// only when every part was absent. Anything mixed is transient: one part
    /// that could still work next time makes the round worth trying again, and
    /// condemning a layer on the strength of its weakest component is how a
    /// single dead state network takes every METAR in the country off the map.
    ///
    /// An empty round is `Transient` — no evidence is not evidence of refusal.
    pub fn of_round(parts: impl IntoIterator<Item = Self>) -> Self {
        let mut all_permanent = true;
        let mut all_absent = true;
        let mut any = false;
        for part in parts {
            any = true;
            all_permanent &= part == Self::Permanent;
            all_absent &= part == Self::Absent;
        }
        match (any, all_permanent, all_absent) {
            (true, true, _) => Self::Permanent,
            (true, _, true) => Self::Absent,
            _ => Self::Transient,
        }
    }
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

    /// One error for a round of several requests that **all** failed: the
    /// merged verdict from [`FetchFailure::of_round`], with every part's own
    /// words kept behind `context`.
    ///
    /// Four layers issue rounds — GLM's satellites, METAR's per-state networks,
    /// HRRR's two candidate runs, storm reports' three CSVs — and each had
    /// grown its own version of this. Storm reports' was `failures.remove(0)`,
    /// which is not a merge at all: `failures[0]` is always the tornado CSV, so
    /// one 400 there condemned the layer while hail and wind had merely timed
    /// out. One helper and one rule, so a caller cannot quietly pick a stricter
    /// one.
    ///
    /// Every part's message is kept rather than a representative one: the panel
    /// line is what a user reads back when reporting a fault, and "which ones
    /// failed, and how" is the whole content of a round failure.
    pub fn of_round(parts: &[FetchError], context: impl Into<String>) -> Self {
        let context = context.into();
        Self {
            failure: FetchFailure::of_round(parts.iter().map(|e| e.failure)),
            message: format!(
                "{context}: {}",
                parts
                    .iter()
                    .map(|e| e.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
        }
    }

    /// The verdict for a status the origin actually returned.
    ///
    /// 408 (timeout) and 429 (rate limited) are 4xx that explicitly invite a
    /// later retry, so they are transient despite the class.
    ///
    /// **401 and 403 join them**, which is not what the class means and is
    /// deliberate. Every origin this app talks to — `api.weather.gov`, SPC,
    /// IEM, the public NOAA S3 buckets — is public and unauthenticated, so
    /// there is no credential for a 401 to be about and no permission for a 403
    /// to be withholding. What actually produces them is the layer in front:
    /// a WAF rule that dislikes a header, an over-eager rate limiter answering
    /// 403 instead of 429, an edge node with a stale config. All three clear on
    /// their own, and all three used to take an alerts layer off the poll for
    /// the rest of the session on a single sample. Retrying them costs the
    /// ceiling — one request per poll interval, what a healthy layer costs.
    ///
    /// What is left as a refusal is the 4xx that is a property of the *request*
    /// rather than of the moment: 400 (we built it wrong), 451 (it is blocked
    /// where the user is), 405/414/422 and the like. Asking again cannot change
    /// any of those, because nothing about the next request differs. Even so it
    /// takes [`REFUSALS_BEFORE_BROKEN`] of them in a row to be believed.
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
            || status == reqwest::StatusCode::UNAUTHORIZED
            || status == reqwest::StatusCode::FORBIDDEN
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
    /// The origin has refused us [`REFUSALS_BEFORE_BROKEN`] times running, so
    /// the ordinary ladder is not worth spending. Dropped to a
    /// [`BROKEN_RETRY_SECS`] heartbeat rather than stopped — see
    /// [`FetchRetry::backoff`].
    Broken { message: String },
}

impl FetchHealth {
    /// Whether what this layer is holding is older than it looks: the last
    /// fetch did not land and nothing has succeeded since.
    ///
    /// [`Absent`](FetchHealth::Absent) is **not** unhealthy — the origin
    /// answered, and "not published right now" is an answer. That distinction
    /// is why this is a method here rather than a `!= Ok` at each call site.
    pub fn is_unhealthy(&self) -> bool {
        matches!(self, Self::Failing { .. } | Self::Broken { .. })
    }
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
    /// Consecutive *refusals* since the last good answer or last non-refusal —
    /// the counter [`REFUSALS_BEFORE_BROKEN`] is measured against. Separate
    /// from `failures` on purpose; see that constant.
    refusals: u32,
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

    /// The origin has refused us [`REFUSALS_BEFORE_BROKEN`] times running.
    /// The automatic poll drops to [`BROKEN_RETRY_SECS`]; it does not stop.
    pub fn is_broken(&self) -> bool {
        matches!(self.health, FetchHealth::Broken { .. })
    }

    /// Whether what the layer is holding is older than it looks — see
    /// [`FetchHealth::is_unhealthy`]. What the enable-fetch rule reads to
    /// decide that switching a layer back on should re-ask rather than trust
    /// what is already drawn.
    pub fn is_unhealthy(&self) -> bool {
        self.health.is_unhealthy()
    }

    /// A good answer: the ladder resets and the layer returns to its interval.
    pub fn record_success(&mut self) {
        self.failures = 0;
        self.refusals = 0;
        self.last_failure = None;
        self.health = FetchHealth::Ok;
    }

    /// Wipe the ledger for a fetch the **user** asked for, so no user action is
    /// ever made to wait out a backoff — including a layer already recorded as
    /// [`Broken`](FetchHealth::Broken). A user pressing Refresh, or switching a
    /// stale layer off and on, may know something we do not (they fixed their
    /// network; the product came back), and one request per press is bounded by
    /// the human pressing it.
    pub fn clear(&mut self) {
        self.record_success();
    }

    /// File a failure against the ladder. `Absent` resets it instead: the
    /// origin answered, and "not published right now" is an answer.
    ///
    /// A [`Permanent`](FetchFailure::Permanent) verdict climbs the same ladder
    /// as a transient one until it has repeated [`REFUSALS_BEFORE_BROKEN`]
    /// times in a row. One refusal reads as `Failing`, which is exactly what it
    /// is: something is wrong, we do not yet know that repetition cannot fix
    /// it, and we are still asking.
    pub fn record_failure(&mut self, error: &FetchError) {
        match error.failure {
            FetchFailure::Absent => {
                self.failures = 0;
                self.refusals = 0;
                self.last_failure = None;
                self.health = FetchHealth::Absent;
            }
            FetchFailure::Transient => {
                self.failures = self.failures.saturating_add(1);
                // A timeout in the middle of a run of refusals means we do not
                // have a run of refusals.
                self.refusals = 0;
                self.last_failure = Some(web_time::Instant::now());
                self.health = FetchHealth::Failing {
                    message: error.message.clone(),
                    attempts: self.failures,
                };
            }
            FetchFailure::Permanent => {
                self.failures = self.failures.saturating_add(1);
                self.refusals = self.refusals.saturating_add(1);
                self.last_failure = Some(web_time::Instant::now());
                self.health = if self.refusals >= REFUSALS_BEFORE_BROKEN {
                    FetchHealth::Broken {
                        message: error.message.clone(),
                    }
                } else {
                    FetchHealth::Failing {
                        message: error.message.clone(),
                        attempts: self.failures,
                    }
                };
            }
        }
    }

    /// The gap the ladder demands after `failures` consecutive failures:
    /// [`FIRST_RETRY_SECS`] doubling, clamped to `interval`.
    ///
    /// `Duration::ZERO` with no failures on record, so a healthy layer is
    /// governed purely by its poll clock.
    ///
    /// A [`broken`](FetchRetry::is_broken) layer leaves the ladder for a single
    /// long rung, [`BROKEN_RETRY_SECS`], floored at `interval` so it can never
    /// poll faster than a healthy layer would. That is one expression of the
    /// whole schedule: `auto_fetch_delay` used to special-case broken with a
    /// `None` of its own, and a `None` there is a state nothing can leave,
    /// because the only thing that clears it is a success and no fetch that
    /// could produce one ever runs.
    pub fn backoff(&self, interval: Duration) -> Duration {
        if self.is_broken() {
            return Duration::from_secs(BROKEN_RETRY_SECS).max(interval);
        }
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
    /// glancing at a blank map — and, for the two failing ones, so that the
    /// "Updated 47m ago" line beneath cannot be read as the whole story. That
    /// pairing is the point: a stale alert set and a fresh one look identical
    /// on the map, and this is the only thing that tells them apart.
    ///
    /// Rendered for **every** layer by `OverlayRegistry::controls`, not by each
    /// handler; four of six used to forget, and the four that forgot included
    /// NWS alerts.
    pub fn status_note(&self) -> Option<String> {
        match &self.health {
            FetchHealth::Ok => None,
            FetchHealth::Absent => Some("Not published right now".to_string()),
            FetchHealth::Failing { message, attempts } => Some(format!(
                "Not loading - what is shown may be stale. Retrying ({attempts} failed): {message}"
            )),
            FetchHealth::Broken { message } => Some(format!(
                "Not loading - what is shown may be stale. Retrying rarely now; \
                 use Refresh to try again at once: {message}"
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

    /// **The stale-warning test.** One refusal is a suspicion, not a verdict:
    /// the layer stays on the ordinary ladder and keeps asking.
    ///
    /// The bug: a single 403 wrote `Broken`, `auto_fetch_delay` then returned
    /// `None` for the session, and the alerts layer went on painting whatever
    /// warnings it last held. Set `REFUSALS_BEFORE_BROKEN` to 1 and this fails.
    #[test]
    fn one_refusal_does_not_condemn_a_layer() {
        let mut retry = FetchRetry::new();
        retry.record_failure(&FetchError::permanent("HTTP 403"));
        assert!(
            !retry.is_broken(),
            "one 4xx took the layer off the poll — a CDN hiccup must not freeze \
             a warning layer for the session",
        );
        assert_eq!(
            retry.backoff(INTERVAL),
            Duration::from_secs(FIRST_RETRY_SECS),
            "a first refusal must climb the ordinary ladder like any failure",
        );
        assert!(
            retry.status_note().is_some_and(|n| n.contains("stale")),
            "a layer that is failing must say what that means for what is drawn",
        );
    }

    /// Refusals have to be *consecutive*: a transient in among them means we do
    /// not have a run, and the count starts over.
    #[test]
    fn a_transient_between_refusals_resets_the_refusal_count() {
        let mut retry = FetchRetry::new();
        retry.record_failure(&FetchError::permanent("HTTP 400"));
        retry.record_failure(&transient());
        retry.record_failure(&FetchError::permanent("HTTP 400"));
        assert!(
            !retry.is_broken(),
            "two refusals with a timeout between them are not two refusals in a row",
        );
        retry.record_failure(&FetchError::permanent("HTTP 400"));
        assert!(retry.is_broken(), "two in a row must still be believed");
    }

    /// `REFUSALS_BEFORE_BROKEN` in a row is the evidence bar, and clearing it
    /// says so in the state.
    #[test]
    fn a_run_of_refusals_is_believed_and_said_out_loud() {
        let mut retry = FetchRetry::new();
        for i in 1..=REFUSALS_BEFORE_BROKEN {
            retry.record_failure(&FetchError::permanent("HTTP 400"));
            assert_eq!(
                retry.is_broken(),
                i == REFUSALS_BEFORE_BROKEN,
                "refusal {i} of {REFUSALS_BEFORE_BROKEN}",
            );
        }
        let note = retry.status_note().expect("a broken layer must say so");
        assert!(
            note.contains("Refresh"),
            "a broken layer must tell the user what would revive it: {note}",
        );
        assert!(
            note.contains("stale"),
            "a broken layer must say what it means for what is drawn: {note}",
        );
    }

    /// Broken is a slower poll, never a stopped one. The old `None` was
    /// absorbing by construction: nothing automatic ran, so nothing could
    /// succeed, so nothing could ever clear it.
    #[test]
    fn a_broken_layer_still_gets_a_heartbeat() {
        let mut retry = FetchRetry::new();
        for _ in 0..REFUSALS_BEFORE_BROKEN {
            retry.record_failure(&FetchError::permanent("HTTP 400"));
        }
        assert!(retry.is_broken());

        let gap = retry.backoff(INTERVAL);
        assert_eq!(gap, Duration::from_secs(BROKEN_RETRY_SECS));
        assert!(
            gap >= INTERVAL,
            "a broken layer must never poll faster than a healthy one",
        );
        assert!(
            retry.backoff_remaining(INTERVAL) > Duration::ZERO,
            "premise: the heartbeat has not come due yet",
        );

        retry.rewind(gap);
        assert_eq!(
            retry.backoff_remaining(INTERVAL),
            Duration::ZERO,
            "the heartbeat never came due — broken is still a dead end",
        );
    }

    /// The heartbeat is floored at the layer's own interval, so a layer that
    /// polls *more* slowly than the heartbeat is not sped up by breaking.
    #[test]
    fn the_broken_heartbeat_never_outpaces_a_slow_layer() {
        let slow = Duration::from_secs(BROKEN_RETRY_SECS * 3);
        let mut retry = FetchRetry::new();
        for _ in 0..REFUSALS_BEFORE_BROKEN {
            retry.record_failure(&FetchError::permanent("HTTP 400"));
        }
        assert_eq!(retry.backoff(slow), slow);
    }

    /// A user action never waits: Refresh clears the ladder even when the layer
    /// had been given up on entirely.
    #[test]
    fn a_user_refresh_clears_even_a_permanent_verdict() {
        let mut retry = FetchRetry::new();
        for _ in 0..REFUSALS_BEFORE_BROKEN {
            retry.record_failure(&FetchError::permanent("HTTP 400"));
        }
        assert!(retry.is_broken());

        retry.clear();
        assert!(!retry.is_broken());
        assert!(!retry.is_unhealthy());
        assert_eq!(retry.failures(), 0);
        assert_eq!(retry.backoff(INTERVAL), Duration::ZERO);
        assert_eq!(retry.status_note(), None);
    }

    /// A success after a run of refusals clears the refusal count too, not just
    /// the failure count — otherwise one more refusal months later would break
    /// a layer that had been healthy in between.
    #[test]
    fn a_success_clears_the_refusal_count_as_well() {
        let mut retry = FetchRetry::new();
        for _ in 0..REFUSALS_BEFORE_BROKEN {
            retry.record_failure(&FetchError::permanent("HTTP 400"));
        }
        retry.record_success();
        retry.record_failure(&FetchError::permanent("HTTP 400"));
        assert!(
            !retry.is_broken(),
            "a refusal after a good answer is the first of a new run, not the last of an old one",
        );
    }

    /// Only [`FetchHealth::Failing`] and [`FetchHealth::Broken`] mean "what is
    /// drawn may be stale". `Absent` is an answer and must not read as a fault.
    #[test]
    fn absent_is_not_unhealthy() {
        let mut retry = FetchRetry::new();
        assert!(!retry.is_unhealthy(), "a fresh ledger is not unhealthy");
        retry.record_failure(&FetchError::absent("HTTP 404"));
        assert!(
            !retry.is_unhealthy(),
            "'not published right now' is an answer, not staleness",
        );
        retry.record_failure(&transient());
        assert!(retry.is_unhealthy());
    }

    /// The round *error* keeps every part's words, not a representative one.
    ///
    /// Storm reports used to take `failures[0]` — always the tornado CSV — so
    /// the layer's whole verdict turned on one of the three, and a 400 there
    /// condemned it while hail and wind had merely timed out. All four
    /// round-issuing layers go through this now.
    #[test]
    fn a_round_error_carries_every_part_and_the_merged_verdict() {
        let parts = [
            FetchError::permanent("tornado CSV: HTTP 400"),
            FetchError::transient("hail CSV: timed out"),
            FetchError::transient("wind CSV: timed out"),
        ];
        let round = FetchError::of_round(&parts, "no storm report CSV could be fetched");
        assert_eq!(
            round.failure,
            FetchFailure::Transient,
            "one refused CSV among three must not condemn the layer",
        );
        for part in &parts {
            assert!(
                round.message.contains(&part.message),
                "the round dropped {:?}: {}",
                part.message,
                round.message,
            );
        }
        assert!(
            round
                .message
                .starts_with("no storm report CSV could be fetched")
        );
    }

    /// A round of several requests is refused only when every part of it was.
    /// One dead state network must not take every METAR in the country off the
    /// map.
    #[test]
    fn a_round_is_only_refused_when_every_part_of_it_was() {
        use FetchFailure::{Absent, Permanent, Transient};
        let cases: [(&[FetchFailure], FetchFailure); 7] = [
            (&[Permanent, Permanent], Permanent),
            (&[Permanent, Transient], Transient),
            (&[Permanent, Absent], Transient),
            (&[Absent, Absent], Absent),
            (&[Transient, Transient], Transient),
            (&[Permanent], Permanent),
            (&[], Transient),
        ];
        for (parts, expected) in cases {
            assert_eq!(
                FetchFailure::of_round(parts.iter().copied()),
                expected,
                "{parts:?}",
            );
        }
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
            // The two that changed sides, and the reason this table has a
            // comment: these origins are public and unauthenticated, so a 401
            // or a 403 from one is a WAF rule or a rate limiter wearing the
            // wrong status, not a refusal that will still be there tomorrow.
            (
                StatusCode::FORBIDDEN,
                NotFound::IsBroken,
                FetchFailure::Transient,
            ),
            (
                StatusCode::UNAUTHORIZED,
                NotFound::IsBroken,
                FetchFailure::Transient,
            ),
            // A property of the request, not of the moment: the next one is
            // identical, so it gets the identical answer.
            (
                StatusCode::BAD_REQUEST,
                NotFound::IsBroken,
                FetchFailure::Permanent,
            ),
            (
                StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS,
                NotFound::IsBroken,
                FetchFailure::Permanent,
            ),
            (
                StatusCode::METHOD_NOT_ALLOWED,
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
