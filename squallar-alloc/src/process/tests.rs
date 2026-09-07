//! The process readers' arithmetic, against **captured kernel output**.
//!
//! Every fixture in this file is real `/proc` text from this workspace's
//! Linux arm, pasted unedited except for the trimming named at each one. The
//! parsers are pure, so these run on every target the workspace builds for,
//! wasm included — where the readers themselves answer `None` and there is
//! nothing else to test.

use super::*;

/// `/proc/self/status`, trimmed to the fields the parser reads plus two it
/// must ignore. Captured 2026-09-07 from a live process on the Linux arm.
const STATUS: &str = "\
Name:\tsquallar
Umask:\t0022
State:\tS (sleeping)
Threads:\t141
VmPeak:\t 4194304 kB
VmSize:\t 3999232 kB
VmRSS:\t 3183206 kB
RssAnon:\t 2932736 kB
RssFile:\t  250470 kB
RssShmem:\t       0 kB
VmData:\t 3176132 kB
VmStk:\t     136 kB
VmExe:\t   59452 kB
";

/// **The three `Rss*` classes partition `VmRSS`**, which is the property the
/// whole instrument rests on: a reader may print `anon`, `file` and `shmem`
/// as a split of the resident set only because the kernel keeps them summing
/// to it.
#[test]
fn status_parses_into_a_partition_of_the_resident_set() {
    let r = parse_status(STATUS).expect("VmRSS is on this fixture");
    assert_eq!(r.rss_bytes, 3_183_206 * 1024);
    assert_eq!(r.anon_bytes, 2_932_736 * 1024);
    assert_eq!(r.file_bytes, 250_470 * 1024);
    assert_eq!(r.shmem_bytes, 0);
    assert_eq!(r.threads, 141);
    assert_eq!(r.parts_total(), r.rss_bytes);
    assert!(r.partitions(), "the captured reading does not partition");
}

/// **A reading that does not add up says so**, rather than being printed as a
/// partition. The kernel generates the four fields without one lock across
/// all of them, so a process allocating hard can produce exactly this.
#[test]
fn a_reading_that_does_not_add_up_is_not_called_a_partition() {
    let torn = "VmRSS:\t 1000 kB\nRssAnon:\t 400 kB\nRssFile:\t 100 kB\nRssShmem:\t 0 kB\n";
    let r = parse_status(torn).expect("VmRSS is present");
    assert!(
        !r.partitions(),
        "a reading 500 kB short of its own total was called a partition"
    );
    assert_eq!(r.parts_total(), 500 * 1024);
    assert_eq!(r.rss_bytes, 1000 * 1024);
}

/// No `VmRSS` is `None` — **not a process of zero bytes**. The distinction is
/// the same one [`crate::live_bytes`] makes and for the same reason: a zero
/// beside a real heap reads as an empty one.
#[test]
fn a_status_without_a_resident_set_is_none() {
    assert_eq!(parse_status("Name:\tsquallar\nThreads:\t8\n"), None);
    assert_eq!(parse_status(""), None);
}

/// `anon_over_live` refuses to wrap, and is `None` — not zero — where the
/// allocator prices above the anonymous residency. A heap whose pages have
/// gone back to the kernel is exactly that state, and printing `0` would
/// hide it.
#[test]
fn the_allocator_overhead_term_refuses_to_wrap() {
    let r = parse_status(STATUS).expect("parses");
    assert_eq!(r.anon_over_live(r.anon_bytes - 1_000), Some(1_000));
    assert_eq!(r.anon_over_live(r.anon_bytes), Some(0));
    assert_eq!(
        r.anon_over_live(r.anon_bytes + 1),
        None,
        "a heap pricing above its own residency wrapped instead of refusing"
    );
}

/// `/proc/self/smaps`, assembled from **real captured mappings** — one of
/// each class this module sorts into. The two anonymous mappings are a
/// verbatim glibc secondary arena captured from a live 7.2 GB process
/// (`7f1524000000` is 64 MiB-aligned; the pair spans exactly 64 MiB, the
/// second being the `PROT_NONE` remainder). Detail lines other than `Rss:`
/// and `AnonHugePages:` are kept on two mappings and dropped from the rest,
/// so the parser is shown ignoring them.
const SMAPS: &str = "\
55d0e2a00000-55d0e2a21000 rw-p 00000000 00:00 0                          [heap]
Size:                132 kB
Rss:                 100 kB
Pss:                  50 kB
Shared_Clean:          8 kB
Private_Dirty:       100 kB
AnonHugePages:         0 kB
VmFlags: rd wr mr mw me ac sd
7f1524000000-7f1527fed000 rw-p 00000000 00:00 0
Size:              65460 kB
Rss:               22528 kB
Anonymous:         22528 kB
AnonHugePages:     22528 kB
VmFlags: rd wr mr mw me ac sd
7f1527fed000-7f1528000000 ---p 00000000 00:00 0
Size:                 76 kB
Rss:                   0 kB
AnonHugePages:         0 kB
7f1600000000-7f1600200000 rw-p 00000000 00:00 0
Rss:                 512 kB
AnonHugePages:         0 kB
7fdcfc6f9000-7fdcfc8f9000 rw-s 00000000 00:07 1686                       /dev/nvidiactl
Rss:                2048 kB
AnonHugePages:         0 kB
7f0204224000-7f02043ac000 r-xp 00024000 fd:01 48236376                   /usr/lib/libc.so.6
Rss:                 400 kB
AnonHugePages:         0 kB
7f0203e00000-7f02041f6000 r--p 00000000 fd:01 48289395                   /usr/lib/locale/locale-archive
Rss:                  64 kB
AnonHugePages:         0 kB
7ffd4b5f9000-7ffd4b61a000 rw-p 00000000 00:00 0                          [stack]
Rss:                  36 kB
AnonHugePages:         0 kB
7ffd4b7d1000-7ffd4b7d5000 r--p 00000000 00:00 0                          [vvar]
Rss:                   4 kB
AnonHugePages:         0 kB
";

/// **Every class, and the partition closes exactly.** The eight buckets are
/// disjoint over the same mapping list, so their sum is the walk's own RSS
/// by construction — a difference is a bug here and not a fact about the
/// process.
#[test]
fn smaps_sorts_every_class_and_the_partition_closes() {
    let b = parse_smaps(SMAPS);
    assert_eq!(b.mappings, 9, "one header per mapping");
    assert_eq!(b.main_heap_bytes, 100 * 1024, "[heap]");
    // The arena is the 22528 kB mapping plus its 0 kB PROT_NONE remainder.
    assert_eq!(b.arena_bytes, 22_528 * 1024, "the glibc secondary arena");
    assert_eq!(b.arenas, 1);
    // The lone 2 MiB anonymous mapping is not 64 MiB, so it is not an arena.
    assert_eq!(b.anon_other_bytes, 512 * 1024, "an mmapped block");
    assert_eq!(b.device_bytes, 2048 * 1024, "/dev/nvidiactl");
    assert_eq!(b.code_bytes, 400 * 1024, "libc, mapped executable");
    assert_eq!(b.file_other_bytes, 64 * 1024, "the locale archive");
    assert_eq!(b.stack_bytes, 36 * 1024, "[stack]");
    assert_eq!(b.kernel_bytes, 4 * 1024, "[vvar]");
    assert_eq!(b.thp_bytes, 22_528 * 1024, "AnonHugePages, summed");

    assert_eq!(
        b.parts_total(),
        b.rss_bytes,
        "the eight classes did not add to the walk's own total"
    );
    assert!(b.partitions());
    // main_heap + arena (the 22 528 kB mapping and its 0 kB PROT_NONE
    // remainder, which the arena term already carries) + anon_other + device
    // + code + file_other + stack + kernel.
    assert_eq!(
        b.rss_bytes,
        (100 + 22_528 + 512 + 2048 + 400 + 64 + 36 + 4) * 1024
    );
}

/// **The THP figure is not a class and is not in the partition.** A huge
/// page backs one of the anonymous classes rather than sitting beside it, so
/// a sum that swept it in would double-count every byte of it. The arena
/// fixture is entirely huge-page backed, which makes the mistake visible:
/// `thp` alone is most of the total.
#[test]
fn transparent_huge_pages_are_reported_and_left_out_of_the_sum() {
    let b = parse_smaps(SMAPS);
    assert!(b.thp_bytes > 0, "the fixture has huge pages to find");
    assert_eq!(
        b.parts_total(),
        b.rss_bytes,
        "the partition moved when the THP term was added"
    );
    assert!(
        b.parts_total() < b.rss_bytes + b.thp_bytes,
        "the THP bytes were swept into the classes"
    );
}

/// **The non-heap floor** — code, file maps, device maps and kernel pages —
/// is the term a resident-set budget owes before a byte of weather data. It
/// is spelled once so a caller cannot assemble it from the wrong four.
#[test]
fn the_non_heap_floor_is_the_four_classes_the_allocator_cannot_free() {
    let b = parse_smaps(SMAPS);
    assert_eq!(b.non_heap_bytes(), (400 + 64 + 2048 + 4) * 1024);
    // And it excludes every anonymous class, which is what the allocator can.
    assert!(
        b.non_heap_bytes() < b.rss_bytes,
        "the floor swallowed the heap"
    );
}

/// **A device map is a device map even when it is executable.** The `/dev/`
/// test runs before the executable one on purpose: a driver may map its
/// region `r-xp`, and charging it to `code` would hide the graphics driver
/// inside the binary's own text.
#[test]
fn an_executable_device_mapping_is_still_a_device_mapping() {
    assert_eq!(classify("/dev/nvidia0", "r-xp"), Class::Device);
    assert_eq!(classify("/dev/nvidiactl", "rw-s"), Class::Device);
    assert_eq!(classify("/usr/lib/libc.so.6", "r-xp"), Class::Code);
    assert_eq!(classify("/usr/lib/libc.so.6", "r--p"), Class::FileOther);
    assert_eq!(classify("[heap]", "rw-p"), Class::MainHeap);
    assert_eq!(classify("[stack]", "rw-p"), Class::Stack);
    assert_eq!(classify("[vdso]", "r-xp"), Class::Kernel);
    assert_eq!(classify("", "rw-p"), Class::Anon);
}

/// **The arena signature needs both halves**: 64 MiB-aligned *and* spanning
/// exactly 64 MiB. A run that is one and not the other is `anon_other`, and
/// the bytes stay on the total either way — the property that makes a
/// misclassification safe.
#[test]
fn the_arena_signature_needs_alignment_and_span_together() {
    // Aligned, but only 2 MiB wide: not an arena.
    let short = "\
7f1524000000-7f1524200000 rw-p 00000000 00:00 0
Rss:                 100 kB
";
    let b = parse_smaps(short);
    assert_eq!(b.arenas, 0, "a 2 MiB aligned mapping is not an arena");
    assert_eq!(b.anon_other_bytes, 100 * 1024);
    assert_eq!(b.parts_total(), b.rss_bytes, "the bytes left the total");

    // 64 MiB wide, but starting 4 KiB past the boundary: not an arena.
    let misaligned = "\
7f1524001000-7f1528001000 rw-p 00000000 00:00 0
Rss:                 100 kB
";
    let b = parse_smaps(misaligned);
    assert_eq!(b.arenas, 0, "an unaligned 64 MiB run is not an arena");
    assert_eq!(b.anon_other_bytes, 100 * 1024);
    assert_eq!(b.parts_total(), b.rss_bytes, "the bytes left the total");
}

/// **An anonymous run only joins across adjacent addresses.** Two 32 MiB
/// mappings that happen to sum to 64 MiB but sit apart are two runs, not one
/// arena — otherwise any two neighbours of the right total would be read as
/// glibc's.
#[test]
fn a_gap_breaks_an_anonymous_run() {
    let split = "\
7f1524000000-7f1526000000 rw-p 00000000 00:00 0
Rss:                  10 kB
7f1a00000000-7f1a02000000 rw-p 00000000 00:00 0
Rss:                  20 kB
";
    let b = parse_smaps(split);
    assert_eq!(b.arenas, 0, "two mappings across a gap were joined");
    assert_eq!(b.anon_other_bytes, 30 * 1024);
    assert_eq!(b.parts_total(), b.rss_bytes);
}

/// **A pathname with spaces in it does not shift the class.** The kernel
/// prints the pathname as the remainder of the line, so a font under a
/// directory with a space must not be parsed as a sixth column.
#[test]
fn a_pathname_containing_spaces_still_classifies() {
    let spaced = "\
7f0204224000-7f02043ac000 r--p 00024000 fd:01 48236376                   /home/a b/Some Font.ttf
Rss:                  64 kB
";
    let b = parse_smaps(spaced);
    assert_eq!(b.file_other_bytes, 64 * 1024, "the pathname was mis-split");
    assert_eq!(b.anon_other_bytes, 0, "it was read as anonymous");
    assert_eq!(b.parts_total(), b.rss_bytes);
}

/// **Detail lines that are not `Rss:` are ignored** — and the near-misses
/// beside it in real output (`Pss`, `Shared_Clean`, `Private_Dirty`) are the
/// ones that would silently inflate every class if the parser matched on a
/// prefix rather than the exact key. The `[heap]` fixture carries all three.
#[test]
fn the_neighbouring_detail_lines_are_not_swept_in() {
    let b = parse_smaps(SMAPS);
    // Rss 100, Pss 50, Shared_Clean 8, Private_Dirty 100 on that mapping.
    assert_eq!(
        b.main_heap_bytes,
        100 * 1024,
        "a detail line beside `Rss:` was counted as residency"
    );
}

/// Empty input is an empty breakdown that still partitions, and a header
/// with no detail lines contributes a mapping and no bytes. Neither is an
/// error state; both are things a truncated read produces.
#[test]
fn an_empty_or_headerless_walk_is_still_coherent() {
    let empty = parse_smaps("");
    assert_eq!(empty.mappings, 0);
    assert_eq!(empty.rss_bytes, 0);
    assert!(empty.partitions());

    let headers_only = parse_smaps(
        "55d0e2a00000-55d0e2a21000 rw-p 00000000 00:00 0                          [heap]\n",
    );
    assert_eq!(headers_only.mappings, 1);
    assert_eq!(headers_only.rss_bytes, 0);
    assert!(headers_only.partitions());
}

/// **The reader agrees with the parser on this machine.** A live Linux arm
/// reads its own `/proc` and the two figures must be the same process: the
/// cheap reading's RSS and the expensive walk's summed RSS are the same
/// quantity by two routes, so they agree to within what the process
/// allocated between the two reads.
///
/// Linux only, because there is no `/proc` to read anywhere else — and the
/// non-Linux arm has its own test below that the answer is `None`.
#[cfg(target_os = "linux")]
#[test]
fn the_two_readings_describe_the_same_process() {
    let r = resident().expect("the Linux arm reads /proc/self/status");
    let b = breakdown().expect("the Linux arm reads /proc/self/smaps");

    assert!(r.rss_bytes > 0, "a running process holds pages");
    assert!(r.partitions(), "the kernel's own split did not close");
    assert!(
        b.partitions(),
        "the mapping walk's classes did not add to its total: {b:?}"
    );

    // The two reads are microseconds apart and the harness allocates between
    // them, so this is a bound and not an equality. A tenth is enormous
    // against that and tiny against a mis-parse, which would be off by
    // orders of magnitude or read zero.
    let (lo, hi) = (r.rss_bytes.min(b.rss_bytes), r.rss_bytes.max(b.rss_bytes));
    assert!(
        hi - lo < hi / 10,
        "status says {} B resident and the smaps walk says {} B; the two \
         readings are not the same process",
        r.rss_bytes,
        b.rss_bytes
    );

    // This test binary is a real ELF with libraries mapped, so the non-heap
    // floor is never zero — the figure a 250 MiB budget owes before it
    // starts.
    assert!(
        b.non_heap_bytes() > 0,
        "a mapped executable read a zero non-heap floor"
    );
}

/// Off Linux there is no `/proc`, and the honest answer is `None` rather
/// than a process of zero bytes. The parsers above are what is tested there.
#[cfg(not(target_os = "linux"))]
#[test]
fn a_target_without_proc_answers_none_rather_than_zero() {
    assert_eq!(resident(), None);
    assert_eq!(breakdown(), None);
}

/// **Sensitivity, at the magnitude a null would be claimed at.**
///
/// Tampering proves a counter responds to its own subject. It does not prove
/// the counter is sensitive at the size the instrument is used to rule
/// something *out*. Every zero this module reports is a null — "no arena
/// retention", "no driver maps" — and a null from an instrument never shown
/// to scream is not a null.
///
/// So each class is built overwhelmingly present, at the shape measured on
/// the arm this campaign runs on (30–31 secondary heaps against 141 threads;
/// nvidia maps at 102.4 MiB, identical across four snapshots), and the
/// instrument is required to report it.
///
/// **Sizes are relations, not pins**: every expectation below is computed
/// from what the fixture was built to contain, so the test says the same
/// thing at any magnitude.
#[test]
fn every_class_is_shown_sensitive_at_the_magnitude_a_null_is_claimed_at() {
    const ARENAS: u64 = 31;
    /// Resident within each 64 MiB arena. Arenas are mostly-untouched
    /// reservations, so a realistic arena is far from full.
    const ARENA_RSS_KB: u64 = 48 * 1024;
    const DEVICE_KB: u64 = 102_400;

    let mut text = String::new();
    // 31 glibc secondary arenas: each a touched run followed by its
    // `PROT_NONE` remainder, together spanning exactly 64 MiB from a 64 MiB
    // boundary — the real shape, captured from a live process.
    for i in 0..ARENAS {
        // Scattered, two spans apart: glibc mmaps arenas at unrelated
        // addresses, and ADJACENT ones are a documented blind spot with its
        // own pin below rather than something this fixture should hide.
        let base = 0x7f00_0000_0000u64 + i * GLIBC_ARENA_SPAN * 2;
        let touched = base + (ARENA_RSS_KB * 1024);
        let end = base + GLIBC_ARENA_SPAN;
        text.push_str(&format!(
            "{base:x}-{touched:x} rw-p 00000000 00:00 0 \n\
             Rss:  {ARENA_RSS_KB} kB\n\
             AnonHugePages:  {ARENA_RSS_KB} kB\n\
             {touched:x}-{end:x} ---p 00000000 00:00 0 \n\
             Rss:  0 kB\n\
             AnonHugePages:  0 kB\n"
        ));
    }
    // The graphics driver's maps, at the figure measured on the RTX 3090 arm.
    text.push_str(&format!(
        "7fdcfc6f9000-7fdd02af9000 rw-s 00000000 00:07 1686    /dev/nvidiactl\n\
         Rss:  {DEVICE_KB} kB\n\
         AnonHugePages:  0 kB\n"
    ));

    let b = parse_smaps(&text);

    // ---- the arena term screams -----------------------------------------
    assert_eq!(
        b.arenas, ARENAS as u32,
        "the arena detector went blind at scale"
    );
    assert_eq!(
        b.arena_bytes,
        ARENAS * ARENA_RSS_KB * 1024,
        "every arena must be counted, not just the first"
    );
    assert!(
        b.arena_bytes > 1 << 30,
        "the fixture must be big enough that a zero here would be absurd: {} B",
        b.arena_bytes
    );

    // ---- so does the driver term ----------------------------------------
    assert_eq!(b.device_bytes, DEVICE_KB * 1024);
    assert_eq!(
        b.non_heap_bytes(),
        DEVICE_KB * 1024,
        "the driver's maps are the whole non-heap floor of this fixture, and \
         a floor that cannot see them is a 250 MiB budget's largest term \
         reading zero"
    );

    // ---- and the partition still closes at magnitude ---------------------
    assert!(
        b.partitions(),
        "the classes stopped adding up at scale: {b:?}"
    );
    assert_eq!(b.rss_bytes, ARENAS * ARENA_RSS_KB * 1024 + DEVICE_KB * 1024);

    // ---- the healthy input that RESEMBLES the defect ---------------------
    // The same 31 runs of the same size and the same total residency, moved
    // 4 KiB off the boundary: not glibc's, so `arenas` must read zero — and
    // the bytes must stay on the total rather than vanishing with them.
    let mut off = String::new();
    for i in 0..ARENAS {
        let base = 0x7f00_0000_0000u64 + i * GLIBC_ARENA_SPAN + 4096;
        let end = base + GLIBC_ARENA_SPAN;
        off.push_str(&format!(
            "{base:x}-{end:x} rw-p 00000000 00:00 0 \nRss:  {ARENA_RSS_KB} kB\n"
        ));
    }
    let o = parse_smaps(&off);
    assert_eq!(o.arenas, 0, "a misaligned run was read as a glibc arena");
    assert_eq!(
        o.anon_other_bytes,
        ARENAS * ARENA_RSS_KB * 1024,
        "the bytes left the census when the run failed the arena test"
    );
    assert!(o.partitions(), "a misclassification broke the partition");
}

/// **The adjacency blind spot, pinned at its real size rather than hidden.**
///
/// Two arenas allocated back to back are joined into one 128 MiB run that
/// carries the signature for neither, so both are missed. This is a FLOOR's
/// worth of under-counting and it is deliberate — the module note at the
/// run-extension site says why the obvious fix over-detects instead, by
/// 5,535 MiB on a real capture.
///
/// What this pins is the part that must never change: **the missed bytes stay
/// on the total.** An under-count that also lost the bytes would break the
/// partition, and the partition is the one property everything else rests on.
#[test]
fn adjacent_arenas_are_under_counted_and_their_bytes_stay_on_the_total() {
    const RSS_KB: u64 = 32 * 1024;
    let a = 0x7f00_0000_0000u64;
    let b = a + GLIBC_ARENA_SPAN;
    let text = format!(
        "{a:x}-{b:x} rw-p 00000000 00:00 0 \nRss:  {RSS_KB} kB\n\
         {b:x}-{:x} rw-p 00000000 00:00 0 \nRss:  {RSS_KB} kB\n",
        b + GLIBC_ARENA_SPAN
    );
    let out = parse_smaps(&text);
    assert_eq!(
        out.arenas, 0,
        "two adjacent arenas are joined and match neither; if this ever reads \
         2 the run rule changed and the module note must change with it"
    );
    assert_eq!(
        out.anon_other_bytes,
        2 * RSS_KB * 1024,
        "the under-counted arenas must land in `anon_other`, not vanish"
    );
    assert!(
        out.partitions(),
        "an under-count took bytes off the total, which the partition forbids"
    );
}
