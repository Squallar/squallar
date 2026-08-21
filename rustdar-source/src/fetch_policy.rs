//! When a failing overlay fetch is allowed to try again.
//!
//! The ladder climbs from [`FIRST_RETRY_SECS`] to the handler's own
//! [`auto_poll_interval`] and stops: a retry faster than that interval cannot
//! deliver anything the next ordinary poll would not. Failure kinds differ —
//! see [`FetchFailure`]. Coverage (whether what is drawn is all of what
//! arrived) is a separate axis from staleness — see [`DataCompleteness`].
//!
//! [`auto_poll_interval`]: crate::handler::SourceHandler::auto_poll_interval

use std::time::Duration;

/// The first retry after a transient failure. Doubles from here, clamped to the layer's poll interval.
pub const FIRST_RETRY_SECS: u64 = 2;

/// Caps the doubling before it can overflow the shift.
const MAX_LADDER_STEPS: u32 = 32;

/// How many **consecutive** refusals it takes before a layer is called
/// [`Broken`](FetchHealth::Broken).
pub const REFUSALS_BEFORE_BROKEN: u32 = 2;

/// What a [`Broken`](FetchHealth::Broken) layer waits before trying once more.
///
/// Floored at the layer's own interval by [`FetchRetry::backoff`], so a broken layer never polls faster than a healthy one.
pub const BROKEN_RETRY_SECS: u64 = 1800;

/// What a failed fetch tells us about whether trying again could work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchFailure {
    /// The origin answered: the product is not there right now (404/410 from an
    /// endpoint published on a schedule).
    Absent,
    /// The request did not complete, or the origin failed to serve it: a timeout,
    /// a connection error, a 5xx, a 429, a body that would not read.
    Transient,
    /// Repetition is unlikely to help: the origin understood the request and
    /// refused it (a 4xx that is not 404/408/429 and not 401/403), or the request
    /// could not be built at all.
    Permanent,
}

impl FetchFailure {
    /// The verdict for a round made of several requests.
    ///
    /// A round is refused only when **every** part of it was refused, and absent
    /// only when every part was absent. Anything mixed is transient.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotFound {
    /// This product is published on a schedule and is simply not up right now.
    IsRoutine,
    /// This path should always exist; its absence is a product change.
    IsBroken,
}

/// A fetch failure with the verdict attached.
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

    /// One error for a round of several requests that **all** failed: the merged
    /// verdict, with every part's own words kept behind `context`.
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
    /// 408 and 429 are 4xx that explicitly invite a later retry. **401 and 403 join
    /// them**: every origin here is public and unauthenticated, so those are a WAF
    /// rule or a rate limiter. What is left as a refusal is the 4xx that is a
    /// property of the *request*: 400, 451, 405/414/422 and the like.
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
            // A 1xx/3xx that reached an `is_success` check and failed it: not a refusal.
            FetchFailure::Transient
        };
        Self { failure, message }
    }

    /// The verdict for a `reqwest` error — a request that never produced a status.
    /// On web a CORS rejection and a dead network are the same opaque
    /// `TypeError: Failed to fetch`, so a CORS failure is retried as transient.
    pub fn from_transport(err: &reqwest::Error, message: impl Into<String>) -> Self {
        let message = message.into();
        if let Some(status) = err.status() {
            return Self::from_status(status, NotFound::IsBroken, message);
        }
        if err.is_builder() {
            return Self::permanent(message);
        }
        Self::transient(message)
    }
}

/// What the last fetch said, in the terms the options panel needs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum FetchHealth {
    /// Nothing has failed since the last good answer.
    #[default]
    Ok,
    /// The origin says this product is not published right now.
    Absent,
    /// Failing, but a retry could still work.
    Failing { message: String, attempts: u32 },
    /// The origin has refused us [`REFUSALS_BEFORE_BROKEN`] times running; dropped
    /// to a [`BROKEN_RETRY_SECS`] heartbeat rather than stopped.
    Broken { message: String },
}

impl FetchHealth {
    /// Whether what this layer is holding is older than it looks.
    pub fn is_unhealthy(&self) -> bool {
        matches!(self, Self::Failing { .. } | Self::Broken { .. })
    }
}

/// What a layer is drawing, against what its last answer said it should draw.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DataCompleteness {
    pub expected: usize,
    /// ...that are drawing only some of their area, not an absent one.
    pub partial: usize,
    /// ...that have nothing at all to draw.
    pub missing: usize,
    /// The pieces those things are assembled from, needed and obtained.
    pub parts_requested: usize,
    pub parts_resolved: usize,
    /// Plural noun for what [`expected`](Self::expected) counts: `"alerts"`.
    pub unit: &'static str,
    /// Plural noun for what the parts are: `"zone boundaries"`.
    pub part_unit: &'static str,
    /// Why the missing parts are missing, commonest first — `("HTTP 503", 198)`.
    pub reasons: Vec<(String, usize)>,
}

impl DataCompleteness {
    /// Everything the answer named is on the map, whole.
    pub fn is_complete(&self) -> bool {
        self.partial == 0 && self.missing == 0
    }

    /// The always-visible half: one word for the layer stack's row, beside
    /// `not updating`. Free to call — asked of every layer on every frame.
    pub fn status_mark(&self) -> Option<&'static str> {
        (!self.is_complete()).then_some("incomplete")
    }

    /// The full sentence for the layer's options panel.
    pub fn status_note(&self) -> Option<String> {
        if self.is_complete() {
            return None;
        }
        let Self {
            expected,
            partial,
            missing,
            unit,
            part_unit,
            ..
        } = self;
        let subject = match (missing, partial) {
            (0, p) => format!("{p} of {expected} {unit} drawing only part of their area"),
            (m, 0) => format!("missing {m} of {expected} {unit}"),
            (m, p) => format!(
                "missing {m} of {expected} {unit}, with {p} more drawing only part of \
                 their area"
            ),
        };
        let mut note = format!("Incomplete - {subject}");
        if self.parts_requested > 0 {
            note.push_str(&format!(
                ". {} of {} {part_unit} resolved",
                self.parts_resolved, self.parts_requested,
            ));
        }
        if !self.reasons.is_empty() {
            note.push_str(": ");
            note.push_str(
                &self
                    .reasons
                    .iter()
                    .map(|(why, count)| {
                        if *count == 1 {
                            why.clone()
                        } else {
                            format!("{count} {why}")
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
        note.push_str(". Not the same as stale data: a refresh retries what is missing.");
        Some(note)
    }
}

/// Whether a layer's fetch round is one answer or several.
pub trait RoundShape: sealed::Sealed {}

/// One request, one answer: it arrived or it did not, and there is no third
/// outcome to declare.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Whole;

/// Several requests, each of which can fail on its own — so the round can end
/// in `Ok`, stamp a fresh clock, read green, and leave part of the map blank.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Assembled;

impl RoundShape for Whole {}
impl RoundShape for Assembled {}

mod sealed {
    pub trait Sealed {}
    impl Sealed for super::Whole {}
    impl Sealed for super::Assembled {}
}

/// What a layer's fetch hands back, and the shape of the round it belongs to.
pub trait FetchRound: 'static {
    /// [`Whole`] or [`Assembled`]; see each for which a round is.
    type Shape: RoundShape;
}

/// The per-layer record of what the last fetch did and what the next automatic
/// one may do.
#[derive(Debug, Clone, Default)]
pub struct FetchRetry {
    /// Consecutive failures since the last good answer.
    failures: u32,
    /// Consecutive *refusals*, the counter [`REFUSALS_BEFORE_BROKEN`] is measured
    /// against.
    refusals: u32,
    /// When the most recent failure landed.
    last_failure: Option<web_time::Instant>,
    health: FetchHealth,
    /// How much of the **last answer that arrived** made it onto the map.
    coverage: DataCompleteness,
}

impl FetchRetry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn health(&self) -> &FetchHealth {
        &self.health
    }

    /// When the failure now on [`health`](Self::health) landed.
    ///
    /// This is the *occurrence* identity, and it is the reason a dismissible
    /// error banner does not need a dismissed state on [`FetchHealth`]: a
    /// dismissal keyed on this instant hides the failure the user dismissed
    /// and not the next one, even when the next one carries a byte-identical
    /// message. `None` for [`FetchHealth::Ok`] and [`FetchHealth::Absent`],
    /// neither of which is a failure a user is asked to acknowledge.
    pub fn last_failure(&self) -> Option<web_time::Instant> {
        self.last_failure
    }

    /// How much of the last answer that arrived is on the map.
    pub fn coverage(&self) -> &DataCompleteness {
        &self.coverage
    }

    /// Record what a **new answer** covered.
    pub fn record_coverage(&mut self, coverage: DataCompleteness) {
        self.coverage = coverage;
    }

    /// Whether the layer is holding less than it was told to draw.
    pub fn is_incomplete(&self) -> bool {
        !self.coverage.is_complete()
    }

    /// Consecutive failures since the last good answer.
    pub fn failures(&self) -> u32 {
        self.failures
    }

    /// The origin has refused us [`REFUSALS_BEFORE_BROKEN`] times running.
    /// The automatic poll drops to [`BROKEN_RETRY_SECS`]; it does not stop.
    pub fn is_broken(&self) -> bool {
        matches!(self.health, FetchHealth::Broken { .. })
    }

    /// Whether what the layer is holding is older than it looks.
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

    /// Wipe the ledger for a fetch the **user** asked for, so no user action waits
    /// out a backoff — including a layer already recorded as `Broken`.
    pub fn clear(&mut self) {
        self.record_success();
    }

    /// File a failure against the ladder. `Absent` resets it instead: the
    /// origin answered, and "not published right now" is an answer.
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
                // A timeout in the middle of a run of refusals means we do not have one.
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

    /// How much of the backoff is still outstanding, given `interval` as ceiling.
    pub fn backoff_remaining(&self, interval: Duration) -> Duration {
        match self.last_failure {
            None => Duration::ZERO,
            Some(at) => self.backoff(interval).saturating_sub(at.elapsed()),
        }
    }

    /// Age the ledger, as though `by` had passed since the last failure.
    #[doc(hidden)]
    pub fn rewind(&mut self, by: Duration) {
        if let Some(at) = self.last_failure {
            self.last_failure = Some(at - by);
        }
    }

    /// One line for the layer's options panel, or `None` while all is well.
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

    /// The ladder itself: 2 s doubling, clamped to the layer's interval.
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

    /// 3089 attempts in 105 s measured; the ladder spends 6 in the same window.
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

    /// A 404 from a product published on a schedule is an answer, not a fault.
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

    /// One refusal is a suspicion, not a verdict: the layer stays on the ladder.
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

    /// Refusals have to be *consecutive*; a transient among them starts the count over.
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

    /// `REFUSALS_BEFORE_BROKEN` in a row is the evidence bar.
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

    /// Broken is a slower poll, never a stopped one.
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

    /// The heartbeat is floored at the layer's own interval.
    #[test]
    fn the_broken_heartbeat_never_outpaces_a_slow_layer() {
        let slow = Duration::from_secs(BROKEN_RETRY_SECS * 3);
        let mut retry = FetchRetry::new();
        for _ in 0..REFUSALS_BEFORE_BROKEN {
            retry.record_failure(&FetchError::permanent("HTTP 400"));
        }
        assert_eq!(retry.backoff(slow), slow);
    }

    /// A user action never waits: Refresh clears the ladder even for a broken layer.
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

    /// A success after a run of refusals clears the refusal count too.
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

    /// Only `Failing` and `Broken` mean "what is drawn may be stale".
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

    /// A round is refused only when every part of it was.
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

    /// A layer left failing must stay at its ceiling, not wrap to zero.
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

    /// Status classification: the same 404 is routine for one path, broken for another.
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
            // These origins are public and unauthenticated: a 401/403 is a WAF rule.
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
            // A property of the request, not of the moment.
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

    /// The three shapes of incomplete read as three different sentences.
    #[test]
    fn the_completeness_note_tells_the_three_shapes_of_incomplete_apart() {
        let base = DataCompleteness {
            expected: 297,
            parts_requested: 1200,
            parts_resolved: 995,
            unit: "alerts",
            part_unit: "zone boundaries",
            reasons: vec![
                ("HTTP 503".to_string(), 198),
                ("no usable boundary".to_string(), 7),
            ],
            ..DataCompleteness::default()
        };

        let missing_only = DataCompleteness {
            missing: 212,
            ..base.clone()
        }
        .status_note()
        .expect("212 alerts with no shape is not a complete answer");
        assert_eq!(
            missing_only,
            "Incomplete - missing 212 of 297 alerts. 995 of 1200 zone boundaries \
             resolved: 198 HTTP 503, 7 no usable boundary. Not the same as stale \
             data: a refresh retries what is missing.",
        );

        let partial_only = DataCompleteness {
            partial: 6,
            ..base.clone()
        }
        .status_note()
        .expect("6 alerts drawing part of themselves is not a complete answer");
        assert!(
            partial_only.contains("6 of 297 alerts drawing only part of their area"),
            "a wrong shape must not be reported as a missing one: {partial_only}",
        );

        let both = DataCompleteness {
            missing: 212,
            partial: 6,
            ..base.clone()
        }
        .status_note()
        .expect("both at once is still not complete");
        assert!(
            both.contains("missing 212 of 297 alerts, with 6 more drawing only part"),
            "both counts must survive: {both}",
        );

        assert_eq!(
            DataCompleteness::default().status_note(),
            None,
            "a layer that assembles nothing must not carry a note",
        );
        assert_eq!(
            base.status_note(),
            None,
            "nothing missing and nothing partial is a whole answer, whatever \
             else the fields say",
        );
    }

    /// A round with no second denominator omits the parts sentence.
    #[test]
    fn a_layer_whose_unit_has_no_parts_omits_the_parts_sentence() {
        let note = DataCompleteness {
            expected: 2,
            missing: 1,
            unit: "satellite feeds",
            reasons: vec![("GOES-19 (East): HTTP 503".to_string(), 1)],
            ..DataCompleteness::default()
        }
        .status_note()
        .expect("a dead satellite is not a complete answer");
        assert_eq!(
            note,
            "Incomplete - missing 1 of 2 satellite feeds: GOES-19 (East): HTTP 503. \
             Not the same as stale data: a refresh retries what is missing.",
        );
        assert!(
            !note.contains(" of 0 "),
            "an absent denominator must not be printed: {note}",
        );
    }

    /// Coverage and health are separate axes; nothing on the ladder moves coverage.
    #[test]
    fn only_a_new_answer_moves_what_the_layer_is_missing() {
        let under_drew = DataCompleteness {
            expected: 297,
            missing: 212,
            unit: "alerts",
            ..DataCompleteness::default()
        };
        let mut retry = FetchRetry::new();
        retry.record_coverage(under_drew.clone());
        assert!(retry.is_incomplete());
        assert!(!retry.is_unhealthy(), "a half round is not a stale one");

        retry.record_failure(&transient());
        assert!(
            retry.is_incomplete(),
            "a failure changed nothing about what is drawn, so it must not \
             change what is reported missing from it",
        );
        assert!(retry.is_unhealthy(), "and the layer is now stale as well");

        retry.record_success();
        assert!(
            retry.is_incomplete(),
            "`record_success` replaced no data - the outlook handler's route - \
             so it cannot claim the missing pieces arrived",
        );
        retry.clear();
        assert!(
            retry.is_incomplete(),
            "a user pressing Refresh has not yet been given the 212 zones that \
             failed",
        );
        assert!(!retry.is_unhealthy(), "but the ladder is theirs to clear");

        retry.record_coverage(DataCompleteness::default());
        assert!(!retry.is_incomplete(), "a whole answer clears it");
    }

    /// The message survives classification.
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
