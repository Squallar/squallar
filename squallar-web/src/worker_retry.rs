//! How long the page waits before starting another rasterization worker.
//!
//! Kept out of [`crate::worker_port`], which is wasm32-gated, so the ladder is
//! driven by the tests at the bottom on a host.

/// What each successive respawn waits, in milliseconds.
///
/// The first rung is short enough to be invisible and the last long enough to be
/// free, and the last **repeats forever** rather than giving up: never retrying
/// costs a second of frame time per volume for the rest of the session, while a
/// retry a minute costs one `Worker` construction and a failed handshake.
///
/// The numbers are a policy, not a measurement. What is measured is the
/// fallback cost at the top of the ladder: 1021.9 ms in Firefox and 911.4 ms in
/// Chrome for a 16.9 MB volume decode.
pub const RESPAWN_BACKOFF_MS: [u32; 4] = [1_000, 4_000, 16_000, 60_000];

/// Which rung of [`RESPAWN_BACKOFF_MS`] the next respawn takes.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    rung: usize,
}

impl Backoff {
    /// A ladder at the bottom rung.
    ///
    /// `const` because the page holds one in a `thread_local!` and a non-const
    /// initializer there is a lazy check on every access.
    pub const fn new() -> Self {
        Self { rung: 0 }
    }

    /// How long to wait before the next attempt, and step the ladder.
    ///
    /// Saturating: past the last rung every answer is the last rung's, which makes
    /// this a *ceiling* on the retry rate rather than a countdown to giving up.
    pub fn next_delay_ms(&mut self) -> u32 {
        let last = RESPAWN_BACKOFF_MS.len() - 1;
        let delay = RESPAWN_BACKOFF_MS[self.rung.min(last)];
        self.rung = self.rung.saturating_add(1);
        delay
    }

    /// Start again from the bottom, because a worker answered the handshake.
    ///
    /// **The reset belongs to `HELLO` and to nothing weaker.** A `Worker` that
    /// constructs is not a worker that works: resetting on the construction would
    /// make a browser that fails at instantiation retry every second forever.
    pub fn reset(&mut self) {
        self.rung = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ladder_is_walked_once_and_then_held() {
        let mut backoff = Backoff::default();
        assert_eq!(
            [
                backoff.next_delay_ms(),
                backoff.next_delay_ms(),
                backoff.next_delay_ms(),
                backoff.next_delay_ms(),
            ],
            RESPAWN_BACKOFF_MS,
            "the first four attempts are the ladder itself",
        );
        for attempt in 0..64 {
            assert_eq!(
                backoff.next_delay_ms(),
                RESPAWN_BACKOFF_MS[RESPAWN_BACKOFF_MS.len() - 1],
                "attempt {attempt} past the ladder left the top rung",
            );
        }
    }

    /// A handshake that lands puts the next loss back at the bottom of the ladder.
    #[test]
    fn a_handshake_puts_the_next_loss_back_at_the_bottom() {
        let mut backoff = Backoff::default();
        // Far enough up that a reset cannot be confused with the walk itself.
        for _ in 0..9 {
            backoff.next_delay_ms();
        }
        assert_eq!(
            backoff.next_delay_ms(),
            RESPAWN_BACKOFF_MS[RESPAWN_BACKOFF_MS.len() - 1],
            "the fixture must be at the top before the reset means anything",
        );

        backoff.reset();
        assert_eq!(
            backoff.next_delay_ms(),
            RESPAWN_BACKOFF_MS[0],
            "a worker that proved itself must not be re-attempted at the top rung",
        );
        assert_eq!(
            backoff.next_delay_ms(),
            RESPAWN_BACKOFF_MS[1],
            "the reset restarts the ladder rather than pinning it to the bottom",
        );
    }

    /// The `const` constructor and the derived `Default` are the same ladder.
    #[test]
    fn the_const_ladder_and_the_derived_one_are_the_same_ladder() {
        assert_eq!(Backoff::new(), Backoff::default());
    }

    #[test]
    fn every_rung_is_longer_than_the_one_below_it() {
        assert!(
            RESPAWN_BACKOFF_MS.windows(2).all(|pair| pair[0] < pair[1]),
            "a ladder that does not rise is a fixed retry interval wearing its name",
        );
        assert!(
            RESPAWN_BACKOFF_MS[0] > 0,
            "a zero first rung is a respawn loop with no wait in it",
        );
    }
}
