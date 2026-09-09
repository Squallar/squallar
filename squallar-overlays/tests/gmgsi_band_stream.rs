//! **The real granule's mosaic, read band by band, is the mosaic read whole —
//! bit for bit — and a real decode takes that path.**
//!
//! `squallar_netcdf`'s own suite pins the same property on a synthetic fixture
//! it can shape at will. This pins it on the product: GMGSI's `data` as NOAA
//! actually stores it, `(time=1, yc, xc)` in 16 shuffled, deflated chunks of
//! `1 x 793 x 1322`, with whatever CF attributes the file carries rather than
//! the ones a fixture chose to set.
//!
//! # Why the whole read had to go
//!
//! `hdf5_pure` 0.44's narrowest read is a window along the **first** dimension,
//! and `data`'s first dimension is `time`, of extent 1 — so the reader
//! assembled all 60,000,000 B of the variable's storage before
//! `read_unpacked_f32_to` walked it one element at a time. Two of those were
//! co-live at the memory campaign's peak. The band walk holds one band of
//! chunks instead: 4 x 4,193,384 = **16,773,536 B**, allocated once and reused
//! for each of the four bands.
//!
//! That is a smaller *quantity*, not the same quantity respelled: the band
//! buffers are four real blocks the allocator is asked for and their sum is the
//! figure. `gmgsi_staging_blocks.rs` counts the block that left.
//!
//! # The falsifiability floor
//!
//! Every assertion here would also hold in a build where the band path never
//! fired and both arms ran the same code, so the counter delta is asserted
//! before anything is compared.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use squallar_overlays::gmgsi::{GmgsiChannel, decode, staging};

/// Blocks at or above this are recorded, with their sizes.
///
/// **1 MiB, and low on purpose.** The question this file answers is not "is
/// the peak smaller" but "is it smaller *for a reason that is not a respelling*
/// " — `live_peak` counts requested bytes, so trading one 60,000,000 B block
/// for fifteen 4 MB ones would buy nothing and would read as a clean pass on
/// any bar set above them. A bar under every block a decode makes is the only
/// one that can tell the two apart.
const RECORD: usize = 1024 * 1024;

static RECORDING: AtomicBool = AtomicBool::new(false);
static SEEN: Mutex<Vec<usize>> = Mutex::new(Vec::new());

struct Recording;

impl Recording {
    fn note(size: usize) {
        if size < RECORD || !RECORDING.load(Ordering::Relaxed) {
            return;
        }
        // Re-entrant by construction: pushing may allocate, and a nested
        // record would deadlock on a non-reentrant lock. The flag is cleared
        // for the push and restored after, so the instrument never measures
        // itself.
        RECORDING.store(false, Ordering::Relaxed);
        if let Ok(mut seen) = SEEN.lock() {
            seen.push(size);
        }
        RECORDING.store(true, Ordering::Relaxed);
    }
}

#[expect(
    unsafe_code,
    reason = "GlobalAlloc is an unsafe trait; every call is forwarded to System unchanged"
)]
unsafe impl GlobalAlloc for Recording {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::note(layout.size());
        // SAFETY: the caller's contract is `GlobalAlloc::alloc`'s, forwarded.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller's contract is `GlobalAlloc::dealloc`'s, forwarded.
        unsafe { System.dealloc(ptr, layout) }
    }

    /// A grow past the bar counts: a `Vec` that reallocates its way up to a
    /// mosaic has taken a mosaic-sized block, whatever the call was named.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if layout.size() < new_size {
            Self::note(new_size);
        }
        // SAFETY: the caller's contract is `GlobalAlloc::realloc`'s, forwarded.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Recording = Recording;

/// Every block at or above [`RECORD`] that `f` asked for, in order.
fn recording<T>(f: impl FnOnce() -> T) -> (T, Vec<usize>) {
    SEEN.lock().expect("no poison").clear();
    RECORDING.store(true, Ordering::Relaxed);
    let out = f();
    RECORDING.store(false, Ordering::Relaxed);
    let seen = SEEN.lock().map(|s| s.clone()).unwrap_or_default();
    (out, seen)
}

/// One band of `data`: four chunks of `1 x 793 x 1322` `f32`.
const CHUNK_BYTES: usize = 793 * 1322 * size_of::<f32>();
const ACROSS: usize = 4;
const BAND_BYTES: usize = ACROSS * CHUNK_BYTES;

const GRANULE: &[u8] = include_bytes!(
    "../testdata/GLOBCOMPLIR_v3r0_blend_s202506011200000_e202506011209599_c202506011234579.nc"
);

/// The granule's `data(time, yc, xc)` dimensions.
const POINTS: usize = 3000 * 5000;

/// **The band-streamed mosaic is the whole-read mosaic, bit for bit — and the
/// shipped decode is what takes that path.**
///
/// One test rather than two: the counters are process-global and `cargo test`
/// runs a binary's tests on parallel threads, so two functions reading deltas
/// off the same counters would be measuring each other.
///
/// The two arms are the two readers on the same variable of the same file:
/// `read_unpacked_f32` assembles the storage whole and walks it,
/// `read_unpacked_f32_into` walks the chunks a band at a time and gathers each
/// element out of the shuffle's four planes. Compared by `to_bits`, because a
/// missing value is a `NaN` on both arms and `==` matches no `NaN` at all — an
/// `assert_eq!` on the values would pass whatever either arm did with the rest.
#[test]
fn the_products_mosaic_reads_the_same_bits_either_way_and_the_decode_takes_it() {
    let granule = squallar_netcdf::Granule::open(GRANULE).expect("the committed granule opens");

    let before = squallar_netcdf::band_stream_totals();
    let mut banded: Vec<f32> = Vec::new();
    let count = granule
        .read_unpacked_f32_into("data", &mut banded)
        .expect("read")
        .expect("`data` is present");
    let after = squallar_netcdf::band_stream_totals();

    assert!(
        after.reads > before.reads,
        "the band path did not fire on the product's own storage, so the \
         comparison below is one arm against itself: {before:?} -> {after:?}",
    );
    assert_eq!(
        after.bytes_avoided - before.bytes_avoided,
        (POINTS * size_of::<f32>() - BAND_BYTES) as u64,
        "the whole variable less the band it held: 60,000,000 B less four \
         chunks of {CHUNK_BYTES} B",
    );

    assert_eq!(count, POINTS);
    let whole = granule
        .read_unpacked_f32("data")
        .expect("read")
        .expect("`data` is present");
    assert_eq!(whole.values.len(), POINTS);

    let differing = banded
        .iter()
        .zip(&whole.values)
        .enumerate()
        .find(|(_, (a, b))| a.to_bits() != b.to_bits());
    assert!(
        differing.is_none(),
        "band-streamed and whole reads of the product's mosaic differ at \
         {differing:?}",
    );

    // ── And the shipped entry point reaches it ───────────────────────────
    // The arms above drive the reader directly; this drives `gmgsi::decode`,
    // which is what `gmgsi::fetch::fetch_key` runs — so what is shown is that
    // the shipped path reaches the cut, not merely that the cut exists.
    let before = squallar_netcdf::band_stream_totals();
    let grid = decode::decode(GRANULE.to_vec(), GmgsiChannel::LongwaveIr)
        .expect("the committed granule decodes");
    let after = squallar_netcdf::band_stream_totals();

    assert_eq!(
        after.reads - before.reads,
        1,
        "one decode reads exactly one variable band by band — `data`. The two \
         coordinate arrays are one 60,000,000 B chunk each, which has no band \
         smaller than itself, so they take the whole read and are refused a \
         plan: {before:?} -> {after:?}",
    );
    assert_eq!(after.elements - before.elements, POINTS as u64);

    let squallar_overlays::render::gridded::GridValues::Bytes(raster) = &grid.grid.values else {
        panic!("a GMGSI raster is a byte store");
    };
    assert_eq!(
        raster.codes().len(),
        POINTS,
        "and the raster it produced is the whole mosaic, at the width the \
         narrowing stores it",
    );
    // Back to the staging slot the way the frame cache's eviction does it, so
    // the decode below is handed a buffer rather than allocating one: a
    // dropped grid takes its block with it, and measuring *that* would be
    // measuring the pool, not the reader.
    staging::recycle(staging::global(), grid.grid);

    // ── What replaced the 60,000,000 B block, observed rather than derived ──
    // `bytes_avoided` above is the *plan's* figure — what the walk intended to
    // hold. This is what the allocator was actually asked for, at a bar under
    // every block a decode makes, so it can tell a smaller peak from the same
    // peak in smaller pieces. `live_peak` counts requested bytes: one
    // 60,000,000 B block and fifteen 4 MB ones cost it the same, and only an
    // arm that sees both sizes can refuse that trade.
    //
    // The decode above was this process's first — it allocated the raster and
    // read both coordinate arrays — so the slot and the axis cache are warm
    // and the one below is the steady state a playing loop lives in.
    let (grid, seen) =
        recording(|| decode::decode(GRANULE.to_vec(), GmgsiChannel::LongwaveIr).expect("decodes"));
    staging::recycle(staging::global(), grid.grid);

    let band: Vec<usize> = seen.iter().copied().filter(|&s| s == CHUNK_BYTES).collect();
    assert_eq!(
        band.len(),
        ACROSS,
        "a steady-state decode holds one band — {ACROSS} chunks of \
         {CHUNK_BYTES} B — and asks for them once, not once per band. Blocks \
         at or above {RECORD} B, in order: {seen:?}",
    );

    let over: Vec<usize> = seen.iter().copied().filter(|&s| s > CHUNK_BYTES).collect();
    assert!(
        over.is_empty(),
        "nothing a steady-state decode asks for is larger than one chunk: the \
         mosaic comes out of the staging slot and the reader no longer \
         assembles the variable. Over-sized blocks: {over:?}; all blocks at or \
         above {RECORD} B: {seen:?}",
    );

    let total: usize = seen.iter().sum();
    assert_eq!(
        total, BAND_BYTES,
        "and the band is the whole of it — {BAND_BYTES} B against the \
         60,000,000 B the assembled variable used to cost. A pass here with a \
         larger total would be the trade this test exists to refuse. Blocks: \
         {seen:?}",
    );
}
