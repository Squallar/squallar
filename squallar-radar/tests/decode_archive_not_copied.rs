//! **That decoding a Level II archive does not duplicate the archive**, watched
//! at the allocator.
//!
//! `DecodeJob::run` used to call `decode_bytes(input.archive.as_ref().clone())`.
//! `JobSpec::run` only ever sees `&Self::In`, and `decode_bytes` takes the
//! buffer by value, so the whole archive was copied to be read: **0.34–17.96 MB
//! per decode** (median 5.56 MB — the compressed-archive spread over the 208
//! real volumes `squallar_app::volume_inventory` measures its slot against),
//! resident for the length of the decode, on up to `concurrent_renders` decodes
//! at once.
//!
//! **`live_bytes` is the wrong instrument here and this suite deliberately does
//! not use it.** The copy is a *transient*: it is taken and freed inside the
//! decode, so a level sampled before and after reads the same number whether the
//! copy happened or not. What can see it is the allocation itself, so this
//! binary installs a `#[global_allocator]` that counts grants at or above the
//! archive's own size within an explicit window — the shape
//! `squallar-overlays/tests/gmgsi_staging_release.rs` uses, for the same reason.

#![cfg(not(target_arch = "wasm32"))]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use squallar_radar::jobs::DecodeJob;
use squallar_source::job::{JobGeometry, JobSpec};

/// The archive this suite decodes. Above the 5.56 MB median of the real corpus,
/// so a copy of it is unmistakable, and small enough to build in a test.
const ARCHIVE_BYTES: usize = 8 * 1024 * 1024;

/// What counts as a block worth watching: half the archive. Nothing else the
/// decode path takes on a refused volume comes near it.
const LARGE: usize = ARCHIVE_BYTES / 2;

static LARGE_ALLOCS: AtomicUsize = AtomicUsize::new(0);
static COUNTING: AtomicBool = AtomicBool::new(false);
static SIZES: Mutex<Vec<usize>> = Mutex::new(Vec::new());

struct LargeBlocks;

impl LargeBlocks {
    fn note(size: usize) {
        if size < LARGE || !COUNTING.load(Ordering::Relaxed) {
            return;
        }
        LARGE_ALLOCS.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut sizes) = SIZES.lock() {
            sizes.push(size);
        }
    }
}

unsafe impl GlobalAlloc for LargeBlocks {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::note(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        Self::note(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if new_size > layout.size() {
            Self::note(new_size);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: LargeBlocks = LargeBlocks;

/// Count the large blocks `f` takes.
fn counting<T>(f: impl FnOnce() -> T) -> (T, usize, Vec<usize>) {
    SIZES.lock().expect("no poison").clear();
    COUNTING.store(true, Ordering::Relaxed);
    let before = LARGE_ALLOCS.load(Ordering::Relaxed);
    let out = f();
    let took = LARGE_ALLOCS.load(Ordering::Relaxed) - before;
    COUNTING.store(false, Ordering::Relaxed);
    let sizes = SIZES.lock().map(|s| s.clone()).unwrap_or_default();
    (out, took, sizes)
}

/// A decode job's envelope. A decode draws nothing, so this is the same
/// effective zero `App::decode_offloaded` hands it.
fn geometry() -> JobGeometry {
    JobGeometry {
        width: 0,
        height: 0,
        bounds: squallar_geo::GeoBounds {
            min_lat: 0.0,
            max_lat: 0.0,
            min_lon: 0.0,
            max_lon: 0.0,
        },
        side_ceiling_px: 0,
    }
}

/// An uncompressed archive-shaped buffer: no gzip magic, so the decode takes
/// the modern `_V06` arm — the one every volume since ~2016 takes.
fn uncompressed_archive() -> Arc<Vec<u8>> {
    let mut bytes = vec![0u8; ARCHIVE_BYTES];
    bytes[..4].copy_from_slice(b"AR2V");
    Arc::new(bytes)
}

/// **The decode does not copy the archive.**
///
/// Floor — put `decode_bytes(input.archive.as_ref().clone())` back in
/// `DecodeJob::run`: the count below reads 1 and the size recorded is
/// `ARCHIVE_BYTES` exactly.
#[test]
fn running_a_decode_job_takes_no_copy_of_the_archive() {
    let job = DecodeJob {
        archive: uncompressed_archive(),
    };
    let geo = geometry();

    let (_out, allocs, sizes) = counting(|| <DecodeJob as JobSpec>::run(&job, &geo));

    assert_eq!(
        allocs, 0,
        "the decode took {allocs} block(s) of at least {LARGE} B to read an \
         archive it was already handed: {sizes:?}. Each one is a whole \
         duplicated volume resident for the length of the decode.",
    );
    assert!(
        Arc::strong_count(&job.archive) >= 1,
        "the archive must still be the caller's afterwards",
    );

    // Control: the instrument can see a block of this class.
    let ((), control_allocs, control_sizes) = counting(|| {
        let block = vec![0u8; ARCHIVE_BYTES];
        std::hint::black_box(&block);
    });
    assert_eq!(
        control_allocs, 1,
        "an explicit archive-sized allocation went unseen, so the figure above \
         says nothing: {control_sizes:?}",
    );
}

/// **The two doors decode the same.** `decode_shared` is `decode_bytes` over a
/// pointer, and the only thing between them is which `nexrad_data::volume::File`
/// constructor runs — so anything one refuses the other must refuse, and the
/// gzip arm has to still inflate.
#[test]
fn the_shared_door_and_the_owned_door_agree() {
    use std::io::Write;

    // A gzip-wrapped payload, which is the arm that still has to allocate: the
    // pre-2016 archive shape.
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    enc.write_all(&vec![0u8; 64 * 1024]).expect("compress");
    let wrapped = enc.finish().expect("finish");
    assert_eq!(
        &wrapped[..2],
        &[0x1f, 0x8b],
        "premise: this really is gzip-wrapped, or the arm below is the plain one",
    );

    for (bytes, what) in [
        (wrapped, "a gzip-wrapped archive"),
        (vec![0u8; 4096], "a short uncompressed buffer"),
        // 24 bytes is the Archive II header and nothing after it. Not shorter:
        // `File::records` indexes `[24..]` unguarded, so an empty buffer panics
        // inside the vendored crate — the same panic on both doors, and older
        // than this change.
        (vec![0u8; 24], "a header with no records behind it"),
    ] {
        let owned = squallar_radar::scan::decode_bytes(bytes.clone());
        let shared = squallar_radar::scan::decode_shared(Arc::new(bytes));
        assert_eq!(
            owned.is_ok(),
            shared.is_ok(),
            "the two doors disagree on {what}",
        );
        if let (Err(o), Err(s)) = (&owned, &shared) {
            assert_eq!(
                o.to_string(),
                s.to_string(),
                "the two doors refuse {what} for different reasons",
            );
        }
    }
}
