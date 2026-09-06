//! **Reserve before you allocate, where the size is knowable.**
//!
//! A budget model that prices what a scene *has* is always one arrival behind
//! the allocation that breaks it. The gap this closes is between the moment a
//! size becomes knowable and the moment the bytes are actually taken: a GRIB2
//! section 3 gives `ni × nj` before the values vector exists, a netCDF header
//! gives `shape("data")` before a chunk is inflated, and an overlay texture's
//! `width × height` is fixed on the frame thread before the job is dispatched.
//! In each case the allocation can be *declared* first, so the scene's need
//! includes it while it is still refusable.
//!
//! # What a reserve is, and what it is not
//!
//! A reserve is an announcement, not an allocation: nothing here allocates,
//! frees, or holds a buffer. It is the counterpart to
//! `Vec::try_reserve_exact` — that call asks the allocator whether the bytes
//! exist, this one tells the application that they are about to be taken.
//! Both matter, and neither substitutes for the other.
//!
//! # Passes, and why a reserve must be scoped to one
//!
//! Every reserve is taken inside a **pass** and released when that pass ends.
//! The model is `LoopFrameStore::begin_pass`/`end_pass` in `squallar-app`:
//! holders are forgotten at the head of a pass, re-stated during it, and
//! whatever nobody re-stated is dropped at the tail. The same discipline is
//! what keeps this ledger from being a leak detector's problem — an
//! unbalanced reserve is a number that only grows, and a number that only
//! grows reads as a scene that keeps getting bigger, which is the exact
//! symptom the budget model exists to notice. [`Reservations::end_pass`]
//! therefore returns the ledger to the level the pass opened at *whatever
//! happened inside it*, and says how much it had to release to do so.
//!
//! # The over-arrival counter
//!
//! One of the sites this serves cannot know its size in advance at all: a
//! decoded radar volume has no `Content-Length` on the wire and S3's `Size`
//! is in a listing document nothing parses. That site reserves a
//! **self-calibrating** figure — a measured bootstrap, raised to the largest
//! volume the session has actually seen from that site — and reconciles down
//! to the truth the moment the decode finishes.
//!
//! A reserve that is too small is not an error, but it *is* evidence, and the
//! whole reason [`Reservations::over_arrivals`] exists is that the last
//! reserve which was too small was discovered by a third corpus rather than
//! by the field: the figure it replaced had been called a maximum and was in
//! fact a 70th percentile. A counter that the shipped build increments on
//! every arrival larger than its reserve is what makes the next such figure
//! report itself.

use std::sync::atomic::{AtomicU64, Ordering};

/// **A pass-scoped ledger of bytes declared but not yet allocated.**
///
/// Cheap enough to be always on: four relaxed atomic adds per reserve, and no
/// lock. `Relaxed` throughout on purpose — every figure here is a *count* read
/// for reporting and for a budget comparison, never a flag another thread's
/// memory safety depends on, so nothing needs to be ordered against anything
/// else. The one consequence is that a reader between two writers may see a
/// total that no single instant held; a budget that would be decided
/// differently by one arrival's worth of bytes is a budget already inside its
/// own noise.
#[derive(Debug, Default)]
pub struct Reservations {
    /// Bytes reserved in the pass now open and not yet settled.
    pending: AtomicU64,
    /// The largest [`Self::pending`] any pass has reached, for a readout that
    /// wants the high-water mark rather than the instant.
    peak: AtomicU64,
    /// Arrivals whose true size exceeded the reserve taken for them.
    over_arrivals: AtomicU64,
    /// By how much, summed — the figure that says whether the reserve is a
    /// little low or the wrong shape entirely.
    over_arrival_bytes: AtomicU64,
    /// Bytes [`Self::end_pass`] has had to release because nothing settled
    /// them. Zero is the healthy reading; a rising figure means a site is
    /// reserving and never arriving.
    released_unsettled: AtomicU64,
}

impl Reservations {
    /// An empty ledger.
    pub const fn new() -> Self {
        Self {
            pending: AtomicU64::new(0),
            peak: AtomicU64::new(0),
            over_arrivals: AtomicU64::new(0),
            over_arrival_bytes: AtomicU64::new(0),
            released_unsettled: AtomicU64::new(0),
        }
    }

    /// **Open a pass**, discarding anything the previous one left outstanding.
    ///
    /// Idempotent and total: it cannot fail and does not care whether the last
    /// pass was closed, because a pass boundary is the one moment at which
    /// "what is outstanding" is knowable without asking any of the sites. What
    /// the previous pass leaked is added to [`Self::released_unsettled`] on
    /// its way out rather than silently dropped — a leak that leaves no trace
    /// is the failure this whole type is trying not to be.
    pub fn begin_pass(&self) {
        let left = self.pending.swap(0, Ordering::Relaxed);
        if left > 0 {
            self.released_unsettled.fetch_add(left, Ordering::Relaxed);
        }
    }

    /// **Close a pass**, returning the bytes it had to release.
    ///
    /// Zero is the healthy reading and means every reserve the pass took was
    /// settled inside it. Anything else is a site that declared bytes and
    /// never said what it actually took.
    pub fn end_pass(&self) -> u64 {
        let left = self.pending.swap(0, Ordering::Relaxed);
        if left > 0 {
            self.released_unsettled.fetch_add(left, Ordering::Relaxed);
        }
        left
    }

    /// **Declare `bytes` about to be allocated.** Returns the figure reserved,
    /// so a caller can hand the same number to [`Self::settle`] without
    /// keeping its own copy of the arithmetic.
    pub fn reserve(&self, bytes: u64) -> u64 {
        let now = self
            .pending
            .fetch_add(bytes, Ordering::Relaxed)
            .saturating_add(bytes);
        self.peak.fetch_max(now, Ordering::Relaxed);
        bytes
    }

    /// **Reconcile a reserve against the truth**, once the allocation has
    /// happened and its size is known.
    ///
    /// The reserve leaves the ledger either way — it has served its purpose
    /// the moment the bytes are real, and from then on the thing that prices
    /// them is whatever counts resident memory. What is recorded is the
    /// *direction of the error*: an arrival at or under its reserve is the
    /// ordinary case and is counted nowhere, and an arrival over it increments
    /// [`Self::over_arrivals`] by one and [`Self::over_arrival_bytes`] by the
    /// shortfall.
    pub fn settle(&self, reserved: u64, actual: u64) {
        // `fetch_min`-style saturation: a settle for a reserve this pass never
        // took (a reply that outlived its pass) must not drive the level
        // negative and wrap.
        let mut current = self.pending.load(Ordering::Relaxed);
        loop {
            let next = current.saturating_sub(reserved);
            match self.pending.compare_exchange_weak(
                current,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(seen) => current = seen,
            }
        }
        if let Some(over) = actual.checked_sub(reserved).filter(|over| *over > 0) {
            self.over_arrivals.fetch_add(1, Ordering::Relaxed);
            self.over_arrival_bytes.fetch_add(over, Ordering::Relaxed);
        }
    }

    /// **Declare `bytes`, guarded**: the reserve releases itself if the
    /// caller leaves by `?` before settling. The spelling every decoder
    /// should use; [`Self::reserve`] is for a caller that settles from a
    /// different stack frame than the one that reserved.
    pub fn take(&self, bytes: u64) -> Reservation<'_> {
        self.reserve(bytes);
        Reservation {
            ledger: self,
            bytes,
            settled: false,
        }
    }

    /// Bytes declared in the open pass and not yet settled.
    pub fn outstanding_bytes(&self) -> u64 {
        self.pending.load(Ordering::Relaxed)
    }

    /// The largest [`Self::outstanding_bytes`] any pass has reached.
    pub fn peak_bytes(&self) -> u64 {
        self.peak.load(Ordering::Relaxed)
    }

    /// **Arrivals larger than the reserve taken for them.** The field's own
    /// report that a reserve is wrong, rather than a later corpus revealing
    /// it.
    pub fn over_arrivals(&self) -> u64 {
        self.over_arrivals.load(Ordering::Relaxed)
    }

    /// By how much [`Self::over_arrivals`] overshot, summed.
    pub fn over_arrival_bytes(&self) -> u64 {
        self.over_arrival_bytes.load(Ordering::Relaxed)
    }

    /// Bytes a pass boundary released because nothing settled them.
    pub fn released_unsettled_bytes(&self) -> u64 {
        self.released_unsettled.load(Ordering::Relaxed)
    }

    /// **One line for a census row**, whether or not anything gates on it.
    pub fn line(&self) -> String {
        format!(
            "reserved {} KiB (peak {} KiB), {} over-arrivals totalling {} KiB, \
             {} KiB released unsettled",
            self.outstanding_bytes() / 1024,
            self.peak_bytes() / 1024,
            self.over_arrivals(),
            self.over_arrival_bytes() / 1024,
            self.released_unsettled_bytes() / 1024,
        )
    }
}

/// **A reserve that releases itself if nothing settles it.**
///
/// The leak-proof spelling, and the only one a decoder should use. Between
/// the header that makes a size knowable and the values vector that spends it
/// there is a run of fallible work — a PNG plan that will not parse, a
/// submessage that will not decode, an allocator that says no — and every one
/// of those paths leaves by `?`. A bare [`Reservations::reserve`] there would
/// declare bytes that no settle ever retires, and the pass boundary would
/// tidy it away into `released_unsettled` where it reads as a leak rather
/// than as the refusal it actually was.
///
/// Dropping unsettled releases the reserve and records **no** over-arrival:
/// a decode that never happened did not overshoot anything.
#[derive(Debug)]
pub struct Reservation<'a> {
    ledger: &'a Reservations,
    bytes: u64,
    settled: bool,
}

impl Reservation<'_> {
    /// The bytes this declared.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// **Reconcile against the truth.** Consumes the guard, so a reserve
    /// cannot be settled twice and the type system carries the discipline
    /// instead of a reviewer.
    pub fn settle(mut self, actual: u64) {
        self.settled = true;
        self.ledger.settle(self.bytes, actual);
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        if !self.settled {
            // Released, and counted as no overshoot: `actual` of zero cannot
            // exceed any reserve.
            self.ledger.settle(self.bytes, 0);
        }
    }
}

/// **The process-wide ledger.**
///
/// One, because a reserve is a claim on the machine's memory and the machine
/// is one. The alternative — a ledger per crate — would answer "how much has
/// this decoder declared", which is a question nobody asks; what a budget
/// needs to know is what the whole application is about to take.
pub fn global() -> &'static Reservations {
    static LEDGER: Reservations = Reservations::new();
    &LEDGER
}

#[path = "reserve/tests.rs"]
#[cfg(test)]
mod tests;
