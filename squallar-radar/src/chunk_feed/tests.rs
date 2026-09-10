use super::*;

fn outcome() -> Result<PollOutcome, String> {
    Ok(PollOutcome {
        ingested: 1,
        ..Default::default()
    })
}

#[test]
fn a_round_in_flight_does_not_take_the_snapshot_with_it() {
    let volume = stub_volume();
    let mut mgr = ChunkFeedManager::new();
    mgr.ensure("KICT");
    mgr.feeds.get_mut("KICT").expect("ensured").last_snapshot = Some(LiveVolume {
        scan: std::sync::Arc::clone(&volume),
        declared: Default::default(),
        bytes: crate::scan_size::scan_bytes(&volume) as u64,
    });

    mgr.force_due("KICT");
    let poller = mgr.take_for_round("KICT").expect("the poller leaves");
    let held = mgr
        .snapshot("KICT")
        .expect("the volume vanished for the duration of the round");
    assert!(
        std::sync::Arc::ptr_eq(&held.scan, &volume),
        "the bridge must serve the very volume the last frame resolved",
    );

    mgr.finish_round("KICT", poller, &empty());
    assert!(
        mgr.snapshot("KICT").is_none(),
        "a poller home with no volume yet answers None, and a bridge that \
             never refreshes would overrule it with the stale copy",
    );

    mgr.force_due("KICT");
    let poller = mgr.take_for_round("KICT").expect("the next round leaves");
    assert!(
        mgr.snapshot("KICT").is_none(),
        "the poller-home refresh never reached the bridge, so the round \
             serves a volume no frame has resolved since",
    );
    mgr.finish_round("KICT", poller, &empty());
}

fn empty() -> Result<PollOutcome, String> {
    Ok(PollOutcome::default())
}

fn stub_volume() -> std::sync::Arc<nexrad_model::data::Scan> {
    use nexrad_model::data::{PulseWidth, Scan, VolumeCoveragePattern};
    std::sync::Arc::new(Scan::new(
        VolumeCoveragePattern::new(
            212,
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
            Vec::new(),
        ),
        Vec::new(),
    ))
}

#[test]
fn a_retired_feed_serves_no_snapshot() {
    let volume = stub_volume();
    let mut mgr = ChunkFeedManager::new();
    mgr.ensure("KICT");
    mgr.feeds.get_mut("KICT").expect("ensured").last_snapshot = Some(LiveVolume {
        scan: std::sync::Arc::clone(&volume),
        declared: Default::default(),
        bytes: crate::scan_size::scan_bytes(&volume) as u64,
    });
    mgr.force_due("KICT");
    let _poller = mgr.take_for_round("KICT").expect("the poller leaves");
    assert!(
        mgr.snapshot("KICT").is_some(),
        "precondition: the feed is serving the volume its flight assembled",
    );

    mgr.force_retire_at("KICT", std::time::Duration::from_secs(1));
    assert!(
        mgr.snapshot("KICT").is_none(),
        "a retired feed kept serving its frozen partial volume, so every \
             consumer merges a dead flight's low tilts over a rolling base",
    );
}

#[test]
fn retirement_drops_the_bridge_copy_and_recovery_starts_fresh() {
    let mut mgr = ChunkFeedManager::new();
    mgr.ensure("KICT");
    mgr.feeds.get_mut("KICT").expect("ensured").last_snapshot = Some(LiveVolume {
        scan: stub_volume(),
        declared: Default::default(),
        bytes: 0,
    });

    mgr.force_stall("KICT");
    let poller = take(&mut mgr, "KICT");
    assert_eq!(
        mgr.finish_round("KICT", poller, &empty()),
        Some(Retirement::Stalled),
        "precondition: this is the real retirement path",
    );
    assert!(
        mgr.feeds
            .get("KICT")
            .expect("still present")
            .last_snapshot
            .is_none(),
        "retirement left the bridge copy in hand; the retired gate is then \
             the only thing between it and every consumer",
    );
    assert!(mgr.snapshot("KICT").is_none());

    mgr.force_retire_at("KICT", RETRY_AFTER + std::time::Duration::from_secs(1));
    mgr.ensure("KICT");
    assert!(mgr.is_feeding("KICT"), "the retry window has passed");
    assert!(
        mgr.snapshot("KICT").is_none(),
        "a fresh flight has assembled nothing yet; anything else is the \
             dead flight's volume back from the grave",
    );
    mgr.force_due("KICT");
    assert!(
        mgr.take_for_round("KICT").is_some(),
        "recovery must resume rounds, so the fresh flight's overlay can \
             merge again",
    );
}

fn take(mgr: &mut ChunkFeedManager, site: &str) -> Box<ChunkPoller> {
    mgr.force_due(site);
    mgr.take_for_round(site).expect("a round was available")
}

#[test]
fn one_round_per_site_is_in_flight_at_a_time() {
    let mut mgr = ChunkFeedManager::new();
    mgr.ensure("KTLX");
    let poller = take(&mut mgr, "KTLX");
    mgr.force_due("KTLX");
    assert!(
        mgr.take_for_round("KTLX").is_none(),
        "a second round was dispatched while the first was still in the air, \
             so the interval is the only thing serialising rounds"
    );
    assert!(mgr.any_in_flight());
    mgr.finish_round("KTLX", poller, &outcome());
    assert!(!mgr.any_in_flight());
}

#[test]
fn an_empty_round_is_not_an_error() {
    let mut mgr = ChunkFeedManager::new();
    mgr.ensure("KTLX");
    for _ in 0..10 {
        let poller = take(&mut mgr, "KTLX");
        assert_eq!(mgr.finish_round("KTLX", poller, &empty()), None);
    }
    assert!(mgr.is_feeding("KTLX"));
}

#[test]
fn three_consecutive_errors_retire_a_site_and_two_do_not() {
    let mut mgr = ChunkFeedManager::new();
    mgr.ensure("KTLX");
    let err = Err("boom".to_string());

    for _ in 0..2 {
        let poller = take(&mut mgr, "KTLX");
        assert_eq!(mgr.finish_round("KTLX", poller, &err), None);
    }
    assert!(mgr.is_feeding("KTLX"), "two failures is not enough");

    let poller = take(&mut mgr, "KTLX");
    assert_eq!(
        mgr.finish_round("KTLX", poller, &err),
        Some(Retirement::Errors)
    );
    assert!(!mgr.is_feeding("KTLX"));
    mgr.force_due("KTLX");
    assert!(
        mgr.take_for_round("KTLX").is_none(),
        "a retired site kept polling"
    );
}

#[test]
fn a_successful_round_clears_the_error_count() {
    let mut mgr = ChunkFeedManager::new();
    mgr.ensure("KTLX");
    let err = Err("boom".to_string());
    for _ in 0..2 {
        let poller = take(&mut mgr, "KTLX");
        mgr.finish_round("KTLX", poller, &err);
    }
    let poller = take(&mut mgr, "KTLX");
    mgr.finish_round("KTLX", poller, &outcome());
    for _ in 0..2 {
        let poller = take(&mut mgr, "KTLX");
        assert_eq!(mgr.finish_round("KTLX", poller, &err), None);
    }
    assert!(mgr.is_feeding("KTLX"));
}

#[test]
fn a_feed_that_makes_no_progress_retires() {
    let mut mgr = ChunkFeedManager::new();
    mgr.ensure("KTLX");
    mgr.force_stall("KTLX");
    let poller = take(&mut mgr, "KTLX");
    assert_eq!(
        mgr.finish_round("KTLX", poller, &empty()),
        Some(Retirement::Stalled)
    );
}

#[test]
fn a_retired_site_is_retried_only_after_the_window() {
    let mut mgr = ChunkFeedManager::new();
    mgr.ensure("KTLX");
    mgr.force_retire_at("KTLX", std::time::Duration::from_secs(60));
    mgr.ensure("KTLX");
    assert!(!mgr.is_feeding("KTLX"), "the retry window has not passed");

    mgr.force_retire_at("KTLX", RETRY_AFTER + std::time::Duration::from_secs(1));
    mgr.ensure("KTLX");
    assert!(mgr.is_feeding("KTLX"));
}

#[test]
fn feeds_for_sites_no_pane_watches_are_dropped() {
    let mut mgr = ChunkFeedManager::new();
    mgr.ensure("KTLX");
    mgr.ensure("KOUN");
    assert_eq!(mgr.feed_count(), 2);
    mgr.retain_live(&["KTLX".to_string()]);
    assert_eq!(mgr.feed_count(), 1);
    assert!(mgr.is_feeding("KTLX"));
    assert!(!mgr.is_feeding("KOUN"));
}

#[test]
fn a_round_landing_after_its_site_was_dropped_is_discarded() {
    let mut mgr = ChunkFeedManager::new();
    mgr.ensure("KTLX");
    let poller = take(&mut mgr, "KTLX");
    mgr.retain_live(&[]);
    assert_eq!(mgr.finish_round("KTLX", poller, &outcome()), None);
    assert_eq!(mgr.feed_count(), 0);
}

/// **A bridge copy the frame thread stops holding must leave through the
/// queue, not through its own `drop`.**
///
/// `ChunkFeedManager::snapshot` runs on the frame thread several times a
/// frame. The moment the poller has rebuilt since the last one it hands back a
/// different `Arc`, and by then the assembler has let go of the volume the
/// bridge is holding — `VolumeAssembler::snapshot` takes `cached` out with
/// `take()` and either unwraps it or clones the sweeps out of it, and drops
/// its own reference either way. So the bridge is the last owner, and
/// `clone_from` freed the whole thing where it stood.
///
/// Measured on a VCP 212-shaped volume (17 cuts, 7,560 radials, six moments):
/// **45,379 deallocations and 58.43 MiB**, on the frame thread, per rebuild.
///
/// The fixture carries real gate arrays for that reason — a volume of empty
/// radials frees a few hundred bytes and could not show this whatever the code
/// did — and the assertions below check that it does.
#[test]
fn a_superseded_bridge_copy_leaves_through_the_queue_rather_than_the_frame_thread() {
    const SITE: &str = "KTLX";
    let mut mgr = mgr_assembling(SITE);

    // The start chunk, then a whole cut: the assembler seals on the radial
    // that carries `ElevationEnd`, which is what makes a snapshot buildable.
    ingest(&mut mgr, SITE, 1, ChunkKind::Start, start_chunk());
    ingest(&mut mgr, SITE, 2, ChunkKind::Intermediate, cut(1, 0.5));

    let first = mgr
        .snapshot(SITE)
        .expect("a sealed cut is a volume the bridge can serve");
    let superseded = std::sync::Arc::clone(&first.scan);
    assert!(
        first.bytes > 1 << 20,
        "fixture: the bridge copy is priced at {} B, so nothing built on it \
         could show a whole-volume free",
        first.bytes,
    );
    drop(first);

    // The rebuild the round would do on the poller's thread: a second cut
    // seals, and the assembler builds a new `Scan` and lets go of the old one.
    ingest(&mut mgr, SITE, 3, ChunkKind::Intermediate, cut(2, 0.9));
    let rebuilt = mgr
        .feeds
        .get_mut(SITE)
        .expect("ensured")
        .poller
        .as_mut()
        .expect("the poller is home")
        .snapshot()
        .expect("the second cut sealed");
    assert!(
        !std::sync::Arc::ptr_eq(&rebuilt, &superseded),
        "fixture: the assembler handed back the same volume, so nothing was \
         superseded and this test asserts nothing",
    );
    drop(rebuilt);
    assert_eq!(
        std::sync::Arc::strong_count(&superseded),
        2,
        "fixture: this test's own handle and the bridge must be the only \
         owners left, or the frame-thread `drop` under test would have been a \
         refcount decrement rather than a whole-volume free",
    );

    // The next frame's ask. The bridge lets go of the old volume here.
    let second = mgr.snapshot(SITE).expect("the poller is still home");
    assert!(
        !std::sync::Arc::ptr_eq(&second.scan, &superseded),
        "precondition: the bridge is still serving the old volume",
    );

    let carried = mgr.take_superseded();
    assert_eq!(
        carried.len(),
        1,
        "the bridge copy was freed on the frame thread instead of being \
         carried out: 45,379 deallocations and 58.43 MiB of per-radial \
         buffers, inside `publish_base_volumes`",
    );
    assert!(
        std::sync::Arc::ptr_eq(&carried[0].scan, &superseded),
        "something other than the superseded bridge copy came out of the queue",
    );
    assert!(
        mgr.take_superseded().is_empty(),
        "the queue hands its entries out twice, so the volume is freed once \
         and priced against the drop budget again",
    );
}

/// **The warm case queues nothing.** The poller hands back the same `Arc`
/// every frame it has not rebuilt on, and a queue that filed those would turn
/// a refcount decrement into an eviction — and hold a live volume against the
/// drop budget on every frame of a quiet leg.
#[test]
fn a_bridge_copy_that_did_not_move_is_not_queued() {
    const SITE: &str = "KTLX";
    let mut mgr = mgr_assembling(SITE);
    ingest(&mut mgr, SITE, 1, ChunkKind::Start, start_chunk());
    ingest(&mut mgr, SITE, 2, ChunkKind::Intermediate, cut(1, 0.5));

    let first = mgr.snapshot(SITE).expect("a sealed cut");
    let held = std::sync::Arc::clone(&first.scan);
    drop(first);
    let second = mgr.snapshot(SITE).expect("still there");
    assert!(
        std::sync::Arc::ptr_eq(&second.scan, &held),
        "fixture: the poller rebuilt with nothing ingested between the asks",
    );
    drop(second);

    assert!(
        mgr.take_superseded().is_empty(),
        "a frame that superseded nothing queued a volume anyway",
    );
}

use crate::chunks::{ChunkContents, ChunkKind};

/// A whole cut: `RADIALS` radials of real gate arrays, the last carrying the
/// terminator that seals it.
const RADIALS: usize = 360;
/// Reflectivity's own gate count at 250 m, and the Doppler moments' — the
/// fixture's payload bytes, at the shape a real cut carries them in.
const REFL_GATES: usize = 1832;
const DOPPLER_GATES: usize = 1192;

fn chunk_time() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 9, 9)
        .expect("a real date")
        .and_hms_opt(7, 22, 0)
        .expect("a real time")
}

fn start_chunk() -> ChunkContents {
    ChunkContents {
        radials: Vec::new(),
        coverage_pattern: Some(crate::volumetric::tests::vcp()),
        ..Default::default()
    }
}

fn moment(gates: usize) -> nexrad_model::data::MomentData {
    nexrad_model::data::MomentData::from_fixed_point(
        gates as u16,
        2125,
        250,
        8,
        2.0,
        66.0,
        vec![7u8; gates],
    )
}

fn cut(elevation_number: u8, elevation: f32) -> ChunkContents {
    use nexrad_model::data::{Radial, RadialStatus};
    let spacing = 360.0 / RADIALS as f32;
    let radials = (0..RADIALS)
        .map(|i| {
            let status = if i + 1 == RADIALS {
                RadialStatus::ElevationEnd
            } else {
                RadialStatus::IntermediateRadialData
            };
            Radial::new(
                1_760_000_000_000 + i as i64,
                i as u16 + 1,
                i as f32 * spacing,
                spacing,
                status,
                elevation_number,
                elevation,
                Some(moment(REFL_GATES)),
                Some(moment(DOPPLER_GATES)),
                Some(moment(DOPPLER_GATES)),
                Some(moment(DOPPLER_GATES)),
                Some(moment(DOPPLER_GATES)),
                Some(moment(DOPPLER_GATES)),
                None,
            )
        })
        .collect();
    ChunkContents {
        radials,
        coverage_pattern: None,
        ..Default::default()
    }
}

/// A manager whose feed for `site` has a poller already on a volume — the
/// state a feed reaches after its discovery round, and the only one from
/// which a chunk can be ingested at all.
fn mgr_assembling(site: &str) -> ChunkFeedManager {
    let mut mgr = ChunkFeedManager::new();
    mgr.ensure(site);
    mgr.feeds.get_mut(site).expect("ensured").poller =
        Some(Box::new(crate::chunks::ChunkPoller::resume(
            site,
            crate::chunks::VolumeIndex::new(42).expect("a real volume index"),
        )));
    mgr
}

fn ingest(
    mgr: &mut ChunkFeedManager,
    site: &str,
    sequence: u16,
    kind: ChunkKind,
    contents: ChunkContents,
) {
    mgr.feeds
        .get_mut(site)
        .expect("ensured")
        .poller
        .as_mut()
        .expect("the poller is home")
        .assembler_mut()
        .expect("a volume is being assembled")
        .ingest_contents(sequence, kind, chunk_time(), contents);
}

/// **Both arms of the rebuild counter, against the real bridge.**
///
/// `crate::chunks::rebuild_totals` is the only thing that can tell a landed
/// ownership change from one that executes zero times, so it is gated on a
/// rebuild that really copies and a rebuild that really moves — the same
/// assembler, the same fixture, the bridge the only difference between them.
/// A counter wired to a constant, or an arm nothing can reach, fails here.
///
/// Deltas and not absolutes: the totals are process-wide and this crate's
/// lib-test binary is multi-threaded. `feed_level_serial::exclusive` covers
/// the window, which is why `record_rebuild` writes through the same
/// serialiser the byte level does.
#[test]
fn the_rebuild_counter_separates_a_copied_volume_from_a_moved_one() {
    const SITE: &str = "KTLX";
    let _exclusive = crate::chunks::feed_level_serial::exclusive();
    let mut mgr = mgr_assembling(SITE);
    ingest(&mut mgr, SITE, 1, ChunkKind::Start, start_chunk());
    ingest(&mut mgr, SITE, 2, ChunkKind::Intermediate, cut(1, 0.5));

    // The frame thread's ask. It leaves the bridge holding the built volume,
    // which is the second owner the next rebuild will find.
    let first = mgr.snapshot(SITE).expect("a sealed cut is a volume");
    let priced = first.bytes;
    assert!(
        priced > 1 << 20,
        "fixture: the volume is priced at {priced} B, so a copy of it would \
         not show up as a whole-volume copy",
    );
    drop(first);

    // ---- the copy arm: the bridge is holding it -------------------------
    let before = crate::chunks::rebuild_totals();
    ingest(&mut mgr, SITE, 3, ChunkKind::Intermediate, cut(2, 0.9));
    rebuild(&mut mgr, SITE);
    let after_copy = crate::chunks::rebuild_totals();
    assert_eq!(
        after_copy.copies - before.copies,
        1,
        "a rebuild with the bridge holding the volume did not count as a copy",
    );
    assert_eq!(
        after_copy.moves, before.moves,
        "the move arm counted a rebuild that cloned the volume",
    );
    assert_eq!(
        after_copy.copied_bytes - before.copied_bytes,
        priced,
        "the copy was priced at something other than what the volume holds",
    );
    assert!(
        after_copy.copied_blocks - before.copied_blocks > 1_000,
        "a whole cut of real gate arrays was copied in {} blocks; the block \
         count is not counting gate buffers",
        after_copy.copied_blocks - before.copied_blocks,
    );

    // The gate term of that copy, which `nexrad_model::data::GateBuffer`
    // turned into refcount bumps. It is the whole point of the change and it
    // is asserted as a proportion rather than a byte count, because the
    // fixture's shape is free to move: what must not move is that nearly all
    // of a copied volume is gates and that the copy no longer allocates them.
    let shared_bytes = after_copy.shared_bytes - before.shared_bytes;
    let shared_blocks = after_copy.shared_blocks - before.shared_blocks;
    assert!(
        shared_bytes > 0 && shared_blocks > 0,
        "the copy reported {shared_bytes} B in {shared_blocks} blocks shared; \
         the gate buffers are being copied, or the arm is not wired",
    );
    assert!(
        shared_bytes * 10 > priced * 9,
        "of {priced} B copied only {shared_bytes} B were shared, under 90 %; \
         the gate term is not what the clone is skipping",
    );
    assert!(
        after_copy.copied_blocks - before.copied_blocks - shared_blocks < 100,
        "the copy still allocated {} blocks after the shared ones are taken \
         out; a clone of a volume should be its containers and nothing else",
        after_copy.copied_blocks - before.copied_blocks - shared_blocks,
    );

    // ---- the move arm: nothing else holds it ----------------------------
    // Exactly the counterfactual an ownership fix would create. It is done
    // here by hand because no shipped path produces it: the rebuild runs
    // inside a round, which is the window the bridge must be held.
    mgr.feeds.get_mut(SITE).expect("ensured").last_snapshot = None;
    assert!(
        mgr.take_superseded().len() <= 1,
        "the queue is drained so the superseded volumes it holds cannot be \
         the second owner this half of the test is about",
    );
    let before_move = crate::chunks::rebuild_totals();
    ingest(&mut mgr, SITE, 4, ChunkKind::Intermediate, cut(3, 1.3));
    rebuild(&mut mgr, SITE);
    let after_move = crate::chunks::rebuild_totals();
    assert_eq!(
        after_move.moves - before_move.moves,
        1,
        "a rebuild whose assembler was the volume's last owner still copied",
    );
    assert_eq!(
        after_move.copies, before_move.copies,
        "the copy arm counted a rebuild that moved the volume",
    );
    assert!(
        after_move.moved_bytes - before_move.moved_bytes > 1 << 20,
        "the move was priced at {} B, so nothing was reported as not copied",
        after_move.moved_bytes - before_move.moved_bytes,
    );
    assert_eq!(
        (after_move.shared_bytes, after_move.shared_blocks),
        (after_copy.shared_bytes, after_copy.shared_blocks),
        "a rebuild that MOVED filed shared bytes; nothing was cloned to share",
    );
    assert!(
        after_move.moved_blocks - before_move.moved_blocks > 1_000,
        "the move reported {} blocks not allocated",
        after_move.moved_blocks - before_move.moved_blocks,
    );
}

/// One rebuild on the poller's own terms, and the handle dropped immediately:
/// a `Scan` kept alive here would be the second owner the next rebuild sees.
/// **A rebuilt volume's gates are the SAME allocation as the volume it was
/// cloned from, and every gate reads back bit-identical.**
///
/// **The counter test above cannot do this job, and that is why this exists.**
/// `shared_bytes` is computed by WALKING the volume being cloned, not by
/// observing the clone, so it reports the same figure on a tree whose
/// `GateBuffer::clone` deep-copies every byte. Only pointer identity can tell
/// a shared clone from a copied one — and only a value comparison can tell a
/// shared clone from a broken one, so both halves are asserted here.
///
/// TAMPER, run 2026-09-09: give `GateBuffer` a hand-written `Clone` that does
/// `Arc::new(self.0.as_ref().clone())`. This test fails on the pointer
/// assertion (`radial 0's gate buffer was reallocated by the rebuild`) while
/// its value assertion and the whole of the counter test above still pass —
/// exactly the split the two halves exist to make visible. A tamper on
/// `GateBuffer::from` instead does NOT fail anything, because the sharing this
/// is about happens at the `Sweep` clone and not at construction.
#[test]
fn a_rebuilt_volume_shares_the_gates_of_the_one_it_was_cloned_from() {
    use nexrad_model::data::DataMoment;

    const SITE: &str = "KTLX";
    let _exclusive = crate::chunks::feed_level_serial::exclusive();
    let mut mgr = mgr_assembling(SITE);
    ingest(&mut mgr, SITE, 1, ChunkKind::Start, start_chunk());
    ingest(&mut mgr, SITE, 2, ChunkKind::Intermediate, cut(1, 0.5));

    // HELD across the rebuild, which is what makes the rebuild copy: this is
    // the bridge's role played by hand, and the reference the comparison
    // needs anyway.
    let first = mgr.snapshot(SITE).expect("a sealed cut is a volume");
    let before = crate::chunks::rebuild_totals();
    ingest(&mut mgr, SITE, 3, ChunkKind::Intermediate, cut(2, 0.9));
    let second = mgr.snapshot(SITE).expect("a rebuilt volume");
    let after = crate::chunks::rebuild_totals();
    assert_eq!(
        after.copies - before.copies,
        1,
        "the fixture did not produce the copying rebuild this test is about",
    );

    // The reflectivity moments of the cut both generations carry.
    let gates = |scan: &nexrad_model::data::Scan| -> Vec<(usize, Vec<u8>)> {
        scan.sweeps()
            .iter()
            .filter(|s| s.elevation_number() == 1)
            .flat_map(nexrad_model::data::Sweep::radials)
            .filter_map(nexrad_model::data::Radial::reflectivity)
            .map(|m| (m.gate_buffer().id(), m.raw_values().to_vec()))
            .collect()
    };
    let old = gates(&first.scan);
    let new = gates(&second.scan);
    assert!(
        !old.is_empty(),
        "fixture: the first cut carries no reflectivity to compare",
    );
    assert_eq!(
        old.len(),
        new.len(),
        "the rebuild did not carry the first cut's radials through",
    );
    for (i, ((old_id, old_bytes), (new_id, new_bytes))) in old.iter().zip(&new).enumerate() {
        assert_eq!(
            old_id, new_id,
            "radial {i}'s gate buffer was reallocated by the rebuild; the \
             clone is still deep-copying gates",
        );
        assert_eq!(
            old_bytes, new_bytes,
            "radial {i}'s gate values moved across a rebuild",
        );
    }
    assert!(
        old.iter()
            .zip(&new)
            .all(|((_, a), (_, b))| a == b && !a.is_empty()),
        "the comparison passed over empty buffers only",
    );
}

fn rebuild(mgr: &mut ChunkFeedManager, site: &str) {
    let built = mgr
        .feeds
        .get_mut(site)
        .expect("ensured")
        .poller
        .as_mut()
        .expect("the poller is home")
        .snapshot()
        .expect("a sealed cut");
    drop(built);
}
