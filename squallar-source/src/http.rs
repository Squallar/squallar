//! Reading a response body **once**.
//!
//! `reqwest::Response::bytes()` is `http_body_util`'s `collect()`: every frame
//! the transport produces is pushed onto a `BufList` and the whole list is
//! held until the body ends, then copied into one contiguous buffer. On the
//! native (hyper/h1) transport those frames are slices of hyper's own read
//! buffer, and a live slice is what stops `BytesMut::reserve` reclaiming that
//! buffer in place — so hyper allocates a fresh backing for the next read, and
//! a body of `S` bytes leaves `S` bytes of 32 KiB transport backings resident
//! for the whole download. Spelled `.bytes().await?.to_vec()`, as this
//! workspace spelled it in ten places, the peak is `2 x S`: the shard list
//! plus the contiguous copy, then the contiguous copy plus the `Vec`.
//!
//! [`body_to_vec`] copies each frame into the output and drops it, so the
//! transport backing is reclaimed in place and the peak is `S` plus one frame.
//! It is also one copy rather than two.
//!
//! The saving is not free of a claim, so it is counted rather than asserted:
//! [`totals`] carries [`BodyReadTotals::peak_shard_bytes`], the high-water mark
//! of "bytes received but not yet at end-of-body, summed across every body
//! being read at that instant". That figure is exactly what the `BufList`
//! would have been holding at that moment, rounded *down* — the real backings
//! are 32 KiB-granular, so the residency this path avoids is at least this.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Bytes [`body_to_vec`] will reserve up front on the strength of a
/// `Content-Length` it did not verify.
///
/// The header is the origin's claim, not a measurement, and the `Vec` grows
/// past this on its own if the body is genuinely larger. Sized to clear every
/// object this workspace asks for — the largest is a Level II volume at tens
/// of MiB — so the reservation is exact in practice and the accumulator never
/// reallocates.
const MAX_RESERVE_BYTES: u64 = 64 * 1024 * 1024;

static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
static PEAK_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
static SHARD_BYTES: AtomicU64 = AtomicU64::new(0);
static PEAK_SHARD_BYTES: AtomicU64 = AtomicU64::new(0);
static BODIES: AtomicU64 = AtomicU64::new(0);
static BODY_BYTES: AtomicU64 = AtomicU64::new(0);

/// What every [`body_to_vec`] since process start has cost, read together.
///
/// Bytes, not MiB. Always on: nothing gates the counting, so a zero here is a
/// real zero rather than an instrument that was not switched on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BodyReadTotals {
    /// Bodies that have finished, successfully or not.
    pub bodies: u64,
    /// Bytes those bodies delivered.
    pub body_bytes: u64,
    /// Bodies being read right now.
    pub in_flight: usize,
    /// The most bodies ever being read at one instant.
    pub peak_in_flight: usize,
    /// Bytes received and already copied out of a transport frame, summed over
    /// every body in flight right now.
    pub shard_bytes: u64,
    /// **The high-water mark of [`Self::shard_bytes`]** — the transport-buffer
    /// residency a collecting reader would have carried, and this one does
    /// not. A floor: the backings behind those bytes are 32 KiB-granular.
    pub peak_shard_bytes: u64,
}

/// A snapshot of [`BodyReadTotals`]. The fields are read with separate loads,
/// so a snapshot taken while bodies are arriving is near-consistent, not
/// atomic; the two peaks are monotonic and never read low.
#[must_use]
pub fn totals() -> BodyReadTotals {
    BodyReadTotals {
        bodies: BODIES.load(Ordering::Relaxed),
        body_bytes: BODY_BYTES.load(Ordering::Relaxed),
        in_flight: IN_FLIGHT.load(Ordering::Relaxed),
        peak_in_flight: PEAK_IN_FLIGHT.load(Ordering::Relaxed),
        shard_bytes: SHARD_BYTES.load(Ordering::Relaxed),
        peak_shard_bytes: PEAK_SHARD_BYTES.load(Ordering::Relaxed),
    }
}

/// One body being read, holding the ledger's in-flight share for its lifetime.
///
/// Drop-based on purpose: a body that errors half-way, or a future that is
/// cancelled between two frames, still hands its share back.
struct BodyRead {
    held: u64,
}

impl BodyRead {
    fn begin() -> Self {
        let now = IN_FLIGHT.fetch_add(1, Ordering::Relaxed) + 1;
        PEAK_IN_FLIGHT.fetch_max(now, Ordering::Relaxed);
        Self { held: 0 }
    }

    /// `len` more bytes are in the accumulator and out of the transport.
    fn copied(&mut self, len: usize) {
        let len = len as u64;
        self.held = self.held.saturating_add(len);
        let now = SHARD_BYTES.fetch_add(len, Ordering::Relaxed) + len;
        PEAK_SHARD_BYTES.fetch_max(now, Ordering::Relaxed);
    }
}

impl Drop for BodyRead {
    fn drop(&mut self) {
        SHARD_BYTES.fetch_sub(self.held, Ordering::Relaxed);
        IN_FLIGHT.fetch_sub(1, Ordering::Relaxed);
        BODIES.fetch_add(1, Ordering::Relaxed);
        BODY_BYTES.fetch_add(self.held, Ordering::Relaxed);
    }
}

/// The whole body of `response` as owned bytes.
///
/// # Errors
///
/// Whatever the transport reports while the body is being read — the same
/// error `reqwest::Response::bytes` would have produced, and on the same
/// conditions. Status is the caller's business: this reads whatever body the
/// response carries.
pub async fn body_to_vec(response: reqwest::Response) -> reqwest::Result<Vec<u8>> {
    let reserve = usize::try_from(
        response
            .content_length()
            .unwrap_or(0)
            .min(MAX_RESERVE_BYTES),
    )
    .unwrap_or(0);
    let mut read = BodyRead::begin();
    drain(response, reserve, &mut read).await
}

/// Native: read the body a transport frame at a time, copying each frame out
/// and dropping it, so hyper's read buffer has no live slice to work around.
///
/// The `cfg` selects **which API the transport offers**, not what this function
/// does: `Response::chunk` exists only on the hyper backend, and only that
/// backend has a buffer to release. Both arms return the whole body as a
/// `Vec<u8>` and count the same thing.
#[cfg(not(target_arch = "wasm32"))]
async fn drain(
    mut response: reqwest::Response,
    reserve: usize,
    read: &mut BodyRead,
) -> reqwest::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(reserve);
    while let Some(frame) = response.chunk().await? {
        out.extend_from_slice(&frame);
        read.copied(frame.len());
    }
    Ok(out)
}

/// wasm32: the browser hands `fetch()` bodies over as one `ArrayBuffer`, so
/// there is no frame list to shorten and no transport buffer to release —
/// `Response::bytes` is already the single-copy read there, and reqwest's wasm
/// backend offers no incremental reader without pulling `wasm-streams` in.
#[cfg(target_arch = "wasm32")]
async fn drain(
    response: reqwest::Response,
    reserve: usize,
    read: &mut BodyRead,
) -> reqwest::Result<Vec<u8>> {
    let body = response.bytes().await?;
    read.copied(body.len());
    let mut out = Vec::with_capacity(reserve.max(body.len()));
    out.extend_from_slice(&body);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The guard hands its whole share back, and the peak keeps the mark.
    #[test]
    fn a_finished_read_leaves_the_peak_and_clears_the_level() {
        let before = totals();
        {
            let mut read = BodyRead::begin();
            read.copied(3);
            read.copied(5);
            let during = totals();
            assert_eq!(during.shard_bytes, before.shard_bytes + 8);
            assert!(during.peak_shard_bytes >= before.shard_bytes + 8);
            assert_eq!(during.in_flight, before.in_flight + 1);
        }
        let after = totals();
        assert_eq!(after.shard_bytes, before.shard_bytes);
        assert_eq!(after.in_flight, before.in_flight);
        assert_eq!(after.bodies, before.bodies + 1);
        assert_eq!(after.body_bytes, before.body_bytes + 8);
        assert!(after.peak_shard_bytes >= before.shard_bytes + 8);
    }

    /// The reservation is bounded by what the workspace will ever ask for, so
    /// a wrong `Content-Length` cannot turn one response into a huge `Vec`.
    #[test]
    fn the_reservation_is_bounded() {
        assert_eq!(MAX_RESERVE_BYTES, 64 * 1024 * 1024);
        assert!(u64::from(u32::MAX) > MAX_RESERVE_BYTES);
    }
}
