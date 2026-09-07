//! **The process census reconciles to the resident set, and moves when
//! memory moves.**
//!
//! Its own test binary because it installs the counting global allocator: a
//! `#[global_allocator]` is per-binary, and these assertions are about the
//! real one rather than a stand-in. That is the same reason
//! `squallar-overlays/tests/overlay_item_release.rs` is its own binary.
//!
//! # What this gate is for
//!
//! The census could always say what its families held. It could not say what
//! the process held, so nothing anywhere could say how much of the heap the
//! families accounted for — on native the residual had no denominator at all
//! and the line printed `residual unknown`. Every assertion here is about a
//! figure that did not exist before, and each is written so that it **fails
//! on a healthy input that resembles the defect** as well as on the defect:
//! a partition that does not close, a floor that is zero, a reading that
//! does not move.
//!
//! # One test, on purpose
//!
//! The allocator's counters and the census's levels are process-global and
//! the harness runs a binary's tests on several threads, so two tests moving
//! them would race each other's arithmetic. `squallar-alloc`'s own suite says
//! the same thing for the same reason.

#[global_allocator]
static ALLOCATOR: squallar_alloc::Counting = squallar_alloc::Counting;

use squallar_egui::heap_census::{census, process_census, process_line, publish_resident};

/// Big enough that nothing else this binary does is mistaken for it, and
/// small enough to be polite on a loaded box.
const BLOCK: usize = 256 << 20;

/// Pages are handed out lazily, so a block that is never written is virtual
/// and never appears in RSS. Touching one byte per page is what makes the
/// allocation resident — and makes this a test of the resident set rather
/// than of the address space.
fn make_resident(block: &mut [u8]) {
    for page in block.chunks_mut(4096) {
        page[0] = 1;
    }
}

#[test]
fn the_process_census_partitions_names_a_floor_and_follows_a_real_allocation() {
    // ---- 1. The kernel's own split is a partition, and we check it. -------
    let Some(before) = squallar_alloc::process::resident() else {
        // No `/proc`: there is nothing to reconcile and saying so is the
        // honest outcome. The parsers are tested on every target in
        // `squallar-alloc`; this binary tests the live reading.
        eprintln!("no /proc on this target; the live-reading gate does not apply");
        return;
    };
    assert!(
        before.partitions(),
        "VmRSS {} B is not RssAnon {} B + RssFile {} B + RssShmem {} B",
        before.rss_bytes,
        before.anon_bytes,
        before.file_bytes,
        before.shmem_bytes,
    );

    // ---- 2. The mapping walk is a partition too, by construction. --------
    let walk = squallar_alloc::process::breakdown().expect("/proc/self/smaps reads where status does");
    assert!(
        walk.partitions(),
        "the eight classes summed to {} B against the walk's own {} B: {walk:?}",
        walk.parts_total(),
        walk.rss_bytes,
    );

    // ---- 3. The non-heap floor is real and is not the whole process. -----
    // This is the term a 250 MiB budget owes before a byte of weather data,
    // and the assertion is two-sided on purpose: a zero would mean the
    // classifier never matched a mapping, and a floor equal to RSS would
    // mean it matched all of them.
    assert!(
        walk.non_heap_bytes() > 0,
        "a mapped ELF with libraries read a zero non-heap floor; the \
         classifier matched nothing"
    );
    assert!(
        walk.non_heap_bytes() < walk.rss_bytes,
        "the non-heap floor swallowed the whole resident set"
    );

    // ---- 4. The two readings describe the same process. ------------------
    let (lo, hi) = (
        before.rss_bytes.min(walk.rss_bytes),
        before.rss_bytes.max(walk.rss_bytes),
    );
    assert!(
        hi - lo < hi / 10,
        "status says {} B resident, the mapping walk says {} B",
        before.rss_bytes,
        walk.rss_bytes,
    );

    // ---- 5. And now the whole point: it MOVES. ---------------------------
    // A counter that cannot be shown to follow what it measures is not
    // evidence. Both figures must rise by about the block: `live` because
    // the allocator granted it, RSS because the pages were touched.
    let live_before = squallar_alloc::live_bytes().expect("this binary installed the counter");
    let mut block = vec![0u8; BLOCK];
    make_resident(&mut block);
    let live_held = squallar_alloc::live_bytes().expect("still counting");
    let held = squallar_alloc::process::resident().expect("still readable");

    assert!(
        live_held >= live_before + BLOCK as u64,
        "a {BLOCK} B grant moved live bytes only from {live_before} to {live_held}"
    );
    // RSS is the kernel's, and other threads on a loaded box move it too, so
    // this is a bound rather than an equality — but three quarters of a
    // 256 MiB block is far outside anything the harness does on its own, and
    // a counter that did not follow at all would read zero here.
    let rss_rise = held.rss_bytes.saturating_sub(before.rss_bytes);
    assert!(
        rss_rise > (BLOCK as u64) * 3 / 4,
        "touching {BLOCK} B moved the resident set only {rss_rise} B \
         ({} B to {} B); the reading does not follow the process",
        before.rss_bytes,
        held.rss_bytes,
    );
    assert!(held.partitions(), "the split stopped closing under load");

    // ---- 6. And it comes back down. --------------------------------------
    // The direction that matters for a campaign: a figure that only rises is
    // a high-water mark, which is the thing `live_bytes` exists not to be.
    drop(block);
    let after = squallar_alloc::live_bytes().expect("still counting");
    assert!(
        after < live_held,
        "the free did not bring live bytes back down: {live_held} then {after}"
    );

    // ---- 7. The published line carries the real figures. -----------------
    // "The instrument ran" proven by a positive match on the content, never
    // by the absence of an error.
    // A FRESH walk: the one at step 2 was taken before the block existed,
    // and publishing that beside a resident reading taken after it would put
    // two different instants on one line. The counts on the line say how many
    // of each reading has landed, but they cannot make two instants into one
    // - so the line is composed from readings taken together.
    let walk = squallar_alloc::process::breakdown().expect("still readable");
    publish_resident(after, Some(squallar_alloc::process::resident().expect("still readable")));
    squallar_egui::heap_census::publish_breakdown(&walk);
    let p = process_census();
    assert!(p.sampled() && p.walked(), "the publish did not land");
    let said = process_line(&census(), &p, "test");
    assert!(said.contains(&format!("live {after} B")), "{said}");
    assert!(
        said.contains(&format!("non-heap {} B", walk.non_heap_bytes())),
        "{said}"
    );
    assert!(
        said.contains("thp") && said.contains("not in the sum"),
        "the overlapping term must be marked wherever it is printed: {said}"
    );
    // Printed so a run of this gate is itself a reading of the machine it
    // ran on.
    eprintln!("{said}");
}
