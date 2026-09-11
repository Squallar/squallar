//! **That moving an archive off the heap really moves the counter the campaign
//! steers by, and that only the spill keeps the way back.**
//!
//! The design turned on one question a byte figure cannot answer by itself:
//! when an archive's bytes leave the heap for a file, which counter moves?
//! `squallar_alloc::live_bytes` is bytes the global allocator granted less
//! bytes returned, so a `Vec` that is dropped moves it and a memory MAPPING
//! would not pass through it at all — an `mmap` of the spilled file would post
//! the whole saving here while the resident pages merely moved from `RssAnon`
//! to `RssFile` and stayed in `VmRSS`. That is why
//! `squallar_radar::archive_spill` reads into a fresh `Vec` and never maps, and
//! why this suite watches `process::resident()` beside the allocator figure
//! rather than trusting one of them.
//!
//! Its own binary with a counting `#[global_allocator]`, for the reason
//! `tests/decode_archive_not_copied.rs` gives, and **one `#[test]`**, because
//! the counter is process-global and libtest runs a binary's tests on several
//! threads.
//!
//! The two arms are the claim. Dropping and spilling free the SAME heap bytes —
//! the spill is not a smaller cut, it is the same cut that keeps the way back —
//! so what separates them is whether the volume in front of the archive can
//! still be evicted afterwards. Without a spill it never can again.

#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use squallar_radar::archive_spill::FsArchiveSpill;
use squallar_radar::loop_downloads::LoopDownloadManager;

#[global_allocator]
static ALLOCATOR: squallar_alloc::Counting = squallar_alloc::Counting;

/// The corpus median compressed archive, measured over the 208-file local
/// Archive II corpus (min 352,159 / median 5,845,849 / max 18,831,036 B).
const MEDIAN_ARCHIVE: usize = 5_845_849;
/// Enough of them to go well past the ceiling below.
const HELD: u32 = 60;
/// The shipped desktop archive ceiling.
const CEILING: usize = 256 * 1024 * 1024;

fn ts(i: u32) -> chrono::NaiveDateTime {
    chrono::DateTime::from_timestamp(1_700_000_000 + i64::from(i) * 300, 0)
        .expect("a fixed in-range stamp")
        .naive_utc()
}

fn live() -> u64 {
    squallar_alloc::live_bytes().expect("this binary installs the counting allocator")
}

/// A manager holding `HELD` median archives for one site, each owned ONLY by
/// the manager — the local `Arc` is dropped at the end of every iteration, so
/// what the counter sees is sole ownership and not a figure another store is
/// also holding.
fn filled(spill: Option<std::path::PathBuf>) -> LoopDownloadManager {
    let mut mgr = LoopDownloadManager::new();
    if let Some(root) = spill {
        mgr.set_spill(
            Box::new(FsArchiveSpill::new(root).expect("a spill root")),
            2 * 1024 * 1024 * 1024,
        );
    }
    for i in 0..HELD {
        mgr.cache_archive("KTLX", ts(i), Arc::new(vec![7u8; MEDIAN_ARCHIVE]));
    }
    mgr
}

/// Furthest-first over the stamp, so the oldest archives are the ones the
/// ceiling reaches — the shipped rank's shape.
fn oldest_first(_: &str, at: &chrono::NaiveDateTime) -> u64 {
    u64::MAX - (at.and_utc().timestamp() as u64)
}

fn ways_back(mgr: &LoopDownloadManager) -> usize {
    (0..HELD)
        .filter(|i| mgr.has_archive("KTLX", &ts(*i)))
        .count()
}

#[test]
fn the_ceiling_gives_the_same_heap_bytes_back_either_way_and_only_a_spill_keeps_the_way_back() {
    // **This process's own directory, never a shared name.** Several lanes
    // run `cargo test --workspace` on this box at once, so two instances of
    // this binary are live together — and `FsArchiveSpill::new` PURGES the
    // root it is given, by design, to close the cross-process leak. Under a
    // constant name that purge lands on the other instance's spilled files
    // while it is still writing them: its `store` returns `false`, the manager
    // treats it as "nowhere to put it" and drops the archive, and the
    // `spill_ways == HELD` assertion below reports a lost way back that this
    // tree does not have. Measured on this binary: 3 of 4 concurrent
    // instances red at 52, 57 and 59 ways back out of 60.
    let root =
        std::env::temp_dir().join(format!("squallar-spill-heap-gate-{}", std::process::id()));
    let before_all = squallar_alloc::process::resident().expect("/proc/self/status on linux");

    // ---- arm 1: no spill, which is this tree before the change ----
    let mut bare = filled(None);
    let filled_live = live();
    let held = bare.cached_archive_bytes();
    assert!(
        held > CEILING,
        "the fixture did not go over the ceiling: {held} B",
    );
    let bare_freed = bare.evict_archives_to_ceiling(CEILING, oldest_first, |_, _, _| false);
    let bare_live_after = live();
    let bare_ways = ways_back(&bare);

    // ---- arm 2: somewhere to put it ----
    let mut spilled = filled(Some(root.clone()));
    let spilled_filled_live = live();
    let spill_freed = spilled.evict_archives_to_ceiling(CEILING, oldest_first, |_, _, _| false);
    let spilled_live_after = live();
    let spill_ways = ways_back(&spilled);
    let after_all = squallar_alloc::process::resident().expect("/proc/self/status on linux");

    // **The same heap bytes, both ways.** The spill is not a smaller cut.
    assert_eq!(
        bare_freed, spill_freed,
        "spilling and dropping did not give the same heap bytes back, so one \
         of the two is not doing what it says",
    );
    assert!(
        bare_freed > 0 && spilled.spilled_bytes() > 0,
        "nothing was evicted or nothing reached the medium: freed \
         {bare_freed}, on-disk {}",
        spilled.spilled_bytes(),
    );

    // **And the counter the campaign steers by really moves**, by about the
    // bytes the pass says it freed. `>= 95 %` and not an equality: the pass
    // also drops map entries and the eviction walk allocates a ranking vector,
    // so an exact match would be asserting the allocator's own bookkeeping.
    let bare_drop = filled_live.saturating_sub(bare_live_after);
    let spill_drop = spilled_filled_live.saturating_sub(spilled_live_after);
    for (tag, dropped) in [("drop", bare_drop), ("spill", spill_drop)] {
        assert!(
            dropped * 100 >= (bare_freed as u64) * 95,
            "{tag}: live_bytes fell {dropped} B against {bare_freed} B the \
             ceiling says it freed — the bytes did not leave the allocator, so \
             the cut is on paper only",
        );
    }

    // **The way back: 0 against every archive that left.** This is what
    // separates the two arms, and it is the whole reason the spill exists —
    // both decoded-eviction policies refuse a volume with no way back, so an
    // archive that is merely DROPPED strands the median 15.5x-larger decoded
    // volume in front of it as permanently un-evictable.
    let evicted = HELD as usize - bare_ways;
    assert!(evicted > 0, "the fixture evicted nothing");
    assert_eq!(
        spill_ways, HELD as usize,
        "an archive that went off-heap stopped being a way back, so the \
         volume in front of it is stranded exactly as dropping it would",
    );
    assert!(
        bare_ways < spill_ways,
        "both arms kept the same ways back, so this suite is not comparing \
         the two behaviours at all",
    );

    // **Nothing was mapped.** A mapping would have posted the same fall in
    // `live_bytes` above while merely moving the pages into another resident
    // class instead of giving them up.
    //
    // **`RssFile` + `RssShmem`, and the second term is the one that can
    // actually fire here.** The kernel files a mapping under the class of the
    // thing mapped, and this spill root is under `std::env::temp_dir()`,
    // which is a tmpfs on this arm — so its pages are SHMEM and not
    // file-backed. Against `file_bytes` alone this was the one assertion in
    // the suite that could not see the medium the spill really uses:
    // tampering `FsArchiveSpill::store` to `mmap` every spilled file and
    // fault it in left this test GREEN, with `file_bytes` up 4,096 B while
    // `VmRSS` grew 87.7 MB. Summed rather than checked one at a time, because
    // the defect is one mapping landing in whichever class its medium has.
    let mapped_growth = (after_all.file_bytes.saturating_add(after_all.shmem_bytes))
        .saturating_sub(before_all.file_bytes.saturating_add(before_all.shmem_bytes));
    assert!(
        mapped_growth < (spilled.spilled_bytes() as u64) / 2,
        "resident file-backed and shared memory grew {mapped_growth} B while \
         {} B went to the medium — the spill is mapping its files, which moves \
         bytes between RSS classes without giving any back",
        spilled.spilled_bytes(),
    );

    println!(
        "drop arm: freed {bare_freed} B, live -{bare_drop} B, ways back {bare_ways}/{HELD}\n\
         spill arm: freed {spill_freed} B, live -{spill_drop} B, on-disk {} B in {}, \
         ways back {spill_ways}/{HELD}\n\
         rss {} -> {} B (anon {} -> {}, file {} -> {}, shmem {} -> {})",
        spilled.spilled_bytes(),
        spilled.spilled_count(),
        before_all.rss_bytes,
        after_all.rss_bytes,
        before_all.anon_bytes,
        after_all.anon_bytes,
        before_all.file_bytes,
        after_all.file_bytes,
        before_all.shmem_bytes,
        after_all.shmem_bytes,
    );

    let _ = std::fs::remove_dir_all(&root);
}
