use super::*;

fn vol(n: u16) -> VolumeIndex {
    VolumeIndex::new(n).expect("valid index")
}

// -- VolumeIndex --------------------------------------------------------

/// The rotation is 1..=999 inclusive, and 0 is not a volume.
#[test]
fn volume_indices_outside_the_rotation_are_refused() {
    assert!(VolumeIndex::new(0).is_none());
    assert!(VolumeIndex::new(1000).is_none());
    assert_eq!(vol(1).get(), 1);
    assert_eq!(vol(999).get(), 999);
}

/// Wraps to 1, not to 0 — which is not a volume — and not to 1000.
#[test]
fn the_volume_after_999_is_1() {
    assert_eq!(vol(999).next(), vol(1));
    assert_eq!(vol(1).next(), vol(2));
}

/// The trailing slash is the whole point: without it `KTLX/9` is a prefix of
/// every key under `KTLX/90/` and `KTLX/99/`, so one listing would return
/// three volumes' chunks and the assembler would refuse most of them.
#[test]
fn a_volume_prefix_cannot_match_a_longer_index() {
    let prefix = vol(9).prefix("KTLX");
    assert_eq!(prefix, "KTLX/9/");
    assert!("KTLX/9/20260728-181234-001-S".starts_with(&prefix));
    assert!(!"KTLX/90/20260728-181234-001-S".starts_with(&prefix));
    assert!(!"KTLX/99/20260728-181234-001-S".starts_with(&prefix));
}

/// Indices are interpolated, never zero-padded — the bucket's own scheme.
#[test]
fn volume_prefixes_are_not_zero_padded() {
    assert_eq!(vol(1).prefix("KTLX"), "KTLX/1/");
    assert_eq!(vol(10).prefix("KTLX"), "KTLX/10/");
    assert_eq!(vol(100).prefix("KTLX"), "KTLX/100/");
}

// -- ChunkId ------------------------------------------------------------

/// Fails on an off-by-one in either slice: the sequence is `name[16..19]`
/// and the type is the last character.
#[test]
fn a_chunk_name_splits_into_time_sequence_and_kind() {
    let id = ChunkId::parse("KTLX", vol(42), "20260728-181234-007-I").expect("parses");
    assert_eq!(
        id.volume_time(),
        chrono::NaiveDate::from_ymd_opt(2026, 7, 28)
            .unwrap()
            .and_hms_opt(18, 12, 34)
            .unwrap()
    );
    assert_eq!(id.sequence(), 7);
    assert_eq!(id.kind(), ChunkKind::Intermediate);
    assert_eq!(id.site(), "KTLX");
    assert_eq!(id.volume(), vol(42));
}

/// The sequence is zero-padded in the name but a number here, so 001 and 055
/// order correctly rather than lexicographically.
#[test]
fn a_zero_padded_sequence_reads_as_a_number() {
    let first = ChunkId::parse("KTLX", vol(1), "20260728-181234-001-S").expect("parses");
    let last = ChunkId::parse("KTLX", vol(1), "20260728-181234-055-E").expect("parses");
    assert_eq!(first.sequence(), 1);
    assert_eq!(last.sequence(), 55);
    assert_eq!(first.kind(), ChunkKind::Start);
    assert_eq!(last.kind(), ChunkKind::End);
    assert!(first < last);
}

/// Names come from bucket keys, so nothing here may panic on one that does
/// not fit — it is dropped from the listing instead.
#[test]
fn a_name_that_does_not_fit_is_refused_rather_than_panicking() {
    for bad in [
        "",
        "2026",
        "20260728-181234-001-",       // 20 bytes: no type character
        "20260728-181234-001-X",      // unknown type
        "20260728_181234-001-S",      // wrong separator
        "20260728-181234-001-S-more", // trailing junk still parses the head
        "20260728-181234-abc-S",      // non-numeric sequence
        "not-a-timestamp-001-S",
        "2026072é-181234-001-S", // non-ASCII inside the first 20 bytes
    ] {
        assert!(
            ChunkId::parse("KTLX", vol(1), bad).is_none(),
            "{bad:?} should not parse"
        );
    }
}

/// A chunk of an older volume sorts before a chunk of a newer one whatever
/// their sequences say. Fails for an `Ord` that derives over the fields in
/// declaration order, which would compare the rotating index first.
#[test]
fn chunks_order_by_volume_time_before_sequence() {
    let old_last = ChunkId::parse("KTLX", vol(999), "20260728-180000-055-E").expect("parses");
    let new_first = ChunkId::parse("KTLX", vol(1), "20260728-181234-001-S").expect("parses");
    assert!(
        old_last < new_first,
        "the rotation makes index 999 older than index 1; ordering on the \
             index would invert them"
    );
}

#[test]
fn a_key_round_trips() {
    let key = "KTLX/42/20260728-181234-007-I";
    let id = ChunkId::from_key(key).expect("parses");
    assert_eq!(id.key(), key);
}

/// A key whose shape is wrong is dropped, not fatal.
#[test]
fn a_key_with_the_wrong_shape_is_refused() {
    for bad in [
        "KTLX/20260728-181234-001-S",          // no volume segment
        "KTLX/0/20260728-181234-001-S",        // index outside the rotation
        "KTLX/1000/20260728-181234-001-S",     // index outside the rotation
        "KTLX/abc/20260728-181234-001-S",      // non-numeric index
        "KTLX/42/extra/20260728-181234-001-S", // too many segments
    ] {
        assert!(ChunkId::from_key(bad).is_none(), "{bad:?} should not parse");
    }
}

// -- discovery ----------------------------------------------------------

/// S3 hands directories back in UTF-8 order because the index is not
/// zero-padded, so `10` arrives between `1` and `2`. Everything downstream
/// treats the list as rotation order, which it only is once sorted.
#[test]
fn volume_directories_are_sorted_numerically_not_lexicographically() {
    let listed = ["KTLX/1/", "KTLX/10/", "KTLX/100/", "KTLX/2/", "KTLX/20/"]
        .map(String::from)
        .to_vec();
    let parsed = parse_volume_indices(&listed);
    assert_eq!(
        parsed.iter().map(|v| v.get()).collect::<Vec<_>>(),
        vec![1, 2, 10, 20, 100],
        "left in S3's order, the pivot search is searching an array that is \
             not a rotation of anything"
    );
}

/// A directory naming something outside the rotation is dropped, not fatal.
#[test]
fn unparseable_volume_directories_are_dropped() {
    let listed = ["KTLX/1/", "KTLX/0/", "KTLX/1000/", "KTLX/abc/", "KTLX/"]
        .map(String::from)
        .to_vec();
    assert_eq!(
        parse_volume_indices(&listed)
            .iter()
            .map(|v| v.get())
            .collect::<Vec<_>>(),
        vec![1]
    );
}

/// Drive `newest_by_rotation` over a canned ladder, counting probes.
///
/// The future is polled by hand rather than through an executor — the probe
/// is immediate, so it never yields — mirroring `archive::tests::paginate`.
fn search(times: &[(u16, i64)]) -> (Option<VolumeIndex>, usize) {
    let base = chrono::NaiveDate::from_ymd_opt(2026, 7, 28)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap();
    let indices: Vec<VolumeIndex> = times.iter().map(|(i, _)| vol(*i)).collect();
    let table: std::collections::HashMap<u16, i64> = times.iter().copied().collect();
    let probes = std::cell::Cell::new(0usize);

    let fut = newest_by_rotation(&indices, |v| {
        probes.set(probes.get() + 1);
        let at = table.get(&v.get()).copied();
        async move {
            // A negative minute stands for a directory holding nothing
            // readable.
            Ok(at
                .filter(|m| *m >= 0)
                .map(|m| base + chrono::Duration::minutes(m)))
        }
    });
    let mut fut = Box::pin(fut);
    let waker = std::task::Waker::noop();
    let mut cx = std::task::Context::from_waker(waker);
    let out = match fut.as_mut().poll(&mut cx) {
        std::task::Poll::Ready(v) => v.expect("the canned probe never fails"),
        std::task::Poll::Pending => panic!("the fixture probe never yields"),
    };
    (out, probes.get())
}

/// Before the rotation has wrapped, the ladder simply ascends and the last
/// directory is the newest.
#[test]
fn an_unwrapped_ladder_ends_at_its_newest_volume() {
    let times: Vec<(u16, i64)> = (1..=20).map(|i| (i, i as i64)).collect();
    let (found, probes) = search(&times);
    assert_eq!(found, Some(vol(20)));
    assert_eq!(probes, 2, "an unwrapped ladder needs only its two ends");
}

/// The case a plain maximum-by-index gets wrong. The write head is at 12, so
/// 13..20 still hold the previous pass and are ~3.5 days older than 1..12.
#[test]
fn a_wrapped_ladder_finds_the_write_head() {
    let mut times: Vec<(u16, i64)> = (1..=12).map(|i| (i, 1000 + i as i64)).collect();
    times.extend((13..=20).map(|i| (i, i as i64 - 13)));
    times.sort();

    let (found, probes) = search(&times);
    assert_eq!(
        found,
        Some(vol(12)),
        "the newest volume is the one before the wrap, not the highest index"
    );
    assert!(probes <= 8, "probe count grew to {probes}");
}

/// The wrap sitting at the very start: only index 1 belongs to the new pass.
#[test]
fn a_ladder_that_just_wrapped_finds_the_first_volume() {
    let mut times: Vec<(u16, i64)> = vec![(1, 1000)];
    times.extend((2..=20).map(|i| (i, i as i64)));
    let (found, _) = search(&times);
    assert_eq!(found, Some(vol(1)));
}

/// One directory is the answer without any search at all.
#[test]
fn a_single_directory_is_the_answer() {
    let (found, probes) = search(&[(7, 5)]);
    assert_eq!(found, Some(vol(7)));
    assert_eq!(probes, 0);
}

#[test]
fn no_directories_means_no_volume() {
    let (found, _) = search(&[]);
    assert_eq!(found, None);
}

/// A directory being written right now can list nothing readable. Treated as
/// older than everything, so the search still converges — the cost of
/// guessing wrong here is one extra roll, not a wrong volume forever.
#[test]
fn a_directory_with_nothing_readable_does_not_derail_the_search() {
    let mut times: Vec<(u16, i64)> = (1..=12).map(|i| (i, 1000 + i as i64)).collect();
    times.push((13, -1)); // unreadable
    times.extend((14..=20).map(|i| (i, i as i64 - 13)));
    times.sort();
    let (found, _) = search(&times);
    assert_eq!(found, Some(vol(12)));
}

// -- decode -------------------------------------------------------------

/// Neither magic sequence is present, so the bytes are named rather than
/// guessed at.
#[test]
fn bytes_that_are_neither_shape_are_refused() {
    for bad in [
        b"hello world!!".as_slice(),
        b"".as_slice(),
        b"AR".as_slice(),
    ] {
        let err = decode_chunk("x", bad).expect_err("should not decode");
        assert!(
            matches!(err, ChunkError::UnrecognizedChunk { .. }),
            "got {err:?}"
        );
    }
}

/// `volume::File::records` slices `&data[24..]` with no length check, so
/// without the guard this panics inside `nexrad-data` rather than returning.
#[test]
fn a_start_chunk_too_short_for_its_header_does_not_panic() {
    let err = decode_chunk("short", b"AR2V0006.").expect_err("should not decode");
    assert!(
        matches!(err, ChunkError::ShortStartChunk { .. }),
        "got {err:?}"
    );
}

/// The magic bytes choose the decoder, not the name's `S`/`I`/`E` letter.
///
/// `ShortStartChunk` is the discriminator: it can only be reached from the
/// volume-header route. So a short `AR2` buffer *named as an intermediate*
/// must still produce it, and a bare LDM record *named as a start* must not
/// — even though it is also shorter than a volume header, which is exactly
/// what routing by filename would trip over.
///
/// The mis-decode this prevents is silent, not loud: an intermediate chunk
/// has no volume header, so the header route would slice 24 bytes off the
/// front of a real record and read a length from the middle of its payload.
#[test]
fn the_magic_bytes_choose_the_decoder_not_the_name() {
    let short_headered = b"AR2V0006.".to_vec();
    assert!(
        short_headered.len() < std::mem::size_of::<volume::Header>(),
        "the fixture has to be shorter than a header for this to discriminate"
    );
    let err = decode_chunk("20260728-181234-007-I", &short_headered).expect_err("too short");
    assert!(
        matches!(err, ChunkError::ShortStartChunk { .. }),
        "an AR2 prefix must take the volume-header route whatever the name's \
             type letter says, got {err:?}"
    );

    // A size-prefixed BZ record, also shorter than a volume header.
    let mut record = vec![0u8, 0, 0, 8];
    record.extend_from_slice(b"BZh9garbage");
    assert!(record.len() < std::mem::size_of::<volume::Header>());
    let outcome = decode_chunk("20260728-181234-001-S", &record);
    assert!(
        !matches!(outcome, Err(ChunkError::ShortStartChunk { .. })),
        "a BZ record was routed through the volume-header path because its \
             name said `S`; a real intermediate chunk would be silently \
             mis-sliced, got {outcome:?}"
    );
}

// -- assembly -----------------------------------------------------------

use nexrad_model::data::{Radial, RadialStatus};

const VOLUME_TIME: &str = "20260728-181234";

fn volume_time() -> chrono::NaiveDateTime {
    chrono::NaiveDateTime::parse_from_str(VOLUME_TIME, "%Y%m%d-%H%M%S").expect("fixture time")
}

/// Copy a radial with a different status. `Radial` has no setters, so every
/// field is read back off the original; if one is ever added and missed
/// here, the digest test below is what notices.
fn with_status(r: &Radial, status: RadialStatus) -> Radial {
    Radial::new(
        r.collection_timestamp(),
        r.azimuth_number(),
        r.azimuth_angle_degrees(),
        r.azimuth_spacing_degrees(),
        status,
        r.elevation_number(),
        r.elevation_angle_degrees(),
        r.reflectivity().cloned(),
        r.velocity().cloned(),
        r.spectrum_width().cloned(),
        r.differential_reflectivity().cloned(),
        r.differential_phase().cloned(),
        r.correlation_coefficient().cloned(),
        r.clutter_filter_power().cloned(),
    )
}

/// The same radial reassigned to another elevation cut, so one sweep's radials
/// can stand in for a cut the golden scan does not have.
fn renumber(r: &Radial, elevation: u8) -> Radial {
    Radial::new(
        r.collection_timestamp(),
        r.azimuth_number(),
        r.azimuth_angle_degrees(),
        r.azimuth_spacing_degrees(),
        r.radial_status(),
        elevation,
        r.elevation_angle_degrees(),
        r.reflectivity().cloned(),
        r.velocity().cloned(),
        r.spectrum_width().cloned(),
        r.differential_reflectivity().cloned(),
        r.differential_phase().cloned(),
        r.correlation_coefficient().cloned(),
        r.clutter_filter_power().cloned(),
    )
}

/// `volumetric::tests::golden_scan` re-sliced into the chunks the bucket
/// actually publishes.
///
/// 120 radials per chunk is the real size — confirmed against the live
/// bucket by `live_a_start_chunk_decodes_and_carries_the_coverage_pattern`,
/// which decodes an intermediate chunk to exactly 120 — so a super-resolution
/// cut is 6 chunks and a standard one 3. The slicing is the bucket's, not an
/// arbitrary split.
///
/// Two edits the fixture needs. Sequence 1 is a synthetic start chunk
/// carrying the VCP and no radials, which is what a real one looks like. And
/// each sweep's last radial is rebuilt with `ElevationEnd` — `ScanEnd` on the
/// final sweep — because the generator marks every radial
/// `IntermediateRadialData`, so without this nothing would ever seal.
fn golden_chunks() -> Vec<(u16, ChunkKind, ChunkContents)> {
    let scan = crate::volumetric::tests::golden_scan();
    let sweeps = scan.sweeps();
    let mut out: Vec<(u16, ChunkKind, ChunkContents)> = vec![(
        1,
        ChunkKind::Start,
        ChunkContents {
            radials: Vec::new(),
            coverage_pattern: Some(crate::volumetric::tests::vcp()),
            ..Default::default()
        },
    )];

    let mut sequence = 2u16;
    for (si, sweep) in sweeps.iter().enumerate() {
        let last_sweep = si + 1 == sweeps.len();
        let radials = sweep.radials();
        let terminator = if last_sweep {
            RadialStatus::ScanEnd
        } else {
            RadialStatus::ElevationEnd
        };
        let rebuilt: Vec<Radial> = radials
            .iter()
            .enumerate()
            .map(|(i, r)| {
                if i + 1 == radials.len() {
                    with_status(r, terminator)
                } else {
                    r.clone()
                }
            })
            .collect();
        for group in rebuilt.chunks(120) {
            out.push((
                sequence,
                ChunkKind::Intermediate,
                ChunkContents {
                    radials: group.to_vec(),
                    coverage_pattern: None,
                    ..Default::default()
                },
            ));
            sequence += 1;
        }
    }
    if let Some(last) = out.last_mut() {
        last.1 = ChunkKind::End;
    }
    out
}

/// Feed chunks to a fresh assembler in the order given.
fn assemble(chunks: Vec<(u16, ChunkKind, ChunkContents)>) -> VolumeAssembler {
    let mut a = VolumeAssembler::new("KTLX", vol(42));
    for (sequence, kind, contents) in chunks {
        a.ingest_contents(sequence, kind, volume_time(), contents);
    }
    a
}

fn digest(a: &mut VolumeAssembler) -> u64 {
    let scan = a.snapshot();
    crate::volumetric::tests::fnv1a64(&crate::volumetric::compute_echo_tops(&scan))
}

/// **The claim this module rests on**: assembling a volume from chunks
/// produces the same `Scan` that decoding the whole volume does.
///
/// Driven through `compute_echo_tops` and pinned on
/// `volumetric::tests::golden_echo_tops_grid_is_pinned`'s own digest, so it
/// exercises gridding, newest-wins dedup, beam heights and interpolation and
/// demands bit-identity — not merely "some sweeps arrived". The NaN
/// assertion additionally pins that ascending elevation-number order
/// reproduces newest-wins, which is the invariant `find_sweep`'s `.rev()`
/// and `VolumeCube` both stand on.
#[test]
fn the_assembled_golden_volume_reproduces_the_pinned_digest() {
    let mut a = assemble(golden_chunks());
    let progress = a.progress();
    assert!(
        progress.volume_complete,
        "the volume did not complete: {progress:?}"
    );
    assert_eq!(progress.late_radials_dropped, 0);

    let scan = a.snapshot();
    assert_eq!(
        scan.sweeps().len(),
        crate::volumetric::tests::golden_scan().sweeps().len()
    );
    let grid = crate::volumetric::compute_echo_tops(&scan);
    assert_eq!(
        crate::volumetric::tests::fnv1a64(&grid),
        0x4559ce366731e030,
        "assembling from chunks does not reproduce the volume the archive \
             path decodes"
    );
    let defined: usize = grid.values.iter().flatten().filter(|v| !v.is_nan()).count();
    assert_eq!(defined, 4680);
    assert!(
        grid.values[300][120].is_nan(),
        "the SAILS repeat no longer displaces the first 0.5° sweep, so the \
             emitted sweep order is not newest-last"
    );
}

/// Out-of-order delivery is the premise of the whole module. Fails for
/// `Sweep::from_radials`, which groups by consecutive runs and would emit a
/// sweep per fragment.
#[test]
fn a_shuffled_chunk_order_assembles_the_same_volume() {
    let mut chunks = golden_chunks();
    // A fixed permutation rather than a random one: no `rand` dependency,
    // and a failure reproduces.
    let mid = chunks.len() / 2;
    let mut shuffled: Vec<_> = Vec::with_capacity(chunks.len());
    let back = chunks.split_off(mid);
    let mut front = chunks.into_iter();
    let mut back = back.into_iter();
    loop {
        match (back.next(), front.next()) {
            (None, None) => break,
            (b, f) => shuffled.extend(b.into_iter().chain(f)),
        }
    }
    assert_eq!(shuffled.len(), golden_chunks().len());

    let mut a = assemble(shuffled);
    assert!(a.progress().volume_complete);
    assert_eq!(digest(&mut a), 0x4559ce366731e030);
}

/// The extreme case of the same property.
#[test]
fn a_reversed_chunk_order_assembles_the_same_volume() {
    let mut chunks = golden_chunks();
    chunks.reverse();
    let mut a = assemble(chunks);
    assert!(a.progress().volume_complete);
    assert_eq!(digest(&mut a), 0x4559ce366731e030);
}

/// A re-listed volume re-delivers chunks already seen. Fails for
/// `Sweep::merge`, which extends without deduping and would double every
/// sweep.
#[test]
fn ingesting_every_chunk_twice_changes_nothing() {
    let mut doubled = golden_chunks();
    doubled.extend(golden_chunks());
    let mut a = assemble(doubled);
    assert_eq!(
        a.progress().chunks_ingested,
        golden_chunks().len(),
        "a repeat was counted as new work"
    );
    assert_eq!(digest(&mut a), 0x4559ce366731e030);
}

/// The safety property, stated directly: a cut short of its radial count is
/// never in the emitted `Scan`, so `find_sweep` and `RenderInput::extract`
/// cannot reach one and NROT cannot be rendered from one.
#[test]
fn a_partial_cut_never_reaches_the_snapshot() {
    // Drop the chunk carrying the 0.5° cut's terminator.
    let mut chunks = golden_chunks();
    let dropped = chunks
        .iter()
        .position(|(_, _, c)| {
            c.radials.iter().any(|r| {
                r.elevation_number() == 1 && r.radial_status() == RadialStatus::ElevationEnd
            })
        })
        .expect("the 0.5° cut has a terminator chunk");
    chunks.remove(dropped);

    let mut a = assemble(chunks);
    let progress = a.progress();
    assert!(
        !progress.sealed_elevations.contains(&1),
        "an unterminated cut sealed anyway: {progress:?}"
    );
    assert!(!progress.volume_complete);

    let scan = a.snapshot();
    assert!(
        scan.sweeps().iter().all(|s| s.elevation_number() != 1),
        "the partial 0.5° cut reached the Scan"
    );
}

/// The terminator alone is not enough: it says the RDA finished the cut, not
/// that every chunk of it arrived. Fails for a seal rule that trusts the
/// status by itself.
#[test]
fn a_terminator_without_the_radial_count_does_not_seal() {
    let mut chunks = golden_chunks();
    // Keep the 0.5° cut's terminator chunk, drop an interior one.
    let interior = chunks
        .iter()
        .position(|(_, _, c)| {
            c.radials.first().is_some_and(|r| r.elevation_number() == 1)
                && c.radials
                    .iter()
                    .all(|r| r.radial_status() != RadialStatus::ElevationEnd)
        })
        .expect("the 0.5° cut spans several chunks");
    chunks.remove(interior);

    let a = assemble(chunks);
    let progress = a.progress();
    assert!(
        !progress.sealed_elevations.contains(&1),
        "a cut missing 120 of its 720 radials sealed on the terminator alone"
    );
}

/// The final cut of a volume carries `ScanEnd`, not `ElevationEnd`. Reading
/// only `ElevationEnd` leaves the topmost cut open forever, so
/// `volume_complete` never fires and everything gated on it — echo tops, the
/// Level III refetch, the loop append — is dead.
///
/// **This is only half of that claim, and the half it is not covers a bug that
/// shipped.** Everything here stops at the assembler: the flag is read off
/// `VolumeAssembler::progress` in the same breath as `snapshot`, so of course
/// the two agree. What the frontend actually reads is
/// `PollOutcome::closed`, produced one layer up by [`ChunkPoller::roll`] —
/// which for a long time reported the completion and dropped the volume in the
/// same statement, so the flag fired and everything gated on it was dead
/// anyway. `a_roll_hands_back_the_volume_it_closed` is the missing layer, and
/// any future claim of the form "`volume_complete` reaches a reader" belongs
/// there rather than here.
#[test]
fn the_final_cut_seals_on_scan_end() {
    let mut a = assemble(golden_chunks());
    let top = crate::volumetric::tests::golden_scan().sweeps().len() as u8;
    assert!(
        a.is_elevation_sealed(top),
        "the last cut never sealed, so the volume can never complete"
    );
    assert!(a.progress().saw_scan_end);
    assert!(a.progress().volume_complete);
    let _ = a.snapshot();
}

/// **A roll must hand back the volume whose completion it reports.**
///
/// [`ChunkPoller::roll`] is the only producer of [`PollOutcome::closed`], and
/// both callers set that field in the same statement that replaces the
/// assembler [`ChunkPoller::snapshot`] reads. Whatever the roll does not hand
/// back is gone: the caller learns a volume completed at the exact instant it
/// can no longer read it, and everything gated on `volume_complete` — echo
/// tops, the Level III refetch, the loop append — is dead on a healthy feed.
///
/// Asserted against the *live* snapshot as well, because that is the read the
/// frontend used to make and the number it got back. Reverting `roll` to
/// return only [`VolumeProgress`] cannot pass this.
#[test]
fn a_roll_hands_back_the_volume_it_closed() {
    let mut poller = ChunkPoller::new("KTLX");
    poller.current = Some(assemble(golden_chunks()));
    let expected = crate::volumetric::tests::golden_scan().sweeps().len();

    let closed = poller.roll(vol(43)).expect("a volume was open");

    assert!(
        closed.progress.volume_complete,
        "the fixture must close complete, or this proves nothing: {:?}",
        closed.progress
    );
    let scan = closed
        .scan
        .as_ref()
        .expect("a completed volume hands back its scan");
    assert_eq!(
        scan.sweeps().len(),
        expected,
        "the roll reported a completed volume and did not hand it over"
    );
    // And the scan really is the volume the progress describes, not some
    // other one: same cuts, in the same order.
    let handed: Vec<u8> = scan.sweeps().iter().map(|s| s.elevation_number()).collect();
    assert_eq!(
        handed, closed.progress.sealed_elevations,
        "the scan and the progress in one `ClosedVolume` describe different \
             volumes"
    );
    assert_eq!(
        poller.snapshot().map(|s| s.sweeps().len()),
        Some(0),
        "the poller is on the new volume now — which is why the closed one \
             has to travel out with the outcome rather than be read back off it"
    );
}

/// A volume that closed short reports the cuts it lost and hands back **no
/// scan**.
///
/// No scan because nothing reads one: every consumer gates on
/// `volume_complete` first, and building it would deep-copy every sealed
/// `Sweep` — `close` clears [`VolumeAssembler::snapshot`]'s cache exactly when
/// it abandoned a cut, so the cold rebuild landed precisely on the volumes that
/// get discarded.
///
/// This deliberately gives up a guarantee an earlier version of this test
/// asserted — that a partial cut is absent from the scan a short close hands
/// over — because there is now no such scan to inspect. Nothing consumed it. The
/// underlying property is unchanged and still pinned where it belongs, on
/// `snapshot` itself, by `a_partial_cut_never_reaches_the_snapshot`.
///
/// The mutation this kills is `roll` reading `progress()` instead of `close()`:
/// the still-`Open` cut would be missing from `progress.abandoned`, so a caller
/// asking "what is this volume missing?" would be told "nothing". Verified by
/// mutation, along with the discovery that the *order* of `close` and `snapshot`
/// is not observable and so is not claimed to be.
#[test]
fn a_roll_leaves_an_abandoned_cut_out_of_the_scan_it_hands_back() {
    // Drop the chunk carrying the 0.5° cut's terminator, so that cut is still
    // open when the volume closes.
    let mut chunks = golden_chunks();
    let dropped = chunks
        .iter()
        .position(|(_, _, c)| {
            c.radials.iter().any(|r| {
                r.elevation_number() == 1 && r.radial_status() == RadialStatus::ElevationEnd
            })
        })
        .expect("the 0.5° cut has a terminator chunk");
    chunks.remove(dropped);

    let mut poller = ChunkPoller::new("KTLX");
    poller.current = Some(assemble(chunks));
    let closed = poller.roll(vol(43)).expect("a volume was open");

    assert!(
        closed.progress.abandoned.iter().any(|a| a.elevation == 1),
        "precondition: the 0.5° cut must have closed short: {:?}",
        closed.progress
    );
    assert!(
        !closed.progress.volume_complete,
        "a volume with an abandoned cut reported complete"
    );
    assert!(
        closed.scan.is_none(),
        "a volume that closed short built a snapshot anyway — a deep copy of \
             every sealed sweep, on the one path where `close` had just cleared \
             the cache, for a volume every consumer discards"
    );
}

/// **`volume_complete` is not "whole", and `roll` reports both.**
///
/// Under a narrow [`CutSelection`] a volume completes on the cuts it was asked
/// for — one, for a single 0.5° pane, which is the *default* layout. That is the
/// right answer for the tilts on screen and the wrong one for anything that
/// integrates the column, and the two used to be one flag, leaving the
/// distinction to callers. A frontend that cached such a volume for its loops
/// then handed it to echo tops the moment the pane changed product.
#[test]
fn a_narrow_selection_completes_without_the_volume_being_whole() {
    let narrow = CutSelection::Tilts(vec![0.5]);
    let mut poller = ChunkPoller::new("KTLX");
    poller.set_selection(narrow.clone());
    poller.current = Some(narrow_volume(&narrow));

    let closed = poller.roll(vol(43)).expect("a volume was open");

    assert!(
        closed.progress.volume_complete,
        "precondition: the cuts the selection asked for must all have sealed, \
             or the distinction below is not the one under test: {:?}",
        closed.progress
    );
    assert!(
        !closed.progress.whole_volume_complete,
        "a volume holding only the 0.5° cuts reported itself whole, so every \
             product that integrates the column may read it: {:?}",
        closed.progress
    );
    assert_eq!(
        closed.scan.as_ref().map(|s| s.sweeps().len()),
        Some(2),
        "precondition: the narrowed volume must really be short — two 0.5° \
             cuts out of a four-cut pattern"
    );
}

/// A volume assembled the way a 0.5°-only feed assembles one: the start chunk,
/// both halves of the 0.5° split cut, and the volume's terminator.
///
/// The terminator is the faithful part. It carries the *final* cut's radials,
/// and it arrives even under a narrow selection because its sequence runs past
/// the planned ranges the coverage pattern implies — SAILS and MRLE inserts are
/// not in message 5, so real sequence numbers overrun the map, and
/// `ElevationChunkMap::wants` deliberately answers `true` for anything it cannot
/// place. That is what lets a narrow volume ever see `ScanEnd` and so ever
/// report complete, and it is also why the top cut is present but partial.
fn narrow_volume(selection: &CutSelection) -> VolumeAssembler {
    let vcp = vcp_with(&[(0.5, true), (0.5, true), (4.0, false), (6.0, false)]);
    let map = ElevationChunkMap::from_coverage_pattern(&vcp).expect("cuts");
    assert_eq!(
        map.wanted_elevations(selection),
        vec![1, 2],
        "precondition: the selection must want both halves of the split cut"
    );

    let mut a = VolumeAssembler::new("KTLX", vol(42));
    a.set_selection(selection.clone());
    a.ingest_contents(
        1,
        ChunkKind::Start,
        volume_time(),
        ChunkContents {
            radials: Vec::new(),
            coverage_pattern: Some(vcp),
            ..Default::default()
        },
    );

    let scan = crate::volumetric::tests::golden_scan();
    // Both 0.5° cuts, each ending on `ElevationEnd`, at the sequences the map
    // places them at — checked against `wants_chunk`, so the fixture cannot
    // quietly stop being what a narrow feed downloads.
    let mut sequence = 2u16;
    for elevation in [1u8, 2u8] {
        let sweep = &scan.sweeps()[0];
        let radials: Vec<Radial> = sweep
            .radials()
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let r = renumber(r, elevation);
                if i + 1 == sweep.radials().len() {
                    with_status(&r, RadialStatus::ElevationEnd)
                } else {
                    r
                }
            })
            .collect();
        for group in radials.chunks(120) {
            assert!(
                a.wants_chunk(sequence),
                "chunk {sequence} of the 0.5° cut was not wanted"
            );
            a.ingest_contents(
                sequence,
                ChunkKind::Intermediate,
                volume_time(),
                ChunkContents {
                    radials: group.to_vec(),
                    coverage_pattern: None,
                    ..Default::default()
                },
            );
            sequence += 1;
        }
    }

    // The cuts in between are never fetched.
    for skipped in sequence..sequence + 3 {
        assert!(
            !a.wants_chunk(skipped),
            "chunk {skipped} belongs to a cut the selection skipped and was \
                 wanted anyway"
        );
    }

    // The terminator, past the map's ranges and so wanted regardless.
    let top = scan.sweeps().last().expect("the golden scan has sweeps");
    let tail: Vec<Radial> = top.radials()[..20]
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let r = renumber(r, 4);
            if i == 19 {
                with_status(&r, RadialStatus::ScanEnd)
            } else {
                r
            }
        })
        .collect();
    let terminator = 900u16;
    assert!(
        a.wants_chunk(terminator),
        "a sequence the map cannot place must be wanted, or a narrow volume \
             never sees ScanEnd and never completes at all"
    );
    a.ingest_contents(
        terminator,
        ChunkKind::End,
        volume_time(),
        ChunkContents {
            radials: tail,
            coverage_pattern: None,
            ..Default::default()
        },
    );
    a
}

/// The counterweight: with everything asked for, the two flags agree. Without
/// this the assertion above could pass on a flag that is simply always false.
#[test]
fn a_whole_volume_reports_both_flags() {
    let mut poller = ChunkPoller::new("KTLX");
    poller.current = Some(assemble(golden_chunks()));
    let closed = poller.roll(vol(43)).expect("a volume was open");
    assert!(closed.progress.volume_complete);
    assert!(
        closed.progress.whole_volume_complete,
        "a volume with every cut sealed contiguously from 1 did not report \
             whole, so nothing would ever reach a volume integral: {:?}",
        closed.progress
    );
}

/// A volume joined mid-flight is complete for a narrow selection that happens
/// to want only cuts it caught, and is still not whole.
///
/// The contiguity clause is what separates them. Without it the missing lower
/// cuts would be invisible and `compute_echo_tops` would integrate tilts 3..23
/// and report every column's top far too low.
#[test]
fn a_volume_joined_mid_flight_is_never_whole() {
    let chunks: Vec<_> = golden_chunks()
        .into_iter()
        .filter(|(_, _, c)| c.radials.first().is_none_or(|r| r.elevation_number() >= 3))
        .collect();
    let a = assemble(chunks);
    assert!(a.progress().saw_scan_end, "the fixture must reach the end");
    assert!(
        !a.is_whole_volume_complete(),
        "a volume missing its lowest cuts reported whole: {:?}",
        a.progress()
    );
}

/// One closing round describes **two** volumes, which is why the closed one
/// cannot be represented by the live snapshot.
///
/// This is the shape `poll` produces whenever the probe round finds the new
/// directory already filling — after an error backoff of up to
/// [`MAX_BACKOFF`], several cuts' worth. A consumer that trusted
/// `closed.volume_complete` and then read the snapshot got a one-cut volume:
/// no error, no NaN, a plausible wrong answer from every product
/// [`crate::types::RadarProduct::reads_whole_volume`] names.
#[test]
fn a_closing_round_can_also_seal_a_cut_of_the_new_volume() {
    let mut poller = ChunkPoller::new("KTLX");
    poller.current = Some(assemble(golden_chunks()));
    let closed = poller.roll(vol(43)).expect("a volume was open");
    assert!(closed.progress.volume_complete);

    // What `poll` does next in the same round: list the new volume's
    // directory and ingest whatever is already in it.
    let current = poller.current.as_mut().expect("the roll started one");
    let mut sealed: Vec<u8> = Vec::new();
    for (sequence, kind, contents) in golden_chunks() {
        let outcome = current.ingest_contents(sequence, kind, volume_time(), contents);
        sealed.extend(outcome.sealed);
        if !sealed.is_empty() {
            break;
        }
    }
    assert_eq!(sealed, vec![1], "the new volume's first cut did not seal");
    assert_eq!(
        poller.snapshot().map(|s| s.sweeps().len()),
        Some(1),
        "the live snapshot on a closing round is the new volume, one cut in"
    );
    assert_eq!(
        closed.scan.as_ref().map(|s| s.sweeps().len()),
        Some(crate::volumetric::tests::golden_scan().sweeps().len()),
        "and the closed volume is unaffected by what the new one ingests"
    );
}

/// Reported per chunk, and only for the cut that finished on it — the
/// frontend uses this to decide which panes to invalidate and whether a
/// snapshot is worth building at all.
#[test]
fn a_cut_is_reported_sealed_on_the_chunk_that_finishes_it() {
    let mut a = VolumeAssembler::new("KTLX", vol(42));
    let mut seals: Vec<(u16, Vec<u8>)> = Vec::new();
    for (sequence, kind, contents) in golden_chunks() {
        let outcome = a.ingest_contents(sequence, kind, volume_time(), contents);
        if !outcome.sealed.is_empty() {
            seals.push((sequence, outcome.sealed));
        }
    }
    let sealed_order: Vec<u8> = seals.iter().flat_map(|(_, e)| e.clone()).collect();
    assert_eq!(
        sealed_order,
        (1..=crate::volumetric::tests::golden_scan().sweeps().len() as u8).collect::<Vec<_>>(),
        "cuts must seal once each, in acquisition order: {seals:?}"
    );
}

/// A volume joined mid-flight has no entry for the cuts that finished before
/// the first chunk arrived. Without the contiguity clause it would report
/// complete, and `compute_echo_tops` would integrate only the upper tilts
/// and report every column's top far too low.
#[test]
fn a_volume_joined_mid_flight_never_reports_complete() {
    let chunks: Vec<_> = golden_chunks()
        .into_iter()
        .filter(|(_, _, c)| c.radials.first().is_none_or(|r| r.elevation_number() >= 3))
        .collect();
    let a = assemble(chunks);
    let progress = a.progress();
    assert!(
        progress.saw_scan_end,
        "the fixture must still reach the end of the volume"
    );
    assert!(
        !progress.volume_complete,
        "a volume missing its lowest cuts reported complete: {progress:?}"
    );
}

/// Radials for a cut that already sealed are dropped, not merged: the sealed
/// sweep may already be inside a `Scan` a render is holding.
#[test]
fn late_radials_for_a_sealed_cut_are_dropped() {
    let mut a = assemble(golden_chunks());
    let before = a.snapshot();
    let replay = golden_chunks();
    // Re-deliver one cut's chunks under fresh sequence numbers so the
    // idempotence check does not short-circuit them.
    let mut next_sequence = 900u16;
    for (_, kind, contents) in replay {
        if contents
            .radials
            .first()
            .is_some_and(|r| r.elevation_number() == 1)
        {
            a.ingest_contents(next_sequence, kind, volume_time(), contents);
            next_sequence += 1;
        }
    }
    let after = a.snapshot();
    assert!(
        std::sync::Arc::ptr_eq(&before, &after),
        "a sealed cut was reopened, so a Scan already handed out changed"
    );
    assert!(a.progress().late_radials_dropped > 0);
}

/// The cache is what keeps a poll cheap: `Sweep: Clone` deep-copies every
/// gate byte, so rebuilding per poll would be hundreds of megabytes of
/// memcpy across a volume.
#[test]
fn the_snapshot_is_shared_until_a_cut_seals() {
    let mut a = VolumeAssembler::new("KTLX", vol(42));
    let chunks = golden_chunks();
    // Up to but not including the chunk that seals the first cut.
    let seal_at = chunks
        .iter()
        .position(|(_, _, c)| {
            c.radials
                .iter()
                .any(|r| r.radial_status() == RadialStatus::ElevationEnd)
        })
        .expect("some chunk seals a cut");
    for (sequence, kind, contents) in chunks.iter().take(seal_at).cloned() {
        a.ingest_contents(sequence, kind, volume_time(), contents);
    }
    let first = a.snapshot();
    let second = a.snapshot();
    assert!(
        std::sync::Arc::ptr_eq(&first, &second),
        "two snapshots with no seal between them rebuilt the volume"
    );

    let (sequence, kind, contents) = chunks[seal_at].clone();
    a.ingest_contents(sequence, kind, volume_time(), contents);
    let third = a.snapshot();
    assert!(
        !std::sync::Arc::ptr_eq(&first, &third),
        "a seal did not invalidate the cached snapshot"
    );
}

/// A leftover from the previous pass through this rotating index carries
/// elevation numbers that would collide with the volume being assembled, so
/// it must be refused outright rather than merged.
#[test]
fn a_chunk_from_another_volume_is_refused() {
    let mut a = VolumeAssembler::new("KTLX", vol(42));
    let chunks = golden_chunks();
    let (sequence, kind, contents) = chunks[1].clone();
    assert!(
        a.ingest_contents(sequence, kind, volume_time(), contents)
            .accepted
    );
    let sealed_before = a.progress().sealed_elevations;

    let stale = volume_time() - chrono::Duration::days(3);
    for (sequence, kind, contents) in chunks.into_iter().skip(2) {
        let outcome = a.ingest_contents(sequence, kind, stale, contents);
        assert!(
            !outcome.accepted,
            "a chunk from volume {stale} was merged into volume {}",
            volume_time()
        );
    }
    assert_eq!(a.progress().sealed_elevations, sealed_before);
    assert_eq!(a.progress().chunks_ingested, 1);
}

// -- poller -------------------------------------------------------------

/// Seed a poller's assembler with one chunk so it has a volume time.
fn primed(volume: VolumeIndex) -> ChunkPoller {
    let mut p = ChunkPoller::resume("KTLX", volume);
    let (sequence, kind, contents) = golden_chunks()[1].clone();
    p.current
        .as_mut()
        .expect("resume seeds an assembler")
        .ingest_contents(sequence, kind, volume_time(), contents);
    p
}

#[test]
fn a_cold_poller_plans_discovery() {
    let p = ChunkPoller::new("KTLX");
    assert_eq!(p.plan(volume_time()), PollPlan::Discover);
}

/// While a volume is still filling, keep listing it; once it has ended, look
/// at the next index instead.
#[test]
fn a_filling_volume_is_listed_and_a_finished_one_probes_the_next() {
    let mut p = primed(vol(42));
    assert_eq!(p.plan(volume_time()), PollPlan::Fill { volume: vol(42) });

    for (sequence, kind, contents) in golden_chunks().into_iter().skip(2) {
        p.current
            .as_mut()
            .unwrap()
            .ingest_contents(sequence, kind, volume_time(), contents);
    }
    assert_eq!(
        p.plan(volume_time()),
        PollPlan::ProbeNext {
            current: vol(42),
            next: vol(43)
        }
    );
}

/// Rollover reaches the plan, not just `VolumeIndex::next`.
#[test]
fn the_volume_probed_after_999_is_1() {
    let mut p = primed(vol(999));
    for (sequence, kind, contents) in golden_chunks().into_iter().skip(2) {
        p.current
            .as_mut()
            .unwrap()
            .ingest_contents(sequence, kind, volume_time(), contents);
    }
    assert_eq!(
        p.plan(volume_time()),
        PollPlan::ProbeNext {
            current: vol(999),
            next: vol(1)
        }
    );
}

/// A backgrounded app comes back to a volume long gone. Stepping one index
/// per round would take many rounds to catch up, so discovery is re-run.
#[test]
fn a_stale_volume_replans_discovery() {
    let p = primed(vol(42));
    let much_later = volume_time() + chrono::Duration::minutes(30);
    assert_eq!(p.plan(much_later), PollPlan::Discover);
    assert_eq!(
        p.plan(volume_time() + chrono::Duration::minutes(2)),
        PollPlan::Fill { volume: vol(42) },
        "a volume two minutes old is simply the current one"
    );
}

/// A re-listed directory returns everything every round; only what is new
/// gets fetched.
#[test]
fn an_already_ingested_chunk_is_not_selected_again() {
    let p = primed(vol(42));
    let listed: Vec<ChunkId> = (1..=3)
        .map(|seq| {
            ChunkId::parse("KTLX", vol(42), &format!("{VOLUME_TIME}-{seq:03}-I")).expect("parses")
        })
        .collect();

    let selected = p.select(&listed);
    assert_eq!(
        selected.iter().map(ChunkId::sequence).collect::<Vec<_>>(),
        vec![1, 3],
        "sequence 2 was already ingested by `primed`"
    );
}

/// A directory the rotation has not yet cleared holds the previous pass's
/// chunks beside the new ones. Merging them would collide elevation numbers.
#[test]
fn a_leftover_from_the_previous_pass_is_not_selected() {
    let p = primed(vol(42));
    let stale = (volume_time() - chrono::Duration::days(3))
        .format("%Y%m%d-%H%M%S")
        .to_string();
    let listed = vec![
        ChunkId::parse("KTLX", vol(42), &format!("{VOLUME_TIME}-005-I")).expect("parses"),
        ChunkId::parse("KTLX", vol(42), &format!("{stale}-006-I")).expect("parses"),
    ];
    assert_eq!(
        p.select(&listed)
            .iter()
            .map(ChunkId::sequence)
            .collect::<Vec<_>>(),
        vec![5],
    );
}

/// A listed-then-missing key is ordinary — S3 is eventually consistent and
/// the rotation retires keys — so it skips. Anything else ends the round,
/// which is what stops a 503 being read as an empty volume.
#[test]
fn a_missing_chunk_is_skipped_and_a_server_error_is_not() {
    assert_eq!(
        fetch_disposition(&ArchiveError::NotFound("k".into())),
        FetchDisposition::Skip
    );
    assert_eq!(
        fetch_disposition(&ArchiveError::Status {
            status: reqwest::StatusCode::SERVICE_UNAVAILABLE,
            url: "u".into(),
            body: None,
        }),
        FetchDisposition::Abort
    );
    assert_eq!(
        fetch_disposition(&ArchiveError::MalformedListing("x".into())),
        FetchDisposition::Abort
    );
}

/// Backing off on quiet, not on error: an empty round is the ordinary state
/// between cuts and across the gap between volumes, and treating it as a
/// failure would retreat from a feed that is working perfectly.
#[test]
fn the_interval_backs_off_on_failure_and_on_quiet_but_not_on_progress() {
    let mut p = ChunkPoller::new("KTLX");
    assert_eq!(p.suggested_interval(), POLL_INTERVAL);

    p.last_round_was_quiet = true;
    assert_eq!(p.suggested_interval(), QUIET_INTERVAL);

    p.last_round_was_quiet = false;
    p.consecutive_failures = 1;
    assert_eq!(p.suggested_interval(), POLL_INTERVAL * 2);
    p.consecutive_failures = 3;
    assert_eq!(p.suggested_interval(), POLL_INTERVAL * 8);
    p.consecutive_failures = 99;
    assert_eq!(
        p.suggested_interval(),
        MAX_BACKOFF,
        "the backoff has to stop somewhere"
    );

    p.consecutive_failures = 0;
    assert_eq!(p.suggested_interval(), POLL_INTERVAL);
}

/// A notification for a newer volume rolls the assembler without any probe —
/// the chunk's own name carries the volume start time, which is what
/// distinguishes a new volume from the previous pass through the same
/// rotating index.
#[test]
fn a_notified_chunk_from_a_newer_volume_rolls_the_assembler() {
    let p = primed(vol(42));
    let later = (volume_time() + chrono::Duration::minutes(6))
        .format("%Y%m%d-%H%M%S")
        .to_string();
    let next = ChunkId::parse("KTLX", vol(43), &format!("{later}-001-S")).expect("parses");

    // `plan` would have said `Fill` — the current volume has not ended — so
    // this roll comes from the notification alone.
    assert_eq!(p.plan(volume_time()), PollPlan::Fill { volume: vol(42) });
    assert!(
        p.should_roll_to(&next),
        "a chunk from a later-started volume must roll the assembler"
    );
}

/// And one from an older volume is refused: an index the rotation has not
/// reused still holds the previous pass, and a replayed message must not drag
/// the assembler backwards.
#[test]
fn a_notified_chunk_from_an_older_volume_is_ignored() {
    let p = primed(vol(42));
    let earlier = (volume_time() - chrono::Duration::days(3))
        .format("%Y%m%d-%H%M%S")
        .to_string();
    let stale = ChunkId::parse("KTLX", vol(41), &format!("{earlier}-001-S")).expect("parses");
    assert!(!p.should_roll_to(&stale));
    assert!(p.is_stale_notification(&stale));
}

/// A chunk of the volume already being assembled is neither a roll nor stale.
#[test]
fn a_notified_chunk_of_the_current_volume_is_just_ingested() {
    let p = primed(vol(42));
    let same = ChunkId::parse("KTLX", vol(42), &format!("{VOLUME_TIME}-009-I")).expect("parses");
    assert!(!p.should_roll_to(&same));
    assert!(!p.is_stale_notification(&same));
}

// -- selective download --------------------------------------------------

/// A VCP whose first `super_res` cuts are half-degree and the rest standard,
/// which is the shape every real pattern has.
fn vcp_with(cuts: &[(f32, bool)]) -> VolumeCoveragePattern {
    let elevation_cuts = cuts
        .iter()
        .map(|(angle, super_res)| {
            ElevationCut::new(
                *angle as f64,
                ChannelConfiguration::ConstantPhase,
                WaveformType::CS,
                0.0,
                *super_res,
                false,
                false,
                false,
                0,
                0,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                false,
                0,
                false,
                0,
                false,
                false,
            )
        })
        .collect();
    VolumeCoveragePattern::new(
        35,
        0,
        0.5,
        PulseWidth::Short,
        false,
        0,
        false,
        0,
        false,
        false,
        0,
        false,
        false,
        elevation_cuts,
    )
}

/// The feed's slack stays wider than the renderer's match window.
///
/// These two were once the same 0.3, and the comment here cited
/// `find_sweep` as the reason. They answer different questions: the
/// renderer compares a request against a sweep already flown and can be
/// exact, while this compares it against the VCP's *planned* angle for a cut
/// that has not been downloaded — the decision being what to skip. The
/// errors are asymmetric too, so when the renderer's window narrowed this
/// one deliberately did not follow it down.
#[test]
fn the_feeds_slack_stays_wider_than_the_renderers_window() {
    assert!(
        f64::from(ELEVATION_MATCH) > crate::render::ELEVATION_WINDOW,
        "a cut the renderer would refuse to draw must still be downloaded — \
             feed slack {ELEVATION_MATCH} vs renderer window {}",
        crate::render::ELEVATION_WINDOW,
    );

    // Concretely: a planned cut a quarter of a degree from the selection is
    // further off than the renderer would accept, and is fetched anyway.
    let vcp = vcp_with(&[(0.5, true), (0.75, true)]);
    let map = ElevationChunkMap::from_coverage_pattern(&vcp).expect("cuts");
    let (second, angle) = map.cut_for(8).expect("the second cut starts at sequence 8");
    assert_eq!(second, 2, "sequence 8 is the second cut");
    assert!(
        (f64::from(angle) - 0.75).abs() < 1e-6
            && (f64::from(angle) - 0.5).abs() > crate::render::ELEVATION_WINDOW,
        "the fixture must put the second cut outside the renderer's window",
    );
    assert!(
        map.wants(8, &CutSelection::Tilts(vec![0.5])),
        "a cut this near the selection must still be downloaded",
    );
}

/// A site that points its lowest cut below the horizon can still ask for it.
///
/// The VCP carries the angle unsigned, so KMSX declares its base tilt as
/// 359.82° while its radials report −0.124°. Taken at face value that cut is
/// 360° from any angle a user could select, so under a narrow
/// [`CutSelection`] it would never be wanted, never downloaded, and the
/// lowest tilt of a mountain-top site would stay empty for good.
#[test]
fn a_cut_below_the_horizon_is_wanted_by_the_angle_it_actually_points_at() {
    let vcp = vcp_with(&[(359.82, true), (0.48, true), (0.88, true)]);
    let map = ElevationChunkMap::from_coverage_pattern(&vcp).expect("cuts");

    assert_eq!(
        map.cut_for(2)
            .map(|(n, a)| (n, (a * 100.0).round() / 100.0)),
        Some((1, -0.18)),
        "the base cut points below the horizon and must read as negative",
    );

    let wanted = CutSelection::Tilts(vec![-0.1]);
    assert!(
        map.wants(2, &wanted),
        "selecting the base tilt must fetch the cut that flies it",
    );
    assert_eq!(
        map.wanted_elevations(&wanted),
        vec![1],
        "and that cut is the one the volume must wait for",
    );

    // The rest of the pattern is unaffected: a positive selection does not
    // suddenly reach the sub-horizon cut.
    assert!(
        !map.wants(2, &CutSelection::Tilts(vec![0.9])),
        "0.9° must not reach the cut below the horizon",
    );
}

/// The mapping this rests on, against the volume shape measured on the live
/// bucket: KTLX VCP 35, twelve cuts, six chunks each for the six
/// super-resolution ones and three for the six standard ones, sealing on
/// sequences 7, 13, 19, 25, 31, 37, 40, 43, 46, 49, 52, 55.
#[test]
fn the_chunk_map_reproduces_a_measured_volume() {
    let mut cuts: Vec<(f32, bool)> = (0..6).map(|i| (0.5 + i as f32 * 0.4, true)).collect();
    cuts.extend((0..6).map(|i| (1.8 + i as f32 * 0.9, false)));
    let vcp = vcp_with(&cuts);
    let map = ElevationChunkMap::from_coverage_pattern(&vcp).expect("a pattern with cuts");
    assert_eq!(map.cut_count(), 12);

    // The start chunk belongs to no cut.
    assert_eq!(map.cut_for(1), None);
    // Each cut ends on the sequence the live volume sealed it on.
    for (elevation, last) in [
        (1u8, 7u16),
        (2, 13),
        (3, 19),
        (4, 25),
        (5, 31),
        (6, 37),
        (7, 40),
        (8, 43),
        (9, 46),
        (10, 49),
        (11, 52),
        (12, 55),
    ] {
        assert_eq!(
            map.cut_for(last).map(|(e, _)| e),
            Some(elevation),
            "sequence {last} should be the end of cut {elevation}"
        );
        assert_ne!(
            map.cut_for(last + 1).map(|(e, _)| e),
            Some(elevation),
            "cut {elevation} ran past sequence {last}"
        );
    }
}

/// The traffic claim, stated as a number: a pane on the lowest tilt needs
/// thirteen chunks of fifty-five.
#[test]
fn one_tilt_wants_a_fraction_of_the_volume() {
    let mut cuts: Vec<(f32, bool)> = (0..2).map(|_| (0.5, true)).collect();
    cuts.extend((0..4).map(|i| (1.3 + i as f32 * 0.4, true)));
    cuts.extend((0..6).map(|i| (1.8 + i as f32 * 0.9, false)));
    let vcp = vcp_with(&cuts);
    let map = ElevationChunkMap::from_coverage_pattern(&vcp).expect("cuts");

    let low = CutSelection::Tilts(vec![0.5]);
    let wanted = (1..=55).filter(|s| map.wants(*s, &low)).count();
    assert_eq!(
        wanted, 13,
        "a 0.5° pane should want the start chunk and both halves of the \
             split cut, and nothing else"
    );
    assert_eq!(
        (1..=55)
            .filter(|s| map.wants(*s, &CutSelection::All))
            .count(),
        55,
        "asking for everything must still take everything"
    );
}

/// The start chunk is never skipped: it carries the coverage pattern, and
/// without it there is no map to skip anything by.
#[test]
fn the_start_chunk_is_always_wanted() {
    let vcp = vcp_with(&[(0.5, true), (4.0, false)]);
    let map = ElevationChunkMap::from_coverage_pattern(&vcp).expect("cuts");
    assert!(map.wants(1, &CutSelection::Tilts(vec![90.0])));
    // And so is a sequence past the end of the pattern — skipping something
    // unplaceable would be guessing.
    assert!(map.wants(999, &CutSelection::Tilts(vec![90.0])));
}

/// A placeholder pattern lists no cuts, so nothing can be mapped and nothing
/// may be skipped.
#[test]
fn a_pattern_with_no_cuts_yields_no_map() {
    assert!(ElevationChunkMap::from_coverage_pattern(&placeholder_coverage_pattern(212)).is_none());
}

/// Before the start chunk lands there is no map, so every chunk is wanted
/// whatever the selection says.
#[test]
fn nothing_is_skipped_until_the_coverage_pattern_arrives() {
    let mut a = VolumeAssembler::new("KTLX", vol(42));
    a.set_selection(CutSelection::Tilts(vec![0.5]));
    for sequence in 1..=55 {
        assert!(
            a.wants_chunk(sequence),
            "chunk {sequence} was skipped with no coverage pattern to judge by"
        );
    }
}

/// The split cut's other half is wanted too: both halves sit at the same
/// angle, and a pane switching from reflectivity to velocity must not have
/// to wait for the next volume.
#[test]
fn both_halves_of_a_split_cut_are_wanted_together() {
    let vcp = vcp_with(&[(0.5, true), (0.5, true), (4.0, false)]);
    let map = ElevationChunkMap::from_coverage_pattern(&vcp).expect("cuts");
    let low = CutSelection::Tilts(vec![0.5]);
    assert_eq!(map.wanted_elevations(&low), vec![1, 2]);
}

/// With cuts deliberately skipped, "complete" means every cut that was asked
/// for — contiguity is meaningless once there are holes by design.
#[test]
fn a_selective_volume_completes_on_the_cuts_it_asked_for() {
    let vcp = vcp_with(&[(0.5, true), (4.0, false), (6.0, false)]);
    let mut a = VolumeAssembler::new("KTLX", vol(42));
    a.set_selection(CutSelection::Tilts(vec![0.5]));

    // Start chunk carrying the pattern, then the 0.5° cut, then the volume
    // ends — cuts 2 and 3 never arrive because they were never fetched.
    a.ingest_contents(
        1,
        ChunkKind::Start,
        volume_time(),
        ChunkContents {
            radials: Vec::new(),
            coverage_pattern: Some(vcp),
            ..Default::default()
        },
    );
    assert!(!a.is_volume_complete(), "nothing has been assembled yet");

    let scan = crate::volumetric::tests::golden_scan();
    let sweep = &scan.sweeps()[0];
    let radials: Vec<Radial> = sweep
        .radials()
        .iter()
        .enumerate()
        .map(|(i, r)| {
            if i + 1 == sweep.radials().len() {
                with_status(r, RadialStatus::ScanEnd)
            } else {
                r.clone()
            }
        })
        .collect();
    for (n, group) in radials.chunks(120).enumerate() {
        a.ingest_contents(
            2 + n as u16,
            ChunkKind::Intermediate,
            volume_time(),
            ChunkContents {
                radials: group.to_vec(),
                coverage_pattern: None,
                ..Default::default()
            },
        );
    }

    assert!(
        a.is_volume_complete(),
        "the only cut asked for is sealed and the volume ended, but it did \
             not report complete: {:?}",
        a.progress()
    );
}

/// The pairing that keeps a selective volume away from `compute_echo_tops`:
/// under `All`, a volume with a hole is not complete.
///
/// Contiguity is what carries this rather than the planned cut list, and
/// deliberately so — SAILS and MRLE cuts are inserted at runtime, so the
/// number of sweeps a volume produces need not match the number its message 5
/// lists, and requiring the planned set could leave a volume that will never
/// complete. `ScanEnd` marks the true final cut, so seeing it with 1..=n
/// sealed and no gaps genuinely is the whole volume.
#[test]
fn a_volume_with_a_hole_is_incomplete_when_everything_was_asked_for() {
    let scan = crate::volumetric::tests::golden_scan();
    let mut a = VolumeAssembler::new("KTLX", vol(42));
    a.set_selection(CutSelection::All);
    a.ingest_contents(
        1,
        ChunkKind::Start,
        volume_time(),
        ChunkContents {
            radials: Vec::new(),
            coverage_pattern: Some(crate::volumetric::tests::vcp()),
            ..Default::default()
        },
    );

    // Every cut but the second, with the last radial of the last one marking
    // the end of the volume — the shape a lost chunk leaves behind.
    let sweeps = scan.sweeps();
    let mut sequence = 2u16;
    for (si, sweep) in sweeps.iter().enumerate() {
        if si == 1 {
            continue;
        }
        let last_sweep = si + 1 == sweeps.len();
        let radials: Vec<Radial> = sweep
            .radials()
            .iter()
            .enumerate()
            .map(|(i, r)| {
                if i + 1 == sweep.radials().len() {
                    with_status(
                        r,
                        if last_sweep {
                            RadialStatus::ScanEnd
                        } else {
                            RadialStatus::ElevationEnd
                        },
                    )
                } else {
                    r.clone()
                }
            })
            .collect();
        for group in radials.chunks(120) {
            a.ingest_contents(
                sequence,
                ChunkKind::Intermediate,
                volume_time(),
                ChunkContents {
                    radials: group.to_vec(),
                    coverage_pattern: None,
                    ..Default::default()
                },
            );
            sequence += 1;
        }
    }

    assert!(a.progress().saw_scan_end, "the fixture must reach the end");
    assert!(
        !a.is_volume_complete(),
        "a volume missing cut 2 reported complete, so `compute_echo_tops` \
             would integrate a volume with a hole in it: {:?}",
        a.progress()
    );
}

/// Widening the selection mid-volume backfills within it rather than waiting
/// for the next one — the skipped chunks are still in the bucket.
#[test]
fn widening_the_selection_makes_the_skipped_chunks_wanted_again() {
    let vcp = vcp_with(&[(0.5, true), (4.0, false)]);
    let mut a = VolumeAssembler::new("KTLX", vol(42));
    a.ingest_contents(
        1,
        ChunkKind::Start,
        volume_time(),
        ChunkContents {
            radials: Vec::new(),
            coverage_pattern: Some(vcp),
            ..Default::default()
        },
    );

    a.set_selection(CutSelection::Tilts(vec![0.5]));
    assert!(!a.wants_chunk(8), "the 4.0° cut should be skipped");
    a.set_selection(CutSelection::Tilts(vec![0.5, 4.0]));
    assert!(
        a.wants_chunk(8),
        "a pane switching to 4.0° must not wait for the next volume"
    );
}

// -- live ---------------------------------------------------------------
//
// Run with:
//   cargo test -p rustdar-radar --lib -- --ignored --nocapture chunks::tests::live_

/// The claim this whole module rests on, mirroring
/// `archive::tests::live_listing_needs_no_credentials`. If the chunk bucket
/// ever required signing, every other live test here would fail with a
/// confusing decode error while this one names the cause.
#[ignore = "hits the live unidata-nexrad-level2-chunks S3 bucket"]
#[tokio::test]
async fn live_chunk_bucket_allows_anonymous_listing() {
    let sources = crate::sources::DataSources::production();
    crate::tls::init();
    let url = crate::archive::list_url_delimited(&sources.level2_chunks_bucket, "KTLX/", "/", None)
        .expect("url");
    let response = crate::archive::shared_client()
        .get(&url)
        .send()
        .await
        .expect("request should reach S3");
    println!("anonymous delimited LIST -> {}", response.status());
    assert!(
        response.status().is_success(),
        "anonymous listing was refused; this module would need SigV4"
    );
}

/// Discovery lands on a volume the radar is actually writing.
///
/// Catches the bucket dropping `CommonPrefixes`, the search regressing to a
/// linear scan, the name format changing, and — the reason the ladder is
/// sorted numerically — a regression to lexicographic order, which would
/// pick a volume hours or days stale.
#[ignore = "hits the live unidata-nexrad-level2-chunks S3 bucket"]
#[tokio::test]
async fn live_discovery_finds_a_current_volume() {
    let sources = crate::sources::DataSources::production();
    crate::tls::init();

    let indices = list_volume_indices(&sources, "KTLX")
        .await
        .expect("listing volume directories");
    println!("KTLX has {} volume directories", indices.len());
    assert!(
        !indices.is_empty(),
        "no volume directories; the delimiter or prefix is wrong"
    );

    let volume = latest_volume(&sources, "KTLX").await.expect("discovery");
    let chunks = list_chunks(&sources, "KTLX", volume)
        .await
        .expect("listing the current volume");
    let newest = chunks.last().expect("a current volume holds chunks");
    let age = chrono::Utc::now().naive_utc() - newest.volume_time();
    println!(
        "volume {} started {} ({} min ago), {} chunks, newest {}",
        volume.get(),
        newest.volume_time(),
        age.num_minutes(),
        chunks.len(),
        newest.name(),
    );
    assert!(
        age < chrono::Duration::minutes(30),
        "discovery picked a volume {} minutes old; it is not the write head",
        age.num_minutes()
    );
}

/// A volume's directory holds a start chunk at sequence 1 and its chunks
/// come back in sequence order.
#[ignore = "hits the live unidata-nexrad-level2-chunks S3 bucket"]
#[tokio::test]
async fn live_a_volume_starts_at_sequence_one_and_is_ordered() {
    let sources = crate::sources::DataSources::production();
    crate::tls::init();
    let volume = latest_volume(&sources, "KTLX").await.expect("discovery");
    let chunks = list_chunks(&sources, "KTLX", volume)
        .await
        .expect("listing");

    assert!(!chunks.is_empty(), "the current volume listed no chunks");
    assert!(
        chunks.len() <= 200,
        "a volume should hold roughly 55 chunks, got {}",
        chunks.len()
    );
    let sequences: Vec<u16> = chunks.iter().map(ChunkId::sequence).collect();
    let mut sorted = sequences.clone();
    sorted.sort_unstable();
    assert_eq!(sequences, sorted, "list_chunks returned them out of order");

    let first = &chunks[0];
    println!(
        "first chunk: {} (seq {}, {:?})",
        first.name(),
        first.sequence(),
        first.kind()
    );
    assert_eq!(first.sequence(), 1);
    assert_eq!(
        first.kind(),
        ChunkKind::Start,
        "sequence 1 must be the start chunk — it is the only carrier of the \
             coverage pattern"
    );
}

/// The poller end to end: discover, fill, seal.
///
/// Two rounds a few seconds apart, which is the cadence the frontend will
/// drive. Prints the age of each cut's freshest radial at the moment it
/// became renderable — **the measurement the whole feature is for**, against
/// the archive path's five to seven minutes.
#[ignore = "hits the live unidata-nexrad-level2-chunks S3 bucket"]
#[tokio::test]
async fn live_a_few_poll_rounds_assemble_and_seal() {
    let sources = crate::sources::DataSources::production();
    crate::tls::init();
    let mut poller = ChunkPoller::new("KTLX");

    // Polled to a deadline rather than a fixed round count: a cut is 3 or 6
    // chunks and the radar takes ~30-40 s over one, so a handful of rounds
    // can legitimately seal nothing.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(150);
    let mut total_sealed = 0usize;
    let mut round = 0;
    while std::time::Instant::now() < deadline && total_sealed == 0 {
        round += 1;
        let outcome = poller.poll(&sources).await.expect("a poll round");
        let progress = outcome.progress.clone().expect("a volume is being tracked");
        println!(
            "round {round}: volume {} | ingested {} | sealed {:?} | {} cuts so far | complete={} | next in {:?}",
            poller.volume().expect("a volume").get(),
            outcome.ingested,
            outcome.sealed_elevations,
            progress.sealed_elevations.len(),
            progress.volume_complete,
            poller.suggested_interval(),
        );
        for (elevation, angle) in outcome
            .sealed_elevations
            .iter()
            .zip(outcome.sealed_angles.iter())
        {
            let scan = poller.snapshot().expect("a snapshot after a seal");
            let age = scan
                .sweeps()
                .iter()
                .find(|s| s.elevation_number() == *elevation)
                .and_then(|s| s.radials().iter().map(|r| r.collection_timestamp()).max())
                .and_then(chrono::DateTime::from_timestamp_millis)
                .map(|t| (chrono::Utc::now() - t).num_seconds());
            println!(
                "   elev {elevation} ({angle:.2}°) renderable, data age {}s",
                age.unwrap_or(-1)
            );
        }
        total_sealed += outcome.sealed_elevations.len();
        if total_sealed == 0 {
            tokio::time::sleep(poller.suggested_interval()).await;
        }
    }

    let progress = poller.progress().expect("a volume");
    assert!(
        total_sealed > 0,
        "{round} rounds over 150s produced no complete cut; the feed is \
             ingesting {} chunks but nothing ever seals",
        progress.chunks_ingested
    );
    assert_eq!(
        progress.late_radials_dropped, 0,
        "radials arrived for a cut that had already sealed, which means \
             elevation numbers repeat within a volume"
    );
    // Every sweep handed out is a full rotation.
    if let Some(scan) = poller.snapshot() {
        for sweep in scan.sweeps() {
            let spacing = sweep
                .radials()
                .first()
                .map(|r| r.azimuth_spacing_degrees())
                .unwrap_or(1.0);
            let expected = (360.0 / spacing).round() as usize;
            assert!(
                sweep.radials().len() * 100 >= expected * MIN_SEALED_RADIAL_PERCENT,
                "elevation {} reached the snapshot with {}/{} radials",
                sweep.elevation_number(),
                sweep.radials().len(),
                expected
            );
        }
    }
}

/// End to end: the start chunk decodes, and it is where the VCP comes from.
#[ignore = "hits the live unidata-nexrad-level2-chunks S3 bucket"]
#[tokio::test]
async fn live_a_start_chunk_decodes_and_carries_the_coverage_pattern() {
    let sources = crate::sources::DataSources::production();
    crate::tls::init();
    let volume = latest_volume(&sources, "KTLX").await.expect("discovery");
    let chunks = list_chunks(&sources, "KTLX", volume)
        .await
        .expect("listing");

    let start = chunks
        .iter()
        .find(|c| c.kind() == ChunkKind::Start)
        .expect("a start chunk");
    let bytes = download_chunk(&sources, start).await.expect("download");
    println!("{} is {} bytes", start.name(), bytes.len());
    let contents = decode_chunk(start.name(), &bytes).expect("decode");
    let vcp = contents
        .coverage_pattern
        .expect("the start chunk carries message 5");
    println!(
        "VCP {} with {} planned cuts, {} radials in the start chunk",
        vcp.pattern_number().number(),
        vcp.elevation_cuts().len(),
        contents.radials.len(),
    );
    assert!(
        !vcp.elevation_cuts().is_empty(),
        "the reconstructed VCP has no elevation cuts"
    );

    // And an intermediate chunk decodes to radials without any header.
    if let Some(mid) = chunks.iter().find(|c| c.kind() == ChunkKind::Intermediate) {
        let bytes = download_chunk(&sources, mid).await.expect("download");
        let contents = decode_chunk(mid.name(), &bytes).expect("decode");
        println!(
            "{} is {} bytes -> {} radials",
            mid.name(),
            bytes.len(),
            contents.radials.len()
        );
        assert!(
            !contents.radials.is_empty(),
            "an intermediate chunk decoded to no radials"
        );
    }
}
