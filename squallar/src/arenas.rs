//! **Cap how many malloc arenas glibc will open**, because on this application
//! it opens one per allocating thread and each one keeps its own free lists.
//!
//! # What this is for
//!
//! A running squallar process carries **130–159 threads** — roughly 60 tokio
//! workers, 38 `rd-job` lanes and 32 async-std — and glibc's allocator hands a
//! thread its own arena on contention rather than serialising it on the main
//! one. That is the right trade for throughput and the wrong one for
//! residency: each arena retains freed spans in its own bins, so memory a
//! thread gave back is not memory any other thread can take, and it is not
//! memory the process gives back to the kernel either.
//!
//! The figure is the gap between what the allocator has handed out and what the
//! process is resident in — `rss_over_live`, the term
//! `squallar_alloc::live_bytes` cannot see by construction, because a free that
//! stays in an arena's bin decrements `live_bytes` and moves no page.
//!
//! # This is glibc only, and the cfg says so
//!
//! `mallopt` is a glibc interface. macOS uses libmalloc with a different
//! design, musl's allocator has no arenas of this kind, Android is bionic, and
//! wasm has neither threads of this sort nor this allocator. On all of those
//! [`cap_malloc_arenas`] compiles to nothing and answers [`ArenaCap::NotGlibc`]
//! — **the figure this module is justified by does not transfer to them**, and
//! a caller reporting it must say which target it measured.
//!
//! # It buys residency with contention, and that is a real price
//!
//! Fewer arenas means more threads sharing each one, which means more lock
//! contention on the allocation path. On a 130-thread process that is not free
//! and is not assumed to be: see the commit that introduced this for the
//! measured allocation-path cost, and treat [`ARENA_MAX`] as a tuning value
//! rather than a constant of nature.

/// **How many arenas glibc may open.**
///
/// The value the residency measurement was taken at. It is deliberately small:
/// the whole point is that a per-thread arena on a 130-thread process is what
/// produces the gap, so a cap that scales with threads would reproduce it.
pub const ARENA_MAX: i32 = 2;

/// What [`cap_malloc_arenas`] did, so a caller can report it rather than
/// assume it.
///
/// Three outcomes and not a `bool`, because "this target has no such knob" and
/// "the knob was there and refused" are different facts and only one of them is
/// a defect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArenaCap {
    /// glibc accepted the cap.
    Applied(i32),
    /// glibc was asked and refused. `mallopt` answers 0 on failure; it has no
    /// error detail to report.
    Refused,
    /// Not a glibc target — nothing was asked and nothing changed.
    NotGlibc,
}

/// Ask glibc to open at most [`ARENA_MAX`] arenas.
///
/// **Call this before any thread is spawned.** `M_ARENA_MAX` bounds how many
/// arenas may exist from the moment it is set; arenas already created stay
/// created, so a call made after the runtimes are up caps a number that has
/// already been reached. `squallar::run` calls it as its first statement, and
/// `main` reaches `run` through `pollster::block_on` — there is no runtime and
/// no worker thread in the process at that point.
///
/// The crate is `deny(unsafe_code)`; this function carries the scoped allow
/// because `mallopt` is a two-`int` C call that returns an `int` and touches
/// nothing of ours — no pointer crosses the boundary in either direction.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[allow(
    unsafe_code,
    reason = "mallopt takes two ints and returns one; no pointer crosses the boundary"
)]
pub fn cap_malloc_arenas() -> ArenaCap {
    // SAFETY: `mallopt` reads two `c_int` by value and returns a `c_int`. It
    // borrows nothing, stores nothing, and cannot observe or alter any Rust
    // value.
    let ok = unsafe { libc::mallopt(libc::M_ARENA_MAX, ARENA_MAX) };
    if ok == 1 {
        ArenaCap::Applied(ARENA_MAX)
    } else {
        ArenaCap::Refused
    }
}

/// See the glibc arm. Compiles to a constant everywhere else.
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub fn cap_malloc_arenas() -> ArenaCap {
    ArenaCap::NotGlibc
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The knob is real on the target that claims it**, and the answer names
    /// which target this build is.
    ///
    /// This is the check that the residency figure in the commit message is
    /// about a call that actually happened: a `mallopt` that silently returned
    /// 0 would leave every arena in place and the module reading as applied.
    #[test]
    fn the_cap_is_applied_on_glibc_and_declines_to_lie_elsewhere() {
        let verdict = cap_malloc_arenas();
        if cfg!(all(target_os = "linux", target_env = "gnu")) {
            assert_eq!(
                verdict,
                ArenaCap::Applied(ARENA_MAX),
                "glibc refused M_ARENA_MAX. The residency this module exists for \
                 is not being bought, and nothing else in the process would say \
                 so — `mallopt` has no error detail and the app would run \
                 normally with every arena still open.",
            );
        } else {
            assert_eq!(
                verdict,
                ArenaCap::NotGlibc,
                "a non-glibc target reported a glibc outcome, so the cfg gate is \
                 wrong and this build is quoting a figure measured elsewhere",
            );
        }
    }

    /// Calling it twice is not an error — glibc takes the latest value — so a
    /// second entry point (a test harness, a re-entered `run`) cannot turn a
    /// working cap into a reported failure.
    #[test]
    fn asking_twice_answers_the_same() {
        assert_eq!(cap_malloc_arenas(), cap_malloc_arenas());
    }
}
