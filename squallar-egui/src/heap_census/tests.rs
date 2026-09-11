//! The census's arithmetic and its one line. Host tests: nothing here needs a
//! browser, and the line is the same line the wasm hook writes.

use super::*;

/// A census with one distinct figure per family, so a field swapped for its
/// neighbour in `census()` or in the line fails rather than reading the same.
fn distinct() -> Census {
    Census {
        loop_scan_bytes: 1,
        loop_archive_bytes: 134_217_728,
        loop_l3_bytes: 2,
        still_l3_bytes: 67_108_864,
        still_scan_bytes: 4,
        derive_memo_bytes: 8,
        loop_frame_scan_bytes: 16,
        // **Not a power of two, and the one figure here that cannot be
        // one.** Every other row is a distinct bit so the sum is `2^29 - 1`
        // and a field swapped for its neighbour fails; this row is a
        // CORRECTION to the radar families and is bounded by them, so a bit
        // above `chunk feed`'s would saturate `radar_ceiling` to zero and the
        // derivation below would be about a clamp rather than about the
        // arithmetic. 3 is distinct from every other figure here, which is
        // what the run exists to buy.
        radar_shared_bytes: 3,
        base_skeleton_bytes: 5,
        render_cache_bytes: 32,
        cached_render_bytes: 16_777_216,
        raster_shared_bytes: 33_554_432,
        overlay_grid_bytes: 128,
        overlay_item_bytes: 256,
        overlay_parked_bytes: 512,
        loop_frame_bytes: 1024,
        overlay_reply_bytes: 1 << 28,
        upload_pending_bytes: 2048,
        tile_body_bytes: 4096,
        tile_parsed_bytes: 8192,
        tile_cache_bytes: 16384,
        loan_outstanding_bytes: 32768,
        volume_store_bytes: 65536,
        job_in_flight_bytes: 131_072,
        deferred_drop_bytes: 262_144,
        tile_mesh_bytes: 524_288,
        render_pool_bytes: 1_048_576,
        render_in_flight_bytes: 2_097_152,
        gpu_texture_bytes: 4_194_304,
        chunk_feed_bytes: 8_388_608,
        font_atlas_bytes: 64,
    }
}

/// The resident total is every family that lives on this heap and **not** the
/// two GPU ones — the tile meshes and the resident textures. Powers of two
/// make the omission unambiguous: a total that swept either in could only be
/// that one bug, and the figure names which.
#[test]
fn the_resident_total_leaves_the_gpu_families_out() {
    let c = distinct();
    // One distinct power of two per family, so the sum of them all is
    // `2^29 - 1`. `overlay pictures` was deleted and its `2^6` left a gap,
    // which stood here as an explicit `- 64` so the derivation named the
    // family that had left rather than hiding it in a literal. `font atlas`
    // has now taken that vacant `2^6`, so the gap closes and the subtraction
    // goes: the run is contiguous again. `overlay replies` extends it by one
    // at `2^28`.
    //
    // The exponent moved 27 -> 28 when `loop archives` landed and took `2^27`,
    // then 28 -> 30 when those two landed. **`overlay replies` was written
    // against 2^27 too, on a branch that did not yet have `loop archives`, and
    // git merged the two silently** — they are additions in different hunks,
    // so nothing conflicted and the fixture briefly had two families sharing
    // one figure, which is the one thing it exists to prevent. What caught it
    // was this assertion, by exactly the duplicated 2^27. A distinct figure
    // per family is not decoration: it is the only reason a merge like that
    // fails loudly instead of quietly making the sum right for the wrong
    // reason.
    //
    // The exclusions are NAMED rather than spelled as their powers: a literal
    // here goes on passing while pointing at the wrong family after any
    // renumber, which is exactly what a fixture like this exists to catch.
    let every_family = (1u64 << 29) - 1;
    assert_eq!(
        c.resident_total(),
        every_family + c.base_skeleton_bytes
            - c.tile_mesh_bytes
            - c.gpu_texture_bytes
            - c.raster_shared_bytes
            - c.radar_shared_bytes,
        "the resident total swept in a family that holds nothing: the two GPU \
         ones (it is a residual against a linear-memory `byteLength`, which no \
         device byte is on) or one of the two shared terms, each of which is \
         one allocation two families counted and not a third copy of it. \
         `radar shared` is subtracted rather than merely left out, because \
         `resident_total` folds the radar families in through \
         `radar_ceiling`, which has already taken it off their sum",
    );
    assert_eq!(c.radar_total(), 1 + 2 + 4 + 8 + 16 + 8_388_608);
}

/// The residual is the reading less the families, and `None` — not zero, and
/// not a wrapped `u64` — where the families price above the reading.
#[test]
fn the_residual_is_against_a_real_reading_and_refuses_to_wrap() {
    let c = distinct();
    let total = c.resident_total();
    assert_eq!(c.residual(total + 1_000), Some(1_000));
    assert_eq!(c.residual(total), Some(0));
    assert_eq!(
        c.residual(total - 1),
        None,
        "a shortfall wrapped instead of refusing"
    );
}

/// Every family is a level: set replaces, it does not accumulate. The whole
/// instrument is wrong if a second publish of the same figure doubles it.
#[test]
fn a_level_is_set_and_not_added() {
    reset();
    set_loop_scan_bytes(500);
    set_loop_scan_bytes(500);
    assert_eq!(census().loop_scan_bytes, 500);
    set_loop_scan_bytes(0);
    assert_eq!(
        census().loop_scan_bytes,
        0,
        "a level could not go back down"
    );
    reset();
}

/// The line names every family, its own total, the reading it is against and
/// the residual — and the GPU family is on it but marked out of the sum.
#[test]
fn the_line_names_every_family_and_its_denominator() {
    let c = distinct();
    let said = line(&c, Some(c.resident_total() + 900_000_000), "page");
    for field in [
        "loop scans 1 B",
        "loop archives 134217728 B",
        "loop l3 2 B",
        "still scans 4 B",
        "derive memo 8 B",
        "loop frame scans 16 B",
        "chunk feed 8388608 B",
        "radar shared 3 B",
        "base skeletons 5 B",
        "render cache 32 B",
        "overlay grids 128 B",
        "overlay items 256 B",
        "overlay parked 512 B",
        "loop frames 1024 B",
        "upload pending 2048 B",
        "tile bodies 4096 B",
        "tile parsed 8192 B",
        "tile cache 16384 B",
        "loans out 32768 B",
        "volume store 65536 B",
        "jobs in flight 131072 B",
        "deferred drops 262144 B",
        "font atlas 64 B",
        "render pools 1048576 B",
        "renders in flight 2097152 B",
        "overlay replies 268435456 B",
    ] {
        assert!(said.contains(field), "{field} missing from {said}");
    }
    assert!(said.starts_with("heap census (page): "), "{said}");
    assert!(said.contains("residual 900000000 B"), "{said}");
    assert!(
        said.contains("tile meshes 524288 B, gpu textures 4194304 B (GPU, not in the total)"),
        "both GPU families must be on the line, after the residual and marked \
         out of the sum: {said}"
    );
}

/// An unread heap says so instead of printing a residual against nothing.
/// The hook's own case: `memory_bytes` is an `Option` because the cast can
/// fail, and a census that invented a denominator there would be the exact
/// defect this whole lane is chasing.
#[test]
fn an_unread_heap_prints_no_residual() {
    let said = line(&distinct(), None, "page");
    assert!(said.contains("unread linear, residual unknown"), "{said}");
    assert!(!said.contains("residual 0"), "{said}");
}

/// The line fits the fixed buffer the allocation-error hook writes it into,
/// at the widest figures a `u64` can hold. The hook cannot allocate, so a
/// line that outgrew its buffer would be silently cut at the very moment it
/// is the only evidence there is.
#[test]
fn the_widest_line_fits_the_hooks_buffer() {
    let widest = Census {
        loop_scan_bytes: u64::MAX,
        loop_archive_bytes: u64::MAX,
        loop_l3_bytes: u64::MAX,
        still_l3_bytes: u64::MAX,
        still_scan_bytes: u64::MAX,
        derive_memo_bytes: u64::MAX,
        loop_frame_scan_bytes: u64::MAX,
        radar_shared_bytes: u64::MAX,
        base_skeleton_bytes: u64::MAX,
        render_cache_bytes: u64::MAX,
        cached_render_bytes: u64::MAX,
        raster_shared_bytes: u64::MAX,
        overlay_grid_bytes: u64::MAX,
        overlay_item_bytes: u64::MAX,
        overlay_parked_bytes: u64::MAX,
        loop_frame_bytes: u64::MAX,
        overlay_reply_bytes: u64::MAX,
        upload_pending_bytes: u64::MAX,
        tile_body_bytes: u64::MAX,
        tile_parsed_bytes: u64::MAX,
        tile_cache_bytes: u64::MAX,
        tile_mesh_bytes: u64::MAX,
        gpu_texture_bytes: u64::MAX,
        chunk_feed_bytes: u64::MAX,
        volume_store_bytes: u64::MAX,
        loan_outstanding_bytes: u64::MAX,
        job_in_flight_bytes: u64::MAX,
        deferred_drop_bytes: u64::MAX,
        render_pool_bytes: u64::MAX,
        render_in_flight_bytes: u64::MAX,
        font_atlas_bytes: u64::MAX,
    };
    // All three residual arms, because the widest is not the obvious one: a
    // reading of `u64::MAX` against saturated families prints `residual 0 B`,
    // a reading one below it prints `residual none (families price above
    // it)`, 27 bytes wider, and an unread heap prints no figure at all.
    let arms = [
        line(&widest, Some(u64::MAX), "rasterization worker"),
        line(&widest, Some(u64::MAX - 1), "rasterization worker"),
        line(&widest, None, "rasterization worker"),
    ];
    let said = arms.iter().max_by_key(|s| s.len()).expect("three arms");
    assert!(
        said.contains("residual none (families price above it)"),
        "the widest arm is no longer the `none` one; re-derive the doc's arithmetic: {said}"
    );
    assert!(
        said.len() <= CENSUS_LINE_CAPACITY,
        "the widest census line is {} bytes, past the hook's {CENSUS_LINE_CAPACITY}",
        said.len()
    );
    // And the constant is the widest line EXACTLY, so its doc's arithmetic
    // stays a derivation and not a figure with unstated headroom.
    assert_eq!(
        said.len(),
        CENSUS_LINE_CAPACITY,
        "the widest census line is {} bytes; re-derive CENSUS_LINE_CAPACITY",
        said.len()
    );
}

// ---------------------------------------------------------------------------
// The de-duplicated bounds, and the process denominator.
// ---------------------------------------------------------------------------

/// **Serialises every test below that touches the process-global levels.**
///
/// The process denominator is a set of statics and the harness runs this
/// binary's tests on several threads, so two of these would each read the
/// other's writes — and a `reset()` between them would race the assertions
/// rather than isolate them. A poisoned lock is read as a live one: a
/// panicking test has already failed and must not cascade into its siblings.
static PROCESS_STATIC: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn process_guard() -> std::sync::MutexGuard<'static, ()> {
    PROCESS_STATIC
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// **The radar floor is the largest single holder, not the sum.**
///
/// The five holders share `Arc`s, so the union they cover is at least the
/// biggest of them and at most their total. A floor that returned the sum
/// would be the upper bound wearing the other name, which is the one mistake
/// that would make the "de-duplicated" claim false.
#[test]
fn the_radar_floor_is_the_largest_holder_and_the_total_is_the_sum() {
    let c = distinct();
    assert_eq!(
        c.radar_total(),
        1 + 2 + 4 + 8 + 16 + 8_388_608,
        "the upper bound"
    );
    assert_eq!(
        c.radar_floor(),
        8_388_608,
        "the floor is the largest holder"
    );
    assert!(c.radar_floor() < c.radar_total());

    // One holder and nothing else: the two ends meet, because there is no
    // overlap to be uncertain about.
    let lone = Census {
        loop_scan_bytes: 900,
        ..Census::default()
    };
    assert_eq!(lone.radar_floor(), 900);
    assert_eq!(lone.radar_total(), 900);
}

/// **The floor differs from the total by exactly the radar overlap**, and by
/// nothing else — every other family is documented disjoint, so swapping the
/// radar end must not move any of them.
#[test]
fn the_resident_floor_moves_only_the_shared_families() {
    let c = distinct();
    assert_eq!(
        c.resident_total() - c.resident_floor(),
        (c.radar_ceiling() - c.radar_floor()) + c.raster_shared_bytes,
        "the floor moved a family that is not shared"
    );
    assert!(c.resident_floor() < c.resident_total());

    // With nothing shared at either overlap the two ends are identical: one
    // raster family holding bytes no other family names moves neither end.
    let unshared = Census {
        render_cache_bytes: 5_000,
        ..Census::default()
    };
    assert_eq!(unshared.resident_floor(), unshared.resident_total());
}

/// **The raster pair's two ends.** `render cache` and `cached renders` hold
/// `Arc`s of the same images, so their sum is an upper bound and the floor is
/// that sum less what they share — measured, not bounded, which is what
/// separates this pair from the radar families.
#[test]
fn the_raster_floor_takes_the_shared_arcs_out() {
    let both = Census {
        render_cache_bytes: 5_000,
        cached_render_bytes: 5_000,
        raster_shared_bytes: 5_000,
        ..Census::default()
    };
    assert_eq!(both.raster_total(), 10_000, "the upper bound is the sum");
    assert_eq!(
        both.raster_floor(),
        5_000,
        "the floor kept the buffer twice"
    );
    assert_eq!(
        both.resident_total() - both.resident_floor(),
        5_000,
        "the resident range did not carry the raster sharing"
    );
}

/// **The case where the correction must dominate**: nothing shared, and the
/// two ends must not move at all. Without this the subtraction above is
/// satisfied by a floor that always halves the pair.
#[test]
fn two_raster_families_sharing_nothing_move_neither_end() {
    let disjoint = Census {
        render_cache_bytes: 5_000,
        cached_render_bytes: 7_000,
        raster_shared_bytes: 0,
        ..Census::default()
    };
    assert_eq!(disjoint.raster_total(), 12_000);
    assert_eq!(
        disjoint.raster_floor(),
        12_000,
        "a floor ate unshared bytes"
    );
    assert_eq!(disjoint.resident_floor(), disjoint.resident_total());
}

/// It refuses to wrap: a `raster shared` larger than the pair it corrects is a
/// torn read across two adjacent instants, not a negative heap.
#[test]
fn the_raster_floor_refuses_to_wrap() {
    let torn = Census {
        render_cache_bytes: 1_000,
        cached_render_bytes: 0,
        raster_shared_bytes: u64::MAX,
        ..Census::default()
    };
    assert_eq!(torn.raster_floor(), 0);
    assert_eq!(torn.resident_floor(), 0);
}

/// **`unaccounted` is a range, and its ends come from the right ends of the
/// census.** The least that can be unaccounted is measured against the
/// families' upper bound; the most, against their floor. Getting these the
/// wrong way round would report a narrower gap than the evidence supports,
/// which is the direction that hides bytes.
#[test]
fn the_unaccounted_range_runs_from_the_upper_bound_to_the_floor() {
    let c = distinct();
    let live = c.resident_total() + 1_000;
    let (least, most) = c.unaccounted(live);
    assert_eq!(least, Some(1_000), "least = live - upper bound");
    assert_eq!(
        most,
        Some(1_000 + (c.resident_total() - c.resident_floor())),
        "most = live - floor"
    );
    assert!(most > least, "the range is the wrong way round");
}

/// **It refuses to wrap at both ends.** Families pricing above `live` is a
/// real state — the radar upper bound double-counts by design — and a
/// wrapped `u64` there would print as an enormous unaccounted figure at
/// exactly the moment the census is over-counting.
#[test]
fn the_unaccounted_range_refuses_to_wrap() {
    let c = distinct();
    // Above the floor but below the upper bound: one end is real, one is not.
    let between = c.resident_floor() + 1;
    let (least, most) = c.unaccounted(between);
    assert_eq!(least, None, "the upper-bound end wrapped");
    assert_eq!(most, Some(1));

    // Below both.
    let (least, most) = c.unaccounted(0);
    assert_eq!((least, most), (None, None));
}

/// A never-published process census is **unread, not zero** — the same
/// distinction `live_bytes` makes. A reader shown `rss 0 B` beside a running
/// process would believe it.
#[test]
fn an_unpublished_process_census_reads_as_unread() {
    let _serialised = process_guard();
    process_levels::reset();
    let p = process_census();
    assert!(
        !p.sampled(),
        "nothing was published, so nothing was sampled"
    );
    assert!(!p.walked());
    let said = process_line(&Census::default(), &p, "page");
    assert!(said.contains("rss unread"), "{said}");
    assert!(
        !said.contains("rss 0 B"),
        "an unread rss printed as a zero: {said}"
    );
    process_levels::reset();
}

/// A cheap sample lands and the line reports it — and the breakdown is still
/// **unwalked**, because the two readings are taken by different callers at
/// different costs and one arriving must never imply the other.
#[test]
fn a_cheap_sample_lands_without_implying_a_walk() {
    let _serialised = process_guard();
    process_levels::reset();
    publish_resident(
        1_000,
        Some(squallar_alloc::process::Resident {
            rss_bytes: 3_000,
            anon_bytes: 2_000,
            file_bytes: 1_000,
            shmem_bytes: 0,
            threads: 141,
        }),
    );
    let p = process_census();
    assert!(p.sampled());
    assert!(!p.walked(), "a cheap sample claimed a walk");
    assert_eq!(p.samples, 1);
    assert_eq!(p.rss_over_live(), Some(2_000), "rss - live");

    let said = process_line(&Census::default(), &p, "page");
    assert!(said.contains("live 1000 B"), "{said}");
    assert!(said.contains("rss 3000 B"), "{said}");
    assert!(said.contains("threads 141"), "{said}");
    assert!(said.contains("rss over live 2000 B"), "{said}");
    assert!(said.contains("breakdown unwalked"), "{said}");
    process_levels::reset();
}

/// The walk lands and the line names every class — **with the huge-page term
/// marked as overlapping**, because a reader who adds it to the arena term
/// has counted the same bytes twice and that is the exact confusion this
/// figure exists to prevent.
#[test]
fn a_walk_names_every_class_and_marks_the_overlapping_one() {
    let _serialised = process_guard();
    process_levels::reset();
    publish_resident(
        1_000,
        Some(squallar_alloc::process::Resident {
            rss_bytes: 3_000,
            anon_bytes: 2_000,
            file_bytes: 1_000,
            shmem_bytes: 0,
            threads: 9,
        }),
    );
    publish_breakdown(&squallar_alloc::process::Breakdown {
        rss_bytes: 3_000,
        main_heap_bytes: 100,
        arena_bytes: 700,
        arenas: 31,
        stack_bytes: 50,
        anon_other_bytes: 1_150,
        code_bytes: 400,
        file_other_bytes: 300,
        device_bytes: 280,
        kernel_bytes: 20,
        thp_bytes: 1_800,
        mappings: 42,
    });
    let p = process_census();
    assert!(p.walked());
    assert_eq!(p.non_heap, 400 + 300 + 280 + 20, "code+file+device+kernel");
    assert_eq!(p.arenas, 31);

    let said = process_line(&Census::default(), &p, "page");
    for field in [
        "main heap 100 B",
        "arenas 31 at 700 B",
        "stacks 50 B",
        "anon other 1150 B",
        "non-heap 1000 B",
        "thp 1800 B (overlaps the anon terms, not in the sum)",
        "walks 1",
    ] {
        assert!(said.contains(field), "{field} missing from {said}");
    }
    process_levels::reset();
}

/// **A level is set, not added** — for the process figures too. The sample
/// and walk COUNTS are the deliberate exception, and they must go up, because
/// they are what tells a reader the reading is fresh.
#[test]
fn process_levels_are_set_while_the_sample_counts_accumulate() {
    let _serialised = process_guard();
    process_levels::reset();
    let reading = squallar_alloc::process::Resident {
        rss_bytes: 500,
        anon_bytes: 500,
        file_bytes: 0,
        shmem_bytes: 0,
        threads: 1,
    };
    publish_resident(10, Some(reading));
    publish_resident(10, Some(reading));
    let p = process_census();
    assert_eq!(p.rss, 500, "a level accumulated instead of being set");
    assert_eq!(p.samples, 2, "the sample count did not advance");
    process_levels::reset();
}

/// A resident reading that could not be taken publishes `live` and **does
/// not advance the sample count** — so a native arm whose `/proc` read failed
/// is `unread` rather than showing the last reading as if it were current.
#[test]
fn a_failed_resident_reading_does_not_count_as_a_sample() {
    let _serialised = process_guard();
    process_levels::reset();
    publish_resident(77, None);
    let p = process_census();
    assert_eq!(p.live, 77, "live is known even when /proc is not");
    assert!(!p.sampled(), "a failed read was counted as a sample");
    process_levels::reset();
}

/// The process line fits a fixed buffer at the widest figures a `u64` can
/// hold, the way the census line does — and the constant is that width
/// EXACTLY, so a field added without re-deriving it fails here.
#[test]
fn the_widest_process_line_fits_its_buffer() {
    let widest = ProcessCensus {
        live: u64::MAX,
        live_peak: u64::MAX,
        live_peak_large: u64::MAX,
        rss: u64::MAX,
        anon: u64::MAX,
        file: u64::MAX,
        shmem: u64::MAX,
        threads: u64::MAX,
        samples: u64::MAX,
        main_heap: u64::MAX,
        arena: u64::MAX,
        arenas: u64::MAX,
        stack: u64::MAX,
        anon_other: u64::MAX,
        non_heap: u64::MAX,
        thp: u64::MAX,
        walks: u64::MAX,
    };
    // Both unaccounted arms, because the `none` one is the wider prose.
    let arms = [
        process_line(&Census::default(), &widest, "rasterization worker"),
        process_line(&distinct(), &widest, "rasterization worker"),
    ];
    let said = arms.iter().max_by_key(|s| s.len()).expect("two arms");
    assert!(
        said.len() <= PROCESS_LINE_CAPACITY,
        "the widest process line is {} bytes, past {PROCESS_LINE_CAPACITY}",
        said.len()
    );
    assert_eq!(
        said.len(),
        PROCESS_LINE_CAPACITY,
        "the widest process line is {} bytes; re-derive PROCESS_LINE_CAPACITY",
        said.len()
    );
}

/// **A process that never installed the counting allocator says so**, rather
/// than printing an empty histogram as if it had measured one.
///
/// This test binary does not declare `squallar_alloc::Counting`, so nothing
/// has ever reached `note_large_grant` here and `none` is the true answer.
/// The line's own honesty rule, the same one `rss unread` and
/// `breakdown unwalked` carry: a zero that means "not counted here" must not
/// read as a zero that means "nothing happened".
#[test]
fn a_process_with_no_counting_allocator_reports_no_large_grants() {
    let said = large_grants_line("page");
    assert_eq!(
        said, "large grants (page): none",
        "this binary installs no counting allocator, so the histogram has \
         seen nothing and must say so rather than print buckets",
    );
}

/// **`live peak` is on the process line and is its own figure**, not a
/// restatement of `live`.
#[test]
fn the_process_line_carries_the_peak_beside_the_live_figure() {
    let p = ProcessCensus {
        live: 111,
        live_peak: 222,
        live_peak_large: 3,
        ..Default::default()
    };
    let said = process_line(&Census::default(), &p, "page");
    assert!(
        said.contains("live 111 B, live peak 222 B, peak large blocks 3"),
        "the peak is the term that says whether a residency lever reaches a \
         wasm page's death, and it is not on the line: {said}",
    );
}

/// **The `chunk feed` family reads radar's own level**, and is not wired to a
/// constant.
///
/// A read-through family has no setter, so nothing in this crate's tests
/// touches its publisher — and every assertion elsewhere in this file is
/// against a HAND-BUILT `Census`, which never runs `census()` at all. A
/// tamper replacing the expression with `0u64` passed all sixteen of them.
/// This moves the level radar actually maintains and reads the family back.
#[test]
fn the_chunk_feed_family_reads_radars_own_level() {
    let _serialised = process_guard();
    let before = census().chunk_feed_bytes;
    // A figure no real assembler would produce, so a stale reading cannot be
    // mistaken for this one.
    const MARK: u64 = 0x5EED_1234;
    squallar_radar::chunks::force_feed_level(0, MARK);
    assert_eq!(
        census().chunk_feed_bytes,
        before + MARK,
        "the census did not see radar's level move"
    );
    squallar_radar::chunks::force_feed_level(MARK, 0);
    assert_eq!(
        census().chunk_feed_bytes,
        before,
        "the level did not come back"
    );
}

/// **The glyph atlas family is a real reading of a real atlas, not a zero.**
///
/// The census's own rule for a readable zero (see [`super`]'s note on
/// `deltas`) applies here too: `font atlas` reads 0 B on a process that has
/// laid out no text and 0 B on a publisher that has silently stopped, and
/// nothing else in the census can tell those apart. What separates them is
/// the arithmetic — four bytes a texel of `Fonts::font_image_size` — so it is
/// pinned against an atlas this test grows itself.
///
/// Denominator: an epaint `Fonts` built the way `egui::Context` builds one
/// (stock `FontDefinitions`, `TextOptions` with the width under test), driven
/// with printable ASCII. Host bytes; the device copy is `gpu textures`.
#[test]
fn the_font_atlas_family_prices_four_bytes_a_texel_of_the_real_atlas() {
    let text: String = (0x20u8..=0x7eu8).map(|b| b as char).collect();
    let mut fonts = egui::epaint::text::Fonts::new(
        egui::epaint::text::TextOptions {
            max_texture_side: 4096,
            ..Default::default()
        },
        egui::epaint::text::FontDefinitions::default(),
    );

    // A fresh atlas is 32 rows tall whatever is in it, so read it before and
    // after: a family that ignored the atlas entirely would match one of the
    // two by luck and cannot match both. Priced through the publisher's own
    // `atlas_bytes` rather than a copy of its multiplication, so a publisher
    // that started charging three bytes a texel fails here.
    let priced = |f: &egui::epaint::text::Fonts| atlas_bytes(f.font_image_size());
    let fresh = priced(&fonts);
    assert_eq!(fresh, 4096 * 32 * 4, "epaint opens the atlas at 32 rows");

    // Enough distinct sizes to force it past its opening height.
    {
        let mut view = fonts.with_pixels_per_point(2.0);
        for step in 0..24 {
            let size = 9.0 + step as f32;
            let _ = view.layout(
                text.clone(),
                egui::FontId::proportional(size),
                egui::Color32::WHITE,
                f32::INFINITY,
            );
        }
    }
    let grown = priced(&fonts);
    assert!(
        grown > fresh,
        "the fixture did not grow the atlas ({fresh} B -> {grown} B), so it \
         cannot say the family tracks a growth"
    );

    // **Read off a `Census` of this test's own and never off the process
    // globals.** `font_atlas_bytes` is published by `Gui::frame`, on every
    // frame, unconditionally — so every frame-driving test in this binary is
    // a concurrent writer of the very level a `set` here would then read
    // back, and the harness runs them on 32 threads. No mutex in this file
    // can reach that writer, so the observation is made immune instead of
    // isolated: the arithmetic under test is `atlas_bytes`, and the two
    // transport claims a `census()` round trip would have added are already
    // gated where nothing global can move them —
    // [`a_level_is_set_and_not_added`] for set-replaces-not-adds, and
    // [`the_line_names_every_family_and_its_denominator`] for `font atlas`
    // reaching the printed line, both against a hand-built `Census`.
    let c = Census {
        font_atlas_bytes: grown,
        ..Default::default()
    };
    assert!(
        line(&c, None, "test").contains(&format!("font atlas {grown} B")),
        "the grown atlas does not reach the line the allocation-error hook \
         prints"
    );
    assert_eq!(
        c.resident_total(),
        grown,
        "a real atlas reading is host bytes and must be inside the page total"
    );
}

/// The atlas is **in** the page total, unlike the two GPU families beside it.
///
/// `gpu textures` and `tile meshes` are carried outside
/// [`Census::resident_total`] because no device byte is on the reading the
/// residual is taken against. This one is a `Vec<Color32>` on the same linear
/// memory as every other family here, so leaving it out would put it straight
/// back into the residual it was added to take out of.
#[test]
fn the_font_atlas_is_inside_the_page_total() {
    let mut c = Census::default();
    assert_eq!(c.resident_total(), 0);
    c.font_atlas_bytes = 4_194_304;
    assert_eq!(
        c.resident_total(),
        4_194_304,
        "the glyph atlas is host bytes and must be inside the total"
    );
}

/// **The narrowing row's exact shape**, pinned at the producer.
///
/// The enumeration gate in `app_render/frame_telemetry_line_tests.rs` is blind
/// to this row by construction — its own doc says it does not count the
/// instance-scoped `<words> (<name>):` families that this module writes — so
/// the row is pinned here instead, by its rendered text and its figure count.
/// Without this, a field added or dropped would change what every reader parses
/// and nothing in the tree would say so.
#[test]
fn the_grid_narrowing_row_keeps_its_shape() {
    let line = super::grid_narrowing_line("page");
    assert!(
        line.starts_with("grid narrowing (page): "),
        "the row's prefix is what every reader keys on: {line}",
    );
    for key in [
        "offered ",
        "narrowed ",
        "refused ",
        "lossy ",
        "verified ",
        " points;",
        "wide ",
        "narrow ",
    ] {
        assert!(line.contains(key), "the row lost `{key}`: {line}");
    }
    // Seven figures, in the order the reader takes them. A field added or
    // dropped moves every later one, which is how a positional parser silently
    // splits a series into two instruments.
    let figures = line
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .count();
    assert_eq!(
        figures, 7,
        "the row must carry exactly seven figures — offered, narrowed, \
         refused, lossy, verified, wide, narrow: {line}",
    );
    // `wide` and `narrow` are the same grids priced twice and are never added;
    // both must be present as byte figures so a reader cannot mistake one for
    // a total.
    assert!(
        line.ends_with(" B"),
        "the row ends with the narrow byte figure: {line}",
    );
}
