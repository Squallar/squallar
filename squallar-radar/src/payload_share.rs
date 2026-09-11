//! **The render-input round trip copied every gate byte twice, and this is
//! what not copying them is worth.**
//!
//! # The two copies
//!
//! [`crate::render_input::MomentPayload`] is a decoded moment taken apart into
//! its scalars plus its raw gate bytes, so that
//! `nexrad_model::data::MomentData::from_fixed_point` can rebuild it exactly
//! across a worker boundary. Both directions used to move the bytes:
//!
//! * `MomentPayload::from_moment_data` called `raw_values().to_vec()` — a
//!   fresh allocation and a memcpy of the whole gate array, per (radial,
//!   moment), **out of a buffer that was already an `Arc`**.
//! * `to_moment_data` and `to_cfp_moment_data` called `self.gates.clone()` —
//!   the same array again, on the way back.
//!
//! `nexrad_model::data::GateBuffer` has been an `Arc<Vec<u8>>` since the sweep
//! clone was made a refcount bump, and it publishes both the buffer
//! (`DataMoment::gate_buffer`) and a constructor that adopts one
//! (`MomentData::from_gate_buffer`). This path was simply not using either: it
//! went through `&[u8]` and `Vec<u8>`, which cannot express sharing, so the
//! copies were a property of the *signatures* and not of the work.
//!
//! # Why sharing cannot cost retention
//!
//! The payload now holds a reference to the volume's buffer rather than a
//! private copy of it, which is a longer lifetime for *that allocation* and
//! **never more bytes**. While both are alive it is one buffer where it was
//! two; once the volume drops it is one buffer where it was one — the payload's
//! copy used to be what survived, and now the original is. So there is no
//! state where this retains more than the copy did, and
//! [`crate::skeleton`]'s release is unaffected in either direction: what it
//! frees was always bounded by the payload's own lifetime.
//!
//! The buffer is immutable by construction — `GateBuffer`'s own note states
//! it, there is no `&mut` path to the bytes in or out of the model — so
//! sharing cannot change what a reader sees.
//!
//! # The counters
//!
//! Always on, running totals rather than levels, and **two pairs rather than
//! one** because the two directions are different populations: a payload is
//! built once per (radial, moment) reachable in a request and rebuilt only on
//! the arms that put a volume back together. A single figure would silently
//! mix them.
//!
//! [`copied`] is published for the reason [`crate::moment_drop::redecodes`]
//! is: it is **0 by construction** — every `impl DataMoment` can hand over its
//! buffer, so there is no arm of `from_moment_data` that must fall back to a
//! copy — and a non-zero reading would mean this module's premise is false.
//! A mechanism that executes zero times reads exactly like one that works,
//! which is why the totals exist at all and why
//! `squallar_app::budget_telemetry` prints them on every loud tick.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// Payloads built by adopting the model's gate buffer.
static ADOPTED: AtomicU64 = AtomicU64::new(0);

/// Gate bytes those payloads did not copy, one
/// [`crate::scan_size::ALLOCATOR_BLOCK_OVERHEAD`] apiece included, so the
/// figure is comparable with `scan_bytes` without a caveat.
static ADOPTED_BYTES: AtomicU64 = AtomicU64::new(0);

/// Moments rebuilt from a payload by handing the buffer back.
static RETURNED: AtomicU64 = AtomicU64::new(0);

/// Gate bytes those rebuilds did not copy, on the same convention.
static RETURNED_BYTES: AtomicU64 = AtomicU64::new(0);

/// Payloads that had to copy their gates after all.
///
/// **Zero by construction** and published anyway: see the module note.
static COPIED: AtomicU64 = AtomicU64::new(0);

/// One payload built without copying `encoded` bytes of gate data.
///
/// A zero-length buffer is charged nothing at all, blocks included — the
/// convention [`crate::scan_size::gate_bytes`] and
/// [`crate::moment_drop::dropped_cfp`] both use, because a `Vec` of zero
/// length never asked the allocator for anything. A moment stripped by
/// [`crate::skeleton`] reaches here with `encoded == 0` and is likewise not
/// counted: there was nothing to copy.
///
/// **One block apiece, not two.** The avoided allocation is the `Vec<u8>` the
/// copy would have lived in and nothing else: the `Arc` block
/// ([`crate::scan_size::GATE_BUFFER_SHARE_BYTES`]) is the one that already
/// exists and is being shared, so it is not a saving and is not charged here.
/// The payload's own inline field also shrinks from a 24-byte `Vec` header to
/// an 8-byte `Arc` pointer, which is real but lives inside its parent
/// `Vec<RadialData>` rather than in a block of its own, so it is not in this
/// figure either.
pub fn adopted(encoded: usize) {
    if encoded == 0 {
        return;
    }
    ADOPTED.fetch_add(1, Relaxed);
    ADOPTED_BYTES.fetch_add(
        (encoded + crate::scan_size::ALLOCATOR_BLOCK_OVERHEAD) as u64,
        Relaxed,
    );
}

/// One moment rebuilt without copying `encoded` bytes back out of the payload.
/// Same zero-length and single-block conventions as [`adopted`].
pub fn returned(encoded: usize) {
    if encoded == 0 {
        return;
    }
    RETURNED.fetch_add(1, Relaxed);
    RETURNED_BYTES.fetch_add(
        (encoded + crate::scan_size::ALLOCATOR_BLOCK_OVERHEAD) as u64,
        Relaxed,
    );
}

/// Record a payload that copied its gates. Nothing calls this; see [`COPIED`].
pub fn copied_gates() {
    COPIED.fetch_add(1, Relaxed);
}

/// Payloads built by adopting a buffer, since the process started.
#[must_use]
pub fn adopted_count() -> u64 {
    ADOPTED.load(Relaxed)
}

/// Gate bytes not copied on the way in, block overhead included.
#[must_use]
pub fn adopted_bytes() -> u64 {
    ADOPTED_BYTES.load(Relaxed)
}

/// Moments rebuilt by handing a buffer back, since the process started.
#[must_use]
pub fn returned_count() -> u64 {
    RETURNED.load(Relaxed)
}

/// Gate bytes not copied on the way out, block overhead included.
#[must_use]
pub fn returned_bytes() -> u64 {
    RETURNED_BYTES.load(Relaxed)
}

/// Payloads that copied their gates. See [`COPIED`].
#[must_use]
pub fn copied() -> u64 {
    COPIED.load(Relaxed)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "payload_share/tests.rs"]
mod tests;
