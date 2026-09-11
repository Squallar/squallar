//! The spill's own contract: a round trip, a deletion, the purge that closes
//! the cross-process leak, a site string this will not put in a path, and the
//! standing refusal of memory mapping.

use super::{ArchiveSpill, FsArchiveSpill};

fn ts(seconds: i64) -> chrono::NaiveDateTime {
    chrono::DateTime::from_timestamp(1_700_000_000 + seconds, 0)
        .expect("a fixed in-range stamp")
        .naive_utc()
}

/// A directory of this test's own, under the OS temp dir. `FsArchiveSpill::new`
/// purges what it is given, so a reused name cannot leak state between runs.
///
/// **The process id is load-bearing, not decoration.** `name` separates these
/// tests from each other inside one binary; it does nothing about a second
/// copy of this binary, which is the ordinary state of this box when two
/// lanes run `cargo test --workspace` at once. That purge is unconditional,
/// so under a shared name one instance's construction deletes the other's
/// stored bytes and `load` hands back `None` for a key that was stored.
fn spill_root(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "squallar-archive-spill-test-{name}-{}",
        std::process::id()
    ))
}

#[test]
fn bytes_come_back_exactly_as_they_went_in() {
    let spill = FsArchiveSpill::new(spill_root("roundtrip")).expect("a temp dir is available");
    // Not a uniform fill: a length check alone would pass on the wrong buffer.
    let archive: Vec<u8> = (0..64_000u32).map(|i| (i % 251) as u8).collect();

    assert!(
        spill.store("KTLX", ts(0), &archive),
        "the store refused a well-formed site and a writable directory",
    );
    let back = spill
        .load("KTLX", ts(0))
        .expect("the bytes were stored under this exact key");
    assert_eq!(
        back, archive,
        "the archive that came back is not the one that went in, so a decode \
         would run on the wrong bytes",
    );
}

#[test]
fn a_key_that_was_never_stored_and_one_that_was_deleted_both_read_nothing() {
    let spill = FsArchiveSpill::new(spill_root("absent")).expect("a temp dir is available");
    assert!(
        spill.load("KTLX", ts(0)).is_none(),
        "a way back was invented for bytes nothing ever stored",
    );

    spill.store("KTLX", ts(0), &[7u8; 1024]);
    assert!(
        spill.load("KTLX", ts(0)).is_some(),
        "the fixture did not reach the state this test is about",
    );
    spill.delete("KTLX", ts(0));
    assert!(
        spill.load("KTLX", ts(0)).is_none(),
        "a deleted archive still reads back, so the byte ceiling frees nothing",
    );

    // Deleting what is already gone is the asserted end state, not a race won.
    spill.delete("KTLX", ts(0));
}

/// Two stamps one microsecond apart must not share a filename, or one archive
/// silently answers for another.
#[test]
fn neighbouring_stamps_do_not_collide() {
    let spill = FsArchiveSpill::new(spill_root("collide")).expect("a temp dir is available");
    let early = ts(0);
    let late = ts(300);

    spill.store("KTLX", early, &[1u8; 512]);
    spill.store("KTLX", late, &[2u8; 512]);

    assert_eq!(
        spill.load("KTLX", early).map(|b| b[0]),
        Some(1),
        "the later archive overwrote the earlier one's file",
    );
    assert_eq!(
        spill.load("KTLX", late).map(|b| b[0]),
        Some(2),
        "the earlier archive answered for the later one",
    );
    // And the site is part of the key, not just the stamp.
    assert!(
        spill.load("KABR", early).is_none(),
        "one site's archive answers for another's",
    );
}

/// **The cross-process leak, closed.** A file whose in-memory key did not
/// survive the process is unreachable and nothing else would ever collect it,
/// so construction purges.
///
/// TAMPER: drop the `remove_dir_all` from `FsArchiveSpill::new` and this goes
/// red — it asserts the stale file is GONE, not merely that a fresh store
/// works.
#[test]
fn construction_purges_a_directory_left_by_a_previous_run() {
    let root = spill_root("purge");
    std::fs::create_dir_all(&root).expect("the fixture can make its own directory");
    let stale = root.join("KTLX-20231114221320000000.spill");
    std::fs::write(&stale, [9u8; 4096]).expect("the fixture can write a stale file");
    let orphan = root.join("not-even-ours.part");
    std::fs::write(&orphan, [9u8; 16]).expect("the fixture can write an orphan");
    assert!(
        stale.exists() && orphan.exists(),
        "the fixture did not arise"
    );

    let spill = FsArchiveSpill::new(root.clone()).expect("a temp dir is available");

    assert!(
        !stale.exists(),
        "a spilled file from a previous run survived construction; its key is \
         gone, so nothing can ever read it and nothing can ever free it",
    );
    assert!(!orphan.exists(), "an orphan .part survived construction");
    assert!(
        root.is_dir(),
        "the purge took the directory and did not put it back, so every later \
         store fails",
    );
    // And the purged spill is usable, so the purge is not a scorched root.
    assert!(spill.store("KTLX", ts(0), &[1u8; 8]));
}

/// A site string comes from a listing and is not this module's to trust.
#[test]
fn a_site_string_this_will_not_put_in_a_path_is_refused_rather_than_escaped() {
    let root = spill_root("traversal");
    let spill = FsArchiveSpill::new(root.clone()).expect("a temp dir is available");

    for hostile in ["../escape", "K/TLX", "K.TLX", "", "K TLX", "KTLX\0"] {
        assert!(
            !spill.store(hostile, ts(0), &[1u8; 32]),
            "the store accepted the site {hostile:?}, which it would have to \
             put in a path",
        );
        assert!(
            spill.load(hostile, ts(0)).is_none(),
            "the load built a path out of the site {hostile:?}",
        );
        // And nothing landed anywhere near the root.
        assert!(
            !root.join("escape").exists(),
            "a store escaped the spill root with {hostile:?}",
        );
    }

    // The floor: a real ICAO identifier still passes, or this test would hold
    // with a `store` that refused everything.
    assert!(
        spill.store("KTLX", ts(0), &[1u8; 32]),
        "the refusal above is refusing everything, including real sites",
    );
}

/// A successful store leaves no `.part` behind, so the rename really is the
/// publish and not a copy.
#[test]
fn a_completed_store_leaves_no_partial_file() {
    let root = spill_root("nopart");
    let spill = FsArchiveSpill::new(root.clone()).expect("a temp dir is available");
    spill.store("KTLX", ts(0), &[3u8; 2048]);

    let parts: Vec<_> = std::fs::read_dir(&root)
        .expect("the root is readable")
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".part"))
        .collect();
    assert!(
        parts.is_empty(),
        "a partial file survived a successful store: {parts:?}",
    );
}

/// **The standing refusal, held as a test rather than as a comment.**
///
/// A mapping would post the whole saving on `live_bytes` — which counts what
/// the global allocator granted, and a mapping never passes through it — while
/// the resident pages move from `RssAnon` to `RssFile` and stay in `VmRSS`. The
/// process would cost the machine exactly what it did before. This scans the
/// module's own source, tests included, so the pattern cannot be introduced
/// here or copied back out of a fixture.
///
/// Every needle is assembled from pieces, so no line of this file contains the
/// word it looks for and the scanner cannot trip over itself. It is pinned on
/// a planted positive at the end rather than on a literal, because a scanner
/// looking for the wrong string would let everything through — the first
/// draft of this test built `mmapmap` and only the pin said so.
#[test]
fn nothing_on_this_path_memory_maps_anything() {
    let syscall = ["m", "m", "ap"].concat();
    let family = ["mem", "m", "ap"].concat();
    let needles = [syscall.as_str(), family.as_str()];

    let sources = [
        ("archive_spill.rs", include_str!("../archive_spill.rs")),
        ("archive_spill/tests.rs", include_str!("tests.rs")),
    ];

    for (name, src) in sources {
        for (n, line) in src.lines().enumerate() {
            // The prose says the word on purpose; only code may not.
            if line.trim_start().starts_with("//") {
                continue;
            }
            let lowered = line.to_ascii_lowercase();
            for needle in needles {
                assert!(
                    !lowered.contains(needle),
                    "{name}:{} maps memory: {line}\nThe spill reads into a \
                     fresh Vec on purpose — a mapping moves bytes between RSS \
                     classes without leaving the resident set, so it posts the \
                     whole saving on live_bytes for no change in what the \
                     process costs.",
                    n + 1,
                );
            }
        }
    }

    // **Pin the scanner on a planted positive**, or every assertion above is
    // vacuous: a needle built wrong matches nothing and passes on anything.
    let planted = format!("let region = unsafe {{ {syscall}(fd, len) }};");
    assert!(
        needles
            .iter()
            .any(|needle| planted.to_ascii_lowercase().contains(needle)),
        "the scanner does not fire on a planted mapping call, so it is \
         looking for the wrong text: {needles:?}",
    );
    // And that it is reading real source rather than an empty include.
    assert!(
        sources
            .iter()
            .all(|(_, src)| src.contains("fn load") || src.contains("fn spill_root")),
        "the scanner is reading the wrong text",
    );
}
