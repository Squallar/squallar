//! How long the page waits before starting another rasterization worker.
//!
//! The one piece of [`crate::worker_port`]'s respawn that is not a browser
//! type, and it is here rather than there for exactly that reason:
//! `worker_port` is `#[cfg(target_arch = "wasm32")]`, so nothing in it runs
//! under `cargo test` on a host, and a ladder that only a browser can walk is a
//! ladder nobody checks. This module compiles everywhere and is driven by the
//! tests at the bottom.

/// What each successive respawn waits, in milliseconds.
///
/// # The shape, and why the last rung repeats
///
/// A worker is lost for two kinds of reason and the ladder has to serve both.
/// One is transient — a page that lost its worker to memory pressure, a
/// service-worker update caught mid-flight, a `FATAL` from an instantiation
/// that raced the module's own fetch — and a second attempt a second later
/// simply works. The other is not: a browser that cannot start this worker at
/// all will not start it on the hundredth try either, and a page that keeps
/// asking spends the user's battery to learn nothing.
///
/// So the first rung is short enough to be invisible and the last is long
/// enough to be free, and the last **repeats forever** rather than giving up.
/// Giving up is the state this whole file exists to remove: it is what the page
/// did before, and it converted one worker error into every later scan, scrub
/// and loop frame running on the browser's one thread. A retry a minute costs a
/// `Worker` construction and a handshake that fails; never retrying costs a
/// second of frame time per volume, for the rest of the session.
///
/// The numbers are a policy, not a measurement — nothing here has a browser to
/// measure — and they are chosen against those two costs rather than against a
/// figure. What is measured is what the fallback costs when the ladder is at
/// its top: 1021.9 ms in Firefox and 911.4 ms in Chrome for a 16.9 MB volume
/// decode, quoted from `rustdar_frontend::offload::JobRequest::Decode`, which
/// is where the audit recorded it.
pub const RESPAWN_BACKOFF_MS: [u32; 4] = [1_000, 4_000, 16_000, 60_000];

/// Which rung of [`RESPAWN_BACKOFF_MS`] the next respawn takes.
///
/// A type rather than a bare counter so that "advance" and "reset" are the only
/// two things that can happen to it, and so the saturation at the top rung is
/// written once instead of at each caller.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    rung: usize,
}

impl Backoff {
    /// A ladder at the bottom rung.
    ///
    /// `const` because the page holds one in a `thread_local!` and a
    /// non-const initializer there is a lazy check on every access.
    /// [`Default`] is derived and agrees with it by construction — the struct
    /// is one `usize` and `Default` zeroes it — which is why the two can
    /// coexist without a second statement of the same state.
    pub const fn new() -> Self {
        Self { rung: 0 }
    }

    /// How long to wait before the next attempt, and step the ladder.
    ///
    /// Saturating: past the last rung every answer is the last rung's, which is
    /// what makes this a *ceiling* on the retry rate rather than a countdown to
    /// giving up. See [`RESPAWN_BACKOFF_MS`].
    pub fn next_delay_ms(&mut self) -> u32 {
        let last = RESPAWN_BACKOFF_MS.len() - 1;
        let delay = RESPAWN_BACKOFF_MS[self.rung.min(last)];
        self.rung = self.rung.saturating_add(1);
        delay
    }

    /// Start again from the bottom, because a worker answered the handshake.
    ///
    /// **The reset belongs to `HELLO` and to nothing weaker.** A `Worker` that
    /// constructs is not a worker that works: the module has still to fetch,
    /// instantiate and prove it is this same build. Resetting on the
    /// construction would make a browser that fails at instantiation retry
    /// every second forever, which is the runaway this ladder exists to bound.
    pub fn reset(&mut self) {
        self.rung = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ladder is walked once and then held, rather than continuing off the
    /// end of the table or wrapping back to the bottom.
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

    /// A handshake that lands puts the next loss back at the bottom of the
    /// ladder — so a worker that dies once an hour is recovered in a second
    /// each time, rather than in a minute because of an hour-old failure.
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

    /// The ladder rises, which is the whole of what makes it a backoff. Written
    /// as a property of the table rather than of the four numbers, so a rung
    /// edited to sit below its predecessor fails here.
    /// The `const` constructor and the derived `Default` are the same ladder.
    /// They are two statements of one state — the page needs the first for a
    /// `thread_local!` and the tests reach for the second — and a divergence
    /// would mean a browser retrying on a rung no test ever walks.
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
