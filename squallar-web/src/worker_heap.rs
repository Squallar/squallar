//! **What the rasterization worker last said about its own heap** — its
//! linear memory's `byteLength` and the live bytes beside it — and **whether
//! the worker that said it is the one this page is still talking to**.
//!
//! # Why these are not in `worker_port`
//!
//! Two readings and a rule about when one stops being current. The rule is
//! the half a bug lives in, `worker_port` is `cfg(target_arch = "wasm32")`,
//! and a wasm-gated rule is checked by `cargo check` and by nothing else.
//! Here it is host-tested, which is the argument [`crate::worker_retry`]
//! makes about the respawn ladder and [`crate::heap_max`] makes about the
//! ceilings these readings are judged against.
//!
//! # Two instances, and the third name is a thread
//!
//! These figures are the rasterization worker's; the page reads its own from
//! `squallar_alloc::live_bytes` and [`crate::shared_loan::memory_bytes`], and
//! the two are reported side by side and **never added** — separate linear
//! memories under separate `--max-memory` bounds, and a sum describes
//! neither. The tile lane is not a third: it runs on the worker's memory and
//! names a THREAD, which is what
//! [`crate::alloc_failure::Instance::observed`] exists to say, so there is no
//! third reading for anything to hold.
//!
//! # Three states, because one reading serves two consumers
//!
//! A worker from a build before `worker_protocol::LIVE` sets no such field,
//! and one whose counting allocator was never installed says nothing either.
//! Both are *unknown*, and 0 would read as a measurement — an instance whose
//! allocator is holding nothing, which no running instance is. So a figure
//! that is not a positive byte count is dropped rather than stored, and the
//! absence is spelled [`Reading::Unread`], the word
//! [`crate::alloc_failure::line`] already prints for an unread heap.
//!
//! **The two states that vocabulary gives are one short**, and the missing
//! one is what this module is for. A figure persists across the worker that
//! gave it: `worker_port::lose` fails that worker's jobs, hands rasterization
//! back to the page's own thread and schedules a respawn, and the last figure
//! stands until a successor speaks. **That rule is right for a readout and
//! wrong for a budget**, which is why it is kept and stamped rather than
//! deleted:
//!
//! * A **readout** wants the last thing anyone said. A telemetry figure that
//!   flapped to `unread` on every worker replacement would be worse telemetry
//!   than one that persists, and a respawn is invisible to the reader.
//! * A **budget term** wants a figure a live instance is standing behind. A
//!   dead worker's last gasp is confidently wrong in whichever direction that
//!   worker was heading when it died — a term taken against it would treat a
//!   last-gasp reading as a live wall forever, or an early one as room that no
//!   longer exists.
//!
//! So [`Reading`] answers both and makes the call site say which it is
//! asking: [`Reading::bytes`] is the last figure whoever gave it, and
//! [`Reading::current`] is a figure only while the worker that reported it is
//! the one this page is talking to.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// One of the worker's two figures, and whether the worker that reported it
/// is still this page's.
struct Reported {
    /// The figure. 0 is "nobody has said": a real heap is never 0 bytes and a
    /// running instance's allocator is never holding nothing.
    bytes: AtomicU64,
    /// Whether [`Self::bytes`] was written by the worker this page is talking
    /// to. Per figure and not per worker, because a build that carries
    /// `worker_protocol::MEM` and not `LIVE` makes one of the two current and
    /// leaves the other where its predecessor left it.
    current: AtomicBool,
}

impl Reported {
    const fn unread() -> Self {
        Self {
            bytes: AtomicU64::new(0),
            current: AtomicBool::new(false),
        }
    }

    /// Store a reading, if it is one. A figure that is not a positive, finite
    /// byte count leaves the cell — value and stamp both — exactly as it was.
    fn note(&self, bytes: Option<f64>) {
        if let Some(bytes) = bytes.filter(|b| b.is_finite() && *b > 0.0) {
            self.bytes.store(bytes as u64, Ordering::Relaxed);
            self.current.store(true, Ordering::Relaxed);
        }
    }

    fn read(&self) -> Reading {
        match self.bytes.load(Ordering::Relaxed) {
            // A stamp without a figure cannot arise — nothing sets the stamp
            // except a store of a positive figure — and reads as unread,
            // which is what having no figure means whatever the stamp says.
            0 => Reading::Unread,
            bytes if self.current.load(Ordering::Relaxed) => Reading::Current(bytes),
            bytes => Reading::Stale(bytes),
        }
    }
}

/// The worker's linear memory as it last reported — `worker_protocol::MEM`.
static MEMORY: Reported = Reported::unread();

/// The worker's live bytes as it last reported — `worker_protocol::LIVE`.
static LIVE: Reported = Reported::unread();

/// One of the worker's figures, and what standing it has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reading {
    /// No worker has ever reported this figure. `unread` on
    /// [`crate::alloc_failure::line`], and the same thing here.
    Unread,
    /// The worker this page is talking to reported it.
    Current(u64),
    /// A worker that is gone reported it, and no successor has spoken. The
    /// state a readout may print and a budget term may not take.
    Stale(u64),
}

impl Reading {
    /// **The last figure, whoever reported it** — what a readout wants, and
    /// what the `budget state:` line has always printed. `None` only for
    /// [`Self::Unread`], so a reader of this alone cannot tell a live worker
    /// from a replaced one; that is [`Self::current`]'s question.
    pub fn bytes(self) -> Option<u64> {
        match self {
            Self::Unread => None,
            Self::Current(bytes) | Self::Stale(bytes) => Some(bytes),
        }
    }

    /// **A figure a live instance is standing behind** — what a budget term
    /// wants. `None` for both [`Self::Unread`] and [`Self::Stale`], which are
    /// different sentences about a worker and the same answer to this
    /// question: nothing here may bound anything.
    pub fn current(self) -> Option<u64> {
        match self {
            Self::Current(bytes) => Some(bytes),
            Self::Unread | Self::Stale(_) => None,
        }
    }

    /// Whether a figure is being held for a worker that is gone.
    pub fn is_stale(self) -> bool {
        matches!(self, Self::Stale(_))
    }
}

/// The rasterization worker's linear memory, in bytes, as it last reported.
pub fn memory() -> Reading {
    MEMORY.read()
}

/// The rasterization worker's live bytes — what its allocator has handed out
/// and not been handed back — as it last reported.
///
/// The figure beside [`memory`] that can FALL: a wasm linear memory never
/// shrinks, so the gap between the two is that heap's freed-but-reserved
/// headroom.
pub fn live() -> Reading {
    LIVE.read()
}

/// Take one reading off a worker message, each figure independently.
///
/// A field the message does not carry arrives here as `None` and leaves that
/// figure alone — value and stamp both. `MEM` and `LIVE` ride the same
/// envelopes, but a build that predates either sets only what it knows, and
/// erasing the other half on its account would lose a figure nothing has
/// contradicted.
///
/// Only ever called for the worker this page is talking to: `worker_port`
/// drops a message from a replaced generation before it reaches here.
pub fn note(memory_bytes: Option<f64>, live_bytes: Option<f64>) {
    MEMORY.note(memory_bytes);
    LIVE.note(live_bytes);
}

/// **The worker that reported these is gone.** Both figures stand and both
/// stop being current, so a readout keeps printing what was last said and a
/// budget term stops taking it.
///
/// Called where the page gives up on a worker, before the respawn is put on
/// its timer.
pub fn lost() {
    MEMORY.current.store(false, Ordering::Relaxed);
    LIVE.current.store(false, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: f64 = 1024.0 * 1024.0;

    /// The whole rule, in the order a session meets it: unread before any
    /// worker speaks, unwritable by anything that is not a byte count, per
    /// figure rather than per message, **held but no longer current once the
    /// worker is lost**, and current again from a successor.
    ///
    /// **One `#[test]` because the cells are process-global**, which is the
    /// reason [`crate::heap_max`]'s own pin is one function too: two test
    /// functions writing these would race and neither would be a pin.
    #[test]
    fn a_figure_outlives_the_worker_that_gave_it_and_stops_being_current_with_it() {
        assert_eq!(
            memory(),
            Reading::Unread,
            "a cell nobody wrote read as a heap"
        );
        assert_eq!(live(), Reading::Unread);

        for junk in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            note(Some(junk), Some(junk));
            assert_eq!(memory(), Reading::Unread, "{junk} declared a heap");
            assert_eq!(live(), Reading::Unread, "{junk} declared live bytes");
        }
        note(None, None);
        assert_eq!(
            memory(),
            Reading::Unread,
            "an empty message declared a heap"
        );
        assert_eq!(live(), Reading::Unread);
        assert_eq!(memory().bytes(), None, "unread is the one absent state");
        assert_eq!(memory().current(), None);

        note(Some(700.0 * MIB), Some(406.0 * MIB));
        assert_eq!(memory(), Reading::Current(700 << 20));
        assert_eq!(live(), Reading::Current(406 << 20), "two separate figures");
        assert_eq!(live().bytes(), Some(406 << 20));
        assert_eq!(live().current(), Some(406 << 20));

        // A message carrying one field and not the other moves one figure. A
        // heap size that stopped being reported is not a heap that shrank.
        note(None, Some(212.0 * MIB));
        assert_eq!(
            memory(),
            Reading::Current(700 << 20),
            "an absent MEM moved it"
        );
        assert_eq!(live(), Reading::Current(212 << 20));

        // And a junk figure beside a good one drops only itself.
        note(Some(f64::NAN), Some(512.0 * MIB));
        assert_eq!(memory(), Reading::Current(700 << 20));
        assert_eq!(live(), Reading::Current(512 << 20));

        // **The worker dies.** Both figures stand — the readout's rule — and
        // neither is current any more, which is the whole of what a budget
        // term needs and the state the two-word vocabulary could not say.
        lost();
        assert_eq!(memory(), Reading::Stale(700 << 20));
        assert_eq!(live(), Reading::Stale(512 << 20));
        assert!(memory().is_stale() && live().is_stale());
        assert_eq!(
            live().bytes(),
            Some(512 << 20),
            "a readout stopped printing what was last said"
        );
        assert_eq!(
            live().current(),
            None,
            "a dead worker's last gasp is still bounding something"
        );
        assert_ne!(
            live(),
            Reading::Unread,
            "a worker that spoke and died reads as one that never spoke"
        );

        // A successor makes its own figures current; one it does not report
        // stays its predecessor's, and stays stale.
        note(None, Some(9.0 * MIB));
        assert_eq!(
            memory(),
            Reading::Stale(700 << 20),
            "a silent field went live"
        );
        assert_eq!(live(), Reading::Current(9 << 20));
        note(Some(64.0 * MIB), None);
        assert_eq!(memory(), Reading::Current(64 << 20));

        lost();
    }
}
