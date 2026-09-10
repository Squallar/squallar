//! **The clutter-filter-power moment is not decoded, and this is what that is
//! worth.**
//!
//! # Why CFP and nothing else
//!
//! A decoded volume retains seven moment slots and 95.9 % of its bytes are
//! their gate buffers ([`crate::scan_size`]). Six of the seven are read:
//! reflectivity, velocity and spectrum width by the plan view, the sampler,
//! the volumetric products and the velocity chain; ZDR, ΦDP and ρHV by the
//! plan view, the sampler and `crate::dpprep`'s dual-pol chain (HCA, HHC,
//! KDP). **The seventh is read by nothing.** Across the workspace, outside
//! tests, `Radial::clutter_filter_power` has exactly three call sites and not
//! one of them is a consumer of the data:
//!
//! * [`crate::scan_size`] adds its length to a volume's price,
//! * [`crate::skeleton`] strips it along with the rest, and
//! * [`crate::volume_wire`] copies it across the wasm message port.
//!
//! It has no [`crate::types::MomentSlot`] variant, no
//! [`crate::types::RadarProduct`] variant, no colour scale and no hover row.
//! It was retained so that it could be counted, stripped and copied.
//!
//! # What it costs to retain, measured
//!
//! 108 real archive volumes (`~/.cache/rd-t18-seam-corpus/arb`, decoded under
//! this crate's own `scan_bytes` walk, 2026-09-09), CFP present on 108 of
//! 108:
//!
//! | | min | median | max |
//! | --- | --- | --- | --- |
//! | CFP MiB per volume | 0.63 | **6.01** | 9.20 |
//! | CFP blocks per volume | 360 | **4,320** | 7,560 |
//! | share of the volume's bytes | 11.75 % | **12.38 %** | 18.45 % |
//! | share of its moment blocks | 12.50 % | **13.33 %** | 20.00 % |
//!
//! It is a block story as much as a byte one: a median volume holds 32,400
//! moment blocks and 4,320 of them are CFP.
//!
//! # Why the drop is at the DECODER and not at a cache
//!
//! Every other memory lever over a decoded volume in this tree is a retention
//! policy — `LoopDownloadManager::evict_decoded_except`, `evict_decoded_to_ceiling`,
//! `App::release_unneeded_base_gates` — and each buys its bytes back with a
//! re-decode when a reader returns. This buys nothing back because nothing
//! reads it, so there is no policy, no eviction order, no second residency
//! state and no re-decode. The gate array is never allocated at all:
//! `nexrad_decode`'s `into_radial_without_clutter_filter_power` skips the
//! `into_owned()` that copies it out of the borrowed record.
//!
//! That also means it frees every holder at once. A volume is cloned into up
//! to eight fields (`crate::scan_size`'s note) and dropping one reference
//! frees nothing; bytes that were never allocated are absent from all eight.
//!
//! # What a reader loses
//!
//! `Radial::clutter_filter_power()` answers `None` on every volume this
//! application decodes, where it used to answer `Some`. A moment's PRESENCE is
//! a thing this tree reads — `crate::types::discover_product_elevations`
//! tests ΦDP and ρHV with `is_some` to decide whether the dual-pol products
//! list — so this is stated rather than assumed: **nothing tests CFP's
//! presence.** If a CFP product is ever added, the reversal is one call site
//! per decode path and the archive bytes a volume was decoded from are already
//! held (`LoopDownloadManager::archive_cache`), so existing volumes come back
//! with a decode and not a download.
//!
//! # The counter
//!
//! Always on, and a running total rather than a level. It exists because a
//! mechanism that executes zero times reads exactly like one that works: the
//! `VolumeSkeleton` beside it was landed with its own note saying "nothing
//! stores one yet", and `App::release_unneeded_base_gates` shipped and did not
//! fire once until the day after. [`redecodes`] is published for the same
//! reason from the other direction — it is 0 by construction and a non-zero
//! reading would mean this module's premise is false.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// CFP moments that were decoded past rather than materialised.
static DROPPED: AtomicU64 = AtomicU64::new(0);

/// Gate bytes those moments would have held, one
/// [`crate::scan_size::ALLOCATOR_BLOCK_OVERHEAD`] apiece included, so the
/// figure is comparable with `scan_bytes` without a caveat.
static BYTES: AtomicU64 = AtomicU64::new(0);

/// Decodes re-run because a reader wanted a moment this module dropped.
///
/// **Zero by construction** and published anyway: see the module note.
static REDECODES: AtomicU64 = AtomicU64::new(0);

/// Record one radial's skipped CFP block, `encoded` bytes of gate data.
///
/// A zero-length block is charged nothing at all, block included — the same
/// convention [`crate::scan_size::gate_bytes`] uses, because a `Vec` of zero
/// length never asked the allocator for anything. A radial that carried no
/// CFP block at all reaches here with `encoded == 0` and is likewise not
/// counted: it had nothing to drop.
pub fn dropped_cfp(encoded: usize) {
    if encoded == 0 {
        return;
    }
    DROPPED.fetch_add(1, Relaxed);
    BYTES.fetch_add(
        (encoded + crate::scan_size::ALLOCATOR_BLOCK_OVERHEAD) as u64,
        Relaxed,
    );
}

/// Record a decode re-run to recover a dropped moment. Nothing calls this;
/// see [`REDECODES`].
pub fn redecoded() {
    REDECODES.fetch_add(1, Relaxed);
}

/// CFP moments dropped since the process started. One block apiece, so this
/// is also the block count.
#[must_use]
pub fn dropped() -> u64 {
    DROPPED.load(Relaxed)
}

/// Blocks never allocated. Equal to [`dropped`] — one gate buffer is one
/// allocation — and spelled separately because a population of ~32,400 blocks
/// per volume is a block story as much as a byte one, and a reader should not
/// have to know they are the same number to quote it.
#[must_use]
pub fn blocks() -> u64 {
    DROPPED.load(Relaxed)
}

/// Bytes never allocated, block overhead included.
#[must_use]
pub fn bytes() -> u64 {
    BYTES.load(Relaxed)
}

/// Decodes re-run to recover a dropped moment. See [`REDECODES`].
#[must_use]
pub fn redecodes() -> u64 {
    REDECODES.load(Relaxed)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "moment_drop/tests.rs"]
mod tests;
