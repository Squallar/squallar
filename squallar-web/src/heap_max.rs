//! **The two linear-memory figures each wasm instance carries**: what its
//! memory was CONSTRUCTED with, and the budget POLICY ceiling its heap is
//! judged against — for this instance, and for its rasterization worker.
//!
//! # Two figures, and why they are never one
//!
//! They used to be one. The module was linked at 1 GiB and the page
//! constructed its memory at a per-device figure — 1024 MiB on a desktop,
//! 512/256 MiB for a handheld's page/worker — that was also the ceiling every
//! budget judged against. The link flag is now wasm32's architectural 4 GiB,
//! and each page and worker constructs the largest memory its engine accepts
//! by walking `squallar-web/heap.js`'s ladder. **That answer is a reservation,
//! not a budget**: an iPhone 13 Pro constructs 4 GiB and iOS kills the tab
//! near 2.3 GiB, so a watermark judging against the reservation would let the
//! phone die of an OS kill before it ever shed. The budgets keep the
//! per-device policy figure, unchanged, until a measured wall model replaces
//! it:
//!
//! * [`this_reservation`] / [`worker_reservation`] — what was constructed.
//!   The allocation-failure line, the worker's `MEMMAX` hello, the `reserved`
//!   field of `budget state:` and the rig's ladder verdict read these, and
//!   nothing sizes anything from them.
//! * [`this_policy`] / [`worker_policy`] — what the budgets judge against:
//!   the host presumption and allowance, `fit`, the watermarks, the admission
//!   doors, and the `heap max` field of `budget state:`.
//!
//! **A policy figure never exceeds its reservation.** A device whose engine
//! constructed less than its policy would otherwise shed against a wall its
//! allocator can never reach, so each policy accessor is the lower of the
//! two. No device that booted before the split reaches that arm — its engine
//! constructed exactly the policy figure — so it changes no budget that ran.
//!
//! The accessors were renamed from `this_instance` / `worker_instance` when
//! the figures split, so a reader written against the old names — which meant
//! both at once — fails to compile rather than reading a reservation as a
//! budget.
//!
//! # Why they cannot be read back
//!
//! `WebAssembly.Memory.prototype.type()` exists in neither Firefox nor
//! Chromium (measured 2026-09-03), and `byteLength` is the memory's CURRENT
//! size. **The values plumbed through here are the only witnesses there
//! are**, which is why `budget state:` prints both: an assertion that the page
//! came up under a given ceiling is an assertion about these cells.
//!
//! # Two instances, two sets of cells
//!
//! The page and the rasterization worker are separate module instances with
//! separate heaps, so each has its own [`this_reservation`]. The page also
//! holds the worker's figures: the policy it chose for the worker, and the
//! reservation the worker reported on its hello (`worker_protocol::MEMMAX`).
//! A worker global has neither `matchMedia` nor `maxTouchPoints`, so the page
//! is the one that can choose.
//!
//! **Two instances, not three or eleven.** The tile lane and rayon's pool
//! threads are further wasm instances, but each is initialised on the
//! rasterization worker's own memory, and these cells are statics living in
//! that memory — so they read what `squallar_worker_main` stored, which is
//! the reservation of the heap they are in. That is the same argument
//! `squallar_tile_lane_main` makes about the panic, log and allocation hooks
//! being process statics in a shared heap, and it is why nothing has to hand
//! the lane a figure of its own.

use std::sync::atomic::{AtomicU64, Ordering};

/// This instance's reservation; 0 until whoever booted this instance said.
static THIS_RESERVED: AtomicU64 = AtomicU64::new(0);

/// This instance's budget policy ceiling; 0 in the worker, which is told
/// none, and before `start` has said.
static THIS_POLICY: AtomicU64 = AtomicU64::new(0);

/// The policy ceiling this page chose for its rasterization worker; 0 in the
/// worker itself.
static WORKER_POLICY: AtomicU64 = AtomicU64::new(0);

/// The reservation the worker reported on its hello; 0 until one has.
static WORKER_RESERVED: AtomicU64 = AtomicU64::new(0);

fn read(cell: &AtomicU64) -> Option<u64> {
    match cell.load(Ordering::Relaxed) {
        0 => None,
        bytes => Some(bytes),
    }
}

/// A policy figure held to the reservation it has to live inside.
fn at_most(policy: Option<u64>, reservation: Option<u64>) -> Option<u64> {
    policy.map(|policy| reservation.map_or(policy, |reserved| policy.min(reserved)))
}

/// What this instance's memory was constructed with, in bytes — the only
/// figure an allocation is refused against. `None` before the entry point
/// has said.
pub fn this_reservation() -> Option<u64> {
    read(&THIS_RESERVED)
}

/// The budget policy ceiling this instance's heap is judged against, in bytes:
/// the page's per-device choice, held to [`this_reservation`]. `None` before
/// `start` has said and in the worker, which is told no policy.
///
/// A caller that reaches this early must treat the absence as "no ceiling to
/// judge against" and not substitute a constant —
/// [`squallar_device_profile::linear_memory::linear_memory_verdict`] already
/// spells a zero ceiling `Quiet`.
pub fn this_policy() -> Option<u64> {
    at_most(read(&THIS_POLICY), this_reservation())
}

/// What the rasterization worker's memory was constructed with, as it
/// reported on its hello; `None` until a worker has said, and in the worker.
pub fn worker_reservation() -> Option<u64> {
    read(&WORKER_RESERVED)
}

/// The rung the next rasterization worker's ladder starts from: the last
/// worker's reported reservation, else this page's own. `worker_port::spawn`
/// names the Worker with it, so a respawned worker starts no higher than its
/// predecessor got.
pub fn worker_ladder_start() -> Option<u64> {
    worker_reservation().or_else(this_reservation)
}

/// The budget policy ceiling the page judges its worker's heap against: the
/// page's per-device choice for the worker, held to the worker's reservation
/// (its ladder's starting rung until it has reported). `None` before `start`
/// has said and in the worker itself.
pub fn worker_policy() -> Option<u64> {
    at_most(read(&WORKER_POLICY), worker_ladder_start())
}

/// Record what the page's memory was constructed with and the two policy
/// ceilings it chose. Called once, from the entry point, with figures JS
/// decided before the module existed. A zero, negative or non-finite figure
/// is dropped rather than stored, so a caller that passes nothing leaves the
/// cell empty rather than pinning a ceiling of zero.
pub fn declare(reserved_bytes: f64, policy_bytes: f64, worker_policy_bytes: f64) {
    store(&THIS_RESERVED, reserved_bytes);
    store(&THIS_POLICY, policy_bytes);
    store(&WORKER_POLICY, worker_policy_bytes);
}

/// Record what this instance's memory was constructed with, for an instance
/// that is told no policy and starts no worker — the rasterization worker.
pub fn declare_this(reserved_bytes: f64) {
    store(&THIS_RESERVED, reserved_bytes);
}

/// Note the reservation the worker reported on its hello.
pub fn note_worker_reported(bytes: u64) {
    if bytes > 0 {
        WORKER_RESERVED.store(bytes, Ordering::Relaxed);
    }
}

fn store(cell: &AtomicU64, bytes: f64) {
    if bytes.is_finite() && bytes > 0.0 {
        cell.store(bytes as u64, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1 << 20;
    const MIB_F: f64 = 1024.0 * 1024.0;

    /// **The two figures stay apart, a policy never exceeds its reservation,
    /// and junk declares nothing.**
    ///
    /// `None` is load-bearing: the one thing a caller must not do with an
    /// absent ceiling is substitute a constant, so a zero or a NaN has to read
    /// as absence rather than as a wall.
    ///
    /// Serialised into one test because the cells are process-global: two
    /// `#[test]` functions writing them would race.
    #[test]
    fn a_reservation_and_a_policy_are_two_figures_and_the_policy_is_held_under_the_reservation() {
        assert_eq!(
            this_reservation(),
            None,
            "a cell nobody wrote read as a wall"
        );
        assert_eq!(this_policy(), None);
        assert_eq!(worker_policy(), None);
        assert_eq!(worker_reservation(), None);
        assert_eq!(worker_ladder_start(), None);

        for junk in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            declare(junk, junk, junk);
            assert_eq!(this_reservation(), None, "{junk} declared a reservation");
            assert_eq!(this_policy(), None, "{junk} declared a policy");
            assert_eq!(worker_policy(), None, "{junk} declared a policy");
        }

        // A desktop page that constructed the top rung: the budgets still see
        // the desktop policy, and the worker's ladder starts at the page's rung.
        declare(4096.0 * MIB_F, 1024.0 * MIB_F, 1024.0 * MIB_F);
        assert_eq!(this_reservation(), Some(4096 * MIB));
        assert_eq!(
            this_policy(),
            Some(1024 * MIB),
            "the reservation leaked into the budget"
        );
        assert_eq!(worker_ladder_start(), Some(4096 * MIB));
        assert_eq!(worker_policy(), Some(1024 * MIB));
        assert_eq!(worker_reservation(), None, "nobody reported a worker yet");

        // The worker reports what its own ladder reached: the policy stands,
        // the next worker starts from the report, and a zero report is junk.
        note_worker_reported(2048 * MIB);
        assert_eq!(worker_reservation(), Some(2048 * MIB));
        assert_eq!(worker_ladder_start(), Some(2048 * MIB));
        assert_eq!(worker_policy(), Some(1024 * MIB));
        note_worker_reported(0);
        assert_eq!(
            worker_reservation(),
            Some(2048 * MIB),
            "a zero report overwrote"
        );

        // A worker that could only reserve less than its policy is judged
        // against what it reserved.
        note_worker_reported(512 * MIB);
        assert_eq!(worker_policy(), Some(512 * MIB));

        // And a page whose engine constructed less than its policy.
        declare(256.0 * MIB_F, 512.0 * MIB_F, 256.0 * MIB_F);
        assert_eq!(this_reservation(), Some(256 * MIB));
        assert_eq!(
            this_policy(),
            Some(256 * MIB),
            "a policy above its reservation"
        );
        assert_eq!(worker_policy(), Some(256 * MIB));
    }
}
