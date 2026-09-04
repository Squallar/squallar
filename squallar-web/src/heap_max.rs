//! **The maximum this module instance's linear memory was constructed with**,
//! and the one its rasterization worker was constructed with.
//!
//! # Why a cell and not a constant
//!
//! It used to be `constants::WASM_LINEAR_MEMORY_MAX_BYTES`, whose doc said "no
//! browser and no device moves it". That is no longer true and was never the
//! only thing it could be: the link flag declares a bound, and a supplied
//! `WebAssembly.Memory` may sit anywhere at or below it (`squallar-web/heap.js`
//! carries the measurement and the rule). The page chooses per device before
//! `init()`, so by the time any Rust runs the figure is already fixed and is
//! simply told to us.
//!
//! # Why it cannot be read back
//!
//! `WebAssembly.Memory.prototype.type()` exists in neither Firefox nor
//! Chromium (measured 2026-09-03), and `byteLength` is the memory's CURRENT
//! size. **The value plumbed through here is the only witness there is**,
//! which is why it is also what the `budget state:` line prints: an assertion
//! that the page came up at a chosen ceiling is an assertion about this cell.
//!
//! # Two instances, two cells
//!
//! The page and the rasterization worker are separate module instances with
//! separate heaps, so each has its own [`this_instance`]. The page also holds
//! [`worker_instance`] — the figure it *chose for* the worker and hands over
//! on the worker's `name` — because the page constructs the worker, and the
//! worker's own `WorkerNavigator` carries neither `matchMedia` nor
//! `maxTouchPoints` and so could not classify the device for itself.
//!
//! **Two instances, not three or eleven.** The tile lane and rayon's pool
//! threads are further wasm instances, but each is initialised on the
//! rasterization worker's own memory, and these cells are statics living in
//! that memory — so they read what `squallar_worker_main` stored, which is
//! the ceiling of the heap they are in. That is the same argument
//! `squallar_tile_lane_main` makes about the panic, log and allocation hooks
//! being process statics in a shared heap, and it is why nothing has to hand
//! the lane a ceiling of its own.

use std::sync::atomic::{AtomicU64, Ordering};

/// This instance's own ceiling; 0 until whoever booted this instance said.
static THIS_INSTANCE: AtomicU64 = AtomicU64::new(0);

/// The ceiling this page hands its rasterization worker; 0 in the worker,
/// which starts none.
static WORKER_INSTANCE: AtomicU64 = AtomicU64::new(0);

/// What this instance's heap may grow to, in bytes.
///
/// `None` before the entry point has said, which on a shipped build is only
/// the window between instantiation and the first line of `start` /
/// `squallar_worker_main`. A caller that reaches this early must treat the
/// absence as "no ceiling to judge against" and not substitute a constant —
/// [`squallar_device_profile::linear_memory::linear_memory_verdict`] already
/// spells a zero ceiling `Quiet`.
pub fn this_instance() -> Option<u64> {
    match THIS_INSTANCE.load(Ordering::Relaxed) {
        0 => None,
        bytes => Some(bytes),
    }
}

/// What this page chose for its rasterization worker, in bytes; `None` in the
/// worker itself and before `start` has said.
///
/// This is the figure the worker is *asked* for. What it actually got is what
/// it reports back on its hello (`worker_protocol::MEMMAX`), and those differ
/// exactly when the worker's own memory was refused and `heap.js` fell back to
/// the module's declared bound. The page prefers the report.
pub fn worker_instance() -> Option<u64> {
    match WORKER_INSTANCE.load(Ordering::Relaxed) {
        0 => None,
        bytes => Some(bytes),
    }
}

/// Record what this instance and its worker were built with. Called once, from
/// the entry point, with figures JS decided before the module existed. A zero
/// or negative figure is dropped rather than stored, so a caller that passes
/// nothing leaves the cell empty rather than pinning a ceiling of zero.
pub fn declare(this_bytes: f64, worker_bytes: f64) {
    store(&THIS_INSTANCE, this_bytes);
    store(&WORKER_INSTANCE, worker_bytes);
}

/// Record what this instance was built with, for an instance that starts no
/// worker of its own — the rasterization worker.
pub fn declare_this(this_bytes: f64) {
    store(&THIS_INSTANCE, this_bytes);
}

/// Note what the worker reported it actually came up with, which outranks what
/// it was asked for.
pub fn note_worker_reported(bytes: u64) {
    if bytes > 0 {
        WORKER_INSTANCE.store(bytes, Ordering::Relaxed);
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

    /// The cells start empty and a figure that is not a byte count leaves
    /// them empty.
    ///
    /// **`None` is load-bearing.** The one thing a caller must not do with an
    /// absent ceiling is substitute
    /// `constants::WASM_LINEAR_MEMORY_MAX_BYTES`: on a handheld that is twice
    /// the truth for the page and four times it for the worker, and a
    /// watermark judging against it would sit `Quiet` right up to the wall.
    /// So the absence has to be representable, and a zero or a NaN has to
    /// read as absence rather than as a wall.
    ///
    /// Serialised into one test because the cells are process-global: two
    /// `#[test]` functions writing them would race.
    #[test]
    fn a_ceiling_is_absent_until_it_is_declared_and_junk_does_not_declare_one() {
        assert_eq!(this_instance(), None, "a cell nobody wrote read as a wall");
        assert_eq!(worker_instance(), None);

        for junk in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            declare(junk, junk);
            assert_eq!(this_instance(), None, "{junk} declared a ceiling");
            assert_eq!(worker_instance(), None, "{junk} declared a ceiling");
        }

        declare(512.0 * 1024.0 * 1024.0, 256.0 * 1024.0 * 1024.0);
        assert_eq!(this_instance(), Some(512 << 20));
        assert_eq!(worker_instance(), Some(256 << 20), "the two are separate");

        // What the worker REPORTS outranks what the page asked for: they
        // differ exactly when the engine refused the supplied memory and the
        // glue built one at the module's declared bound instead.
        note_worker_reported(1 << 30);
        assert_eq!(worker_instance(), Some(1 << 30));
        assert_eq!(this_instance(), Some(512 << 20), "the page's cell moved");
        note_worker_reported(0);
        assert_eq!(worker_instance(), Some(1 << 30), "a zero report overwrote");
    }
}
