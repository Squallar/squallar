//! **That a volume decode lends its archive instead of writing it.**
//!
//! The `decode` row's whole message IS the downloaded Level II volume —
//! 0.34-17.96 MB, median 5.56 MB over the 208 real volumes
//! `squallar_app::volume_inventory` measures its slot against — and its
//! `encode` memcpy'd every byte of it into a wire buffer inside
//! `JobRequest::to_parts`, which runs at the dispatch site, on the FRAME
//! THREAD.
//!
//! `App::decode_offloaded` already keeps a refcount on that archive for the
//! loop's compressed cache, so there is a live owner to lend against and the
//! write has nowhere it needs to be. The row nominates it as a
//! `ResidentBytes` and writes only the envelope; the transport lends it in
//! place, exactly as the gridded overlay row's band is lent.
//!
//! Its own binary with a counting `#[global_allocator]`, for the reason
//! `squallar-radar/tests/decode_archive_not_copied.rs` gives: the write is a
//! transient, freed as the message is sent, so a level sampled before and
//! after reads the same number whether it happened or not. What can see it is
//! the allocation.
//!
//! **One `#[test]`**, for the reason that suite gives too: the counter and its
//! window are process-global and libtest runs a binary's tests on several
//! threads.

#![cfg(not(target_arch = "wasm32"))]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// What counts as a block worth watching: well under the archive and far above
/// the envelope, so a written archive is unmistakable and a lent one is a zero.
const LARGE: usize = 64 * 1024;

static LARGE_ALLOCS: AtomicUsize = AtomicUsize::new(0);
/// Bytes a `realloc` had to carry across — the copy the growth walk pays.
static MOVED: AtomicUsize = AtomicUsize::new(0);
static COUNTING: AtomicBool = AtomicBool::new(false);
static SIZES: Mutex<Vec<usize>> = Mutex::new(Vec::new());

struct LargeBlocks;

impl LargeBlocks {
    fn note(size: usize, moved: usize) {
        if size < LARGE || !COUNTING.load(Ordering::Relaxed) {
            return;
        }
        LARGE_ALLOCS.fetch_add(1, Ordering::Relaxed);
        MOVED.fetch_add(moved, Ordering::Relaxed);
        if let Ok(mut sizes) = SIZES.lock() {
            sizes.push(size);
        }
    }
}

unsafe impl GlobalAlloc for LargeBlocks {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::note(layout.size(), 0);
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        Self::note(layout.size(), 0);
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if new_size > layout.size() {
            Self::note(new_size, layout.size());
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: LargeBlocks = LargeBlocks;

/// Count the large blocks `f` takes, and the bytes its reallocations carried.
fn counting<T>(f: impl FnOnce() -> T) -> (T, usize, usize, Vec<usize>) {
    SIZES.lock().expect("no poison").clear();
    LARGE_ALLOCS.store(0, Ordering::Relaxed);
    MOVED.store(0, Ordering::Relaxed);
    COUNTING.store(true, Ordering::Relaxed);
    let out = f();
    COUNTING.store(false, Ordering::Relaxed);
    let sizes = SIZES.lock().map(|s| s.clone()).unwrap_or_default();
    (
        out,
        LARGE_ALLOCS.load(Ordering::Relaxed),
        MOVED.load(Ordering::Relaxed),
        sizes,
    )
}

/// **A volume decode's archive is lent, not written into the message.**
///
/// The `decode` row's whole message IS the downloaded archive — 0.34-17.96 MB,
/// median 5.56 MB — and `encode` memcpy'd all of it into a wire buffer at the
/// dispatch site. The page already holds it behind an `Arc`, so `to_parts`
/// nominates it and writes only the envelope.
///
/// Floor — delete `DecodeJob::resident_payload`: the payload below reads
/// `None`, the head grows to the archive's own size, and the count reads at
/// least one block instead of zero.
#[test]
fn a_volume_decode_lends_its_archive_rather_than_writing_it() {
    const ARCHIVE_BYTES: usize = 8 * 1024 * 1024;
    let mut bytes = vec![0u8; ARCHIVE_BYTES];
    bytes[..4].copy_from_slice(b"AR2V");
    let archive = std::sync::Arc::new(bytes);
    let request = squallar_worker::offload::JobRequest::describe(
        squallar_radar::jobs::DecodeJob {
            archive: std::sync::Arc::clone(&archive),
        },
        squallar_worker::offload::ceiling_only_geometry(0),
    );

    let ((head, payload), allocs, moved, sizes) = counting(|| request.to_parts());
    let payload = payload.expect(
        "the decode row must nominate its archive; without that the whole \
         volume is written into the head on the frame thread",
    );
    assert_eq!(
        payload.len(),
        ARCHIVE_BYTES,
        "the lent range must be the whole archive",
    );
    assert!(
        std::ptr::eq(payload.bytes().as_ptr(), archive.as_ptr()),
        "the payload must be a view onto the page's own archive, not a copy \
         of it",
    );
    assert!(
        head.len() < LARGE,
        "the head is {} B; with the archive lent it is the envelope alone",
        head.len(),
    );
    assert_eq!(
        allocs, 0,
        "framing a {ARCHIVE_BYTES} B archive took {allocs} block(s) of at \
         least {LARGE} B ({sizes:?}, {moved} B carried). Each one is a whole \
         duplicated volume, written at the dispatch site on the frame thread.",
    );

    // Control: the instrument can see a block of this class.
    let ((), control, control_sizes) = {
        let (out, a, _, s) = counting(|| {
            let block = vec![0u8; ARCHIVE_BYTES];
            std::hint::black_box(&block);
        });
        (out, a, s)
    };
    assert_eq!(
        control, 1,
        "an explicit archive-sized allocation went unseen, so the figure \
         above says nothing: {control_sizes:?}",
    );

    // **And the split says exactly what the whole message said.** A head that
    // decodes to a different job than `to_bytes` produces is a silently wrong
    // volume, not an error.
    let whole = squallar_worker::offload::JobRequest::from_bytes(&request.to_bytes())
        .expect("the whole-message form decodes");
    let split = squallar_worker::offload::JobRequest::from_parts(&head, payload.bytes())
        .expect("the split form decodes");
    assert_eq!(
        whole.geometry, split.geometry,
        "the two forms disagree about the envelope",
    );
    assert_eq!(
        split
            .job
            .downcast_ref::<squallar_radar::jobs::DecodeJob>()
            .map(|j| j.archive.as_slice()),
        Some(archive.as_slice()),
        "the split form rebuilt a different archive than was lent",
    );
}
