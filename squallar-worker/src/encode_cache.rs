//! **The encoded row bytes of a job input, kept across dispatches.**
//!
//! [`JobRequest::to_parts`](crate::offload::JobRequest::to_parts) runs at the
//! dispatch site, on the frame thread, and writes the row's whole message every
//! time it is called. For most rows that is a few dozen bytes of scalars. For
//! the polygon layers it is the geometry itself, written two `f64`s at a time —
//! 2.08 MB of it on the live `/alerts/active` feed.
//!
//! A pan is what makes that repeat. `prepare_job` already memoises the built
//! input behind a stable `Arc`, so a gesture that moves only the viewport hands
//! the same input back on every dispatch; but the *envelope* carries the
//! viewport, so the message differs and the row was encoded again for it.
//! Measured over the scripted `pan-zoom-2d` script, two loops at 175 Hz: the
//! alerts row encoded 27 times from **one** input, and 26 of those wrote bytes
//! identical to the first.
//!
//! **What this removes is the encoder, not the copy.** A hit still appends the
//! remembered bytes to the caller's buffer; what it skips is the walk that
//! produced them. Lending the bytes instead of appending them would remove the
//! copy too, but that is a change to the wire shape of seven rows and is not
//! what this is.
//!
//! **Two things make reuse safe.** A row must have opted in through
//! [`JobSpec::ENCODE_IGNORES_CTX`], which is `false` unless a row says
//! otherwise — so a row that reads the context is never served an encode taken
//! under a different one. And an entry **holds its input**, rather than
//! remembering where it was: an `Arc` whose last owner drops frees its
//! allocation, and the allocator may hand that same address to the next input,
//! so a bare pointer compares equal across two different values. That is not a
//! hypothetical — a census that kept only the address read 10 false matches in
//! 32 dispatches of the storm-reports row, and 0 once it held the inputs.
//!
//! [`JobSpec::ENCODE_IGNORES_CTX`]: squallar_source::job::JobSpec::ENCODE_IGNORES_CTX

use std::cell::{Cell, RefCell};

use squallar_source::job::{DescribedJob, EncodeCtx, JobCodec};

/// The most this thread holds in remembered row bytes.
///
/// Sized to fit two of the largest real head (2.08 MB) so a two-pane split
/// looking at two different filter sets keeps both rather than evicting one
/// for the other on every dispatch. Past that the oldest goes: a third such
/// pane encodes as it did before this existed, which is the honest worst case
/// and is bounded rather than growing.
pub const CAP_BYTES: usize = 4 * 1024 * 1024;

/// The smallest encode worth remembering. Below this the walk being skipped is
/// a few dozen scalar writes, and an entry would only take room from one of
/// the megabyte rows this exists for.
pub const FLOOR_BYTES: usize = 64 * 1024;

struct Entry {
    code: u8,
    /// **Held, not pointed at** — see the module doc: this is what makes the
    /// address below unique for as long as the entry can be matched against.
    job: DescribedJob,
    bytes: Vec<u8>,
}

thread_local! {
    /// Most recently used first, so eviction takes from the back.
    static ROWS: RefCell<Vec<Entry>> = const { RefCell::new(Vec::new()) };
    static ENCODED: Cell<u64> = const { Cell::new(0) };
    static ENCODED_BYTES: Cell<u64> = const { Cell::new(0) };
    static REUSED: Cell<u64> = const { Cell::new(0) };
    static REUSED_BYTES: Cell<u64> = const { Cell::new(0) };
    static HELD_BYTES: Cell<usize> = const { Cell::new(0) };
}

/// What this thread's encoding has cost and saved. Always maintained, whether
/// or not anything gates on it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EncodeTotals {
    /// Row encodes that ran the encoder.
    pub encoded: u64,
    /// What those wrote.
    pub encoded_bytes: u64,
    /// Row encodes served from a remembered encode instead.
    pub reused: u64,
    /// What those would have written.
    pub reused_bytes: u64,
    /// What the table is holding right now.
    pub held_bytes: usize,
}

pub fn totals() -> EncodeTotals {
    EncodeTotals {
        encoded: ENCODED.with(Cell::get),
        encoded_bytes: ENCODED_BYTES.with(Cell::get),
        reused: REUSED.with(Cell::get),
        reused_bytes: REUSED_BYTES.with(Cell::get),
        held_bytes: HELD_BYTES.with(Cell::get),
    }
}

/// Drop everything this thread is holding. For tests and for a caller that
/// wants the encoder's own cost back on a cold reading.
pub fn clear() {
    ROWS.with(|rows| rows.borrow_mut().clear());
    HELD_BYTES.with(|h| h.set(0));
}

/// Append `job`'s row bytes to `out`, running `row`'s encoder only if this
/// thread does not already hold them for **this** input.
pub(crate) fn encode_row(
    row: &'static JobCodec,
    code: u8,
    job: &DescribedJob,
    ctx: &EncodeCtx,
    out: &mut Vec<u8>,
) {
    if !row.encode_ignores_ctx {
        let at = out.len();
        (row.encode)(job, ctx, out);
        note_encoded(out.len() - at);
        return;
    }
    if let Some(len) = reuse(code, job, out) {
        REUSED.with(|c| c.set(c.get() + 1));
        REUSED_BYTES.with(|c| c.set(c.get() + len as u64));
        return;
    }
    let at = out.len();
    (row.encode)(job, ctx, out);
    note_encoded(out.len() - at);
    remember(code, job, &out[at..]);
}

fn note_encoded(len: usize) {
    ENCODED.with(|c| c.set(c.get() + 1));
    ENCODED_BYTES.with(|c| c.set(c.get() + len as u64));
}

/// Append the remembered bytes for `(code, job)` if there are any, promote the
/// entry to the front, and answer how many bytes it wrote.
fn reuse(code: u8, job: &DescribedJob, out: &mut Vec<u8>) -> Option<usize> {
    ROWS.with(|rows| {
        let mut rows = rows.borrow_mut();
        let at = rows
            .iter()
            .position(|e| e.code == code && std::sync::Arc::ptr_eq(&e.job.0, &job.0))?;
        let entry = rows.remove(at);
        out.extend_from_slice(&entry.bytes);
        let len = entry.bytes.len();
        rows.insert(0, entry);
        Some(len)
    })
}

/// File `bytes` as this input's encode, and evict from the back until the
/// table is inside [`CAP_BYTES`].
fn remember(code: u8, job: &DescribedJob, bytes: &[u8]) {
    if bytes.len() < FLOOR_BYTES || bytes.len() > CAP_BYTES {
        return;
    }
    // **Taken out of the borrow before anything frees.** `discard` hands the
    // payload to another thread natively and files it on wasm, and neither
    // route comes back through here — but a drop running under `ROWS`'
    // `borrow_mut` would panic if one ever did, and that is not a hazard worth
    // leaving in place for the sake of one scope.
    let evicted = ROWS.with(|rows| {
        let mut rows = rows.borrow_mut();
        let mut held = HELD_BYTES.with(Cell::get);
        // An input can only be filed once: a hit promotes rather than
        // re-files, so reaching here for a code+input already present would
        // mean two entries answering one question.
        rows.retain(|e| {
            let same = e.code == code && std::sync::Arc::ptr_eq(&e.job.0, &job.0);
            if same {
                held -= e.bytes.len();
            }
            !same
        });
        rows.insert(
            0,
            Entry {
                code,
                job: job.clone(),
                bytes: bytes.to_vec(),
            },
        );
        held += bytes.len();
        // Oldest first, and handed back rather than dropped: an entry is a
        // megabyte-scale flat block, and a free that large can return pages to
        // the system rather than merely update a pointer.
        let mut evicted = Vec::new();
        while held > CAP_BYTES {
            match rows.pop() {
                Some(gone) => {
                    held -= gone.bytes.len();
                    evicted.push(gone);
                }
                None => break,
            }
        }
        HELD_BYTES.with(|h| h.set(held));
        evicted
    });
    // The frame thread is where a multi-megabyte teardown must not land; this
    // is the lane the rest of this crate retires large payloads through, and it
    // prices them so the discard ledger's total stays honest.
    for gone in evicted {
        crate::offload::discard(
            "encode_cache entry",
            crate::offload::Priced::new(gone.bytes.len() as u64, gone),
        );
    }
}
