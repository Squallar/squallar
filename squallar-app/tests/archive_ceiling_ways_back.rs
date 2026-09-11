//! **What lowering the archive ceiling actually takes off the heap, and that it
//! takes no way back with it.**
//!
//! `loop archives` in the heap census is `LoopDownloadManager::cached_archive_bytes`
//! — `App::handle_redraw` feeds exactly that to `census::set_loop_archive_bytes`
//! — so this suite measures the census term itself rather than a proxy for it.
//!
//! **What this is NOT**: a steady-window census reading off a running app. It
//! drives the manager and the ceiling pass directly, so it measures the term
//! and the allocator level with real corpus-sized archives, on the shipped
//! constants. The t=330-384 s figure from a six-site leg is a different
//! measurement and this one does not stand in for it.
//!
//! Its own binary with a counting `#[global_allocator]` and **one `#[test]`**,
//! because the level is process-global and libtest threads a binary's tests.
//!
//! # The hazard this exists to rule out
//!
//! Lowering a memory ceiling when the overflow goes to a bounded medium can
//! simply move the pressure: the medium fills, refuses, the archive is dropped
//! after all, and the decoded volume in front of it strands un-evictable —
//! which is the defect the spill was built to remove, reappearing one layer
//! down. So the assertion is not only that the heap term fell; it is that the
//! ways back are **all still there** at the lower ceiling.

#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use squallar_device_profile::constants::{
    DESKTOP_LOOP_ARCHIVE_CEILING_BYTES, LOOP_ARCHIVE_SPILL_CEILING_BYTES,
    LOOP_ARCHIVE_WAYS_BACK_BYTES,
};
use squallar_radar::archive_spill::FsArchiveSpill;
use squallar_radar::loop_downloads::LoopDownloadManager;

#[global_allocator]
static ALLOCATOR: squallar_alloc::Counting = squallar_alloc::Counting;

/// The 208-file corpus median compressed archive
/// (min 352,159 / median 5,845,849 / max 18,831,036 B).
const CORPUS_MEDIAN_ARCHIVE: usize = 5_845_849;
/// What the desktop ceiling was before the spill gave a displaced archive
/// somewhere to go. Spelled here as the figure this suite compares against,
/// deliberately a literal: it is the BEFORE of a measurement, so tying it to
/// the constant would make the comparison vanish the moment the constant moved.
const CEILING_BEFORE: usize = 256 * 1024 * 1024;
/// Enough archives to saturate either ceiling.
const HELD: u32 = 60;

fn ts(i: u32) -> chrono::NaiveDateTime {
    chrono::DateTime::from_timestamp(1_700_000_000 + i64::from(i) * 300, 0)
        .expect("a fixed in-range stamp")
        .naive_utc()
}

fn live() -> u64 {
    squallar_alloc::live_bytes().expect("this binary installs the counting allocator")
}

fn oldest_first(_: &str, at: &chrono::NaiveDateTime) -> u64 {
    u64::MAX - (at.and_utc().timestamp() as u64)
}

/// One arm: fill to `HELD` median archives with a spill installed, run the
/// ceiling pass at `ceiling`, and report
/// `(loop_archives_bytes, live_dropped, ways_back, on_disk)`.
///
/// Every archive is owned ONLY by the manager — the local `Arc` is dropped as
/// the loop iterates — so the figure is **sole**, the half a release can
/// actually give back, and not a total another store is also holding.
fn arm(root: &std::path::Path, ceiling: usize) -> (usize, u64, usize, usize) {
    let mut mgr = LoopDownloadManager::new();
    mgr.set_spill(
        Box::new(FsArchiveSpill::new(root.to_path_buf()).expect("a spill root")),
        LOOP_ARCHIVE_SPILL_CEILING_BYTES,
    );
    for i in 0..HELD {
        mgr.cache_archive("KTLX", ts(i), Arc::new(vec![7u8; CORPUS_MEDIAN_ARCHIVE]));
    }
    // Bank `sole` at the instrument: assert the thing being counted is held
    // once, here, rather than leaving a reader to remember which half it is.
    for i in 0..HELD {
        if let Some(held) = mgr.archive_for("KTLX", &ts(i)) {
            assert_eq!(
                Arc::strong_count(&held),
                2,
                "archive {i} is held by something besides this cache and the \
                 local withdrawal, so `loop archives` is not a sole figure and \
                 lowering the ceiling would not give these bytes back",
            );
        }
    }
    let before = live();
    mgr.evict_archives_to_ceiling(ceiling, oldest_first, |_, _, _| false);
    let dropped = before.saturating_sub(live());
    let ways = (0..HELD)
        .filter(|i| mgr.has_archive("KTLX", &ts(*i)))
        .count();
    (
        mgr.cached_archive_bytes(),
        dropped,
        ways,
        mgr.spilled_bytes(),
    )
}

#[test]
fn the_lower_ceiling_takes_heap_bytes_and_no_ways_back() {
    let root = std::env::temp_dir().join("squallar-ceiling-ways-back");
    let _ = std::fs::remove_dir_all(&root);

    let (term_before, live_before, ways_before, disk_before) = arm(&root, CEILING_BEFORE);
    let (term_after, live_after, ways_after, disk_after) =
        arm(&root, DESKTOP_LOOP_ARCHIVE_CEILING_BYTES);

    // **The term is the census term**, and it sits at the ceiling it is given.
    assert!(
        term_before <= CEILING_BEFORE && term_after <= DESKTOP_LOOP_ARCHIVE_CEILING_BYTES,
        "a ceiling pass left the cache over its ceiling: {term_before} vs \
         {CEILING_BEFORE}, {term_after} vs {DESKTOP_LOOP_ARCHIVE_CEILING_BYTES}",
    );
    let cut = term_before.saturating_sub(term_after);
    assert!(
        cut > 0,
        "the lower ceiling took nothing off the heap: {term_before} -> {term_after}",
    );
    // Within one archive of the difference between the two ceilings — the pass
    // stops as soon as it is under, so the residue is one archive at most.
    let ceiling_delta = CEILING_BEFORE - DESKTOP_LOOP_ARCHIVE_CEILING_BYTES;
    assert!(
        cut.abs_diff(ceiling_delta) <= CORPUS_MEDIAN_ARCHIVE,
        "the heap gave back {cut} B against a {ceiling_delta} B ceiling change, \
         more than one archive apart — the ceiling is not what bounds this term",
    );

    // **And every way back is still there at the lower ceiling.** This is the
    // hazard: a lower memory ceiling must not push the pressure into the
    // medium's bound, drop the archive after all and strand the volume.
    assert_eq!(
        (ways_before, ways_after),
        (HELD as usize, HELD as usize),
        "a way back was lost. At the lower ceiling more archives are displaced \
         to the medium, and if its bound refused them they were dropped — which \
         re-strands the median 15.5x-larger decoded volume in front of each, the \
         defect the spill exists to remove.",
    );
    assert!(
        disk_after > disk_before,
        "the lower ceiling displaced no extra bytes to the medium \
         ({disk_before} -> {disk_after}), so the arms are not different",
    );
    assert!(
        disk_after <= LOOP_ARCHIVE_SPILL_CEILING_BYTES,
        "the medium is over its own bound: {disk_after} > {LOOP_ARCHIVE_SPILL_CEILING_BYTES}",
    );
    // The composition, at runtime rather than only as arithmetic.
    assert!(
        term_after + disk_after <= LOOP_ARCHIVE_WAYS_BACK_BYTES,
        "heap {term_after} + medium {disk_after} exceeds the ways-back budget \
         {LOOP_ARCHIVE_WAYS_BACK_BYTES}",
    );

    let resident = squallar_alloc::process::resident().expect("/proc/self/status on linux");
    println!(
        "loop archives: {term_before} B ({:.1} MiB) at the {} MiB ceiling -> \
         {term_after} B ({:.1} MiB) at {} MiB = {cut} B ({:.1} MiB) off the heap\n\
         live_bytes dropped by the pass: {live_before} B -> {live_after} B\n\
         ways back: {ways_before}/{HELD} -> {ways_after}/{HELD}; \
         on medium {disk_before} B -> {disk_after} B of {LOOP_ARCHIVE_SPILL_CEILING_BYTES} B\n\
         rss {} B (anon {}, file {}, shmem {} — the medium here is the OS temp \
         dir, which may be a tmpfs, so shmem is not a disk-case figure)",
        term_before as f64 / 1048576.0,
        CEILING_BEFORE >> 20,
        term_after as f64 / 1048576.0,
        DESKTOP_LOOP_ARCHIVE_CEILING_BYTES >> 20,
        cut as f64 / 1048576.0,
        resident.rss_bytes,
        resident.anon_bytes,
        resident.file_bytes,
        resident.shmem_bytes,
    );

    let _ = std::fs::remove_dir_all(&root);
}
