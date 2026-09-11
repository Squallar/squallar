//! **The decode treadmill, driven deterministically.**
use super::*;

pub(super) const SITE: &str = "KTLX";
pub(super) const FRAMES: usize = 24;
const SLOTS: usize = 4;

pub(super) fn distance(idx: usize, playhead: usize) -> u64 {
    ((idx + FRAMES - playhead) % FRAMES) as u64
}

pub(super) struct Rig {
    pub(super) mgr: LoopDownloadManager,
    pub(super) laps: u64,
    pub(super) ceiling: usize,
    pub(super) playhead: usize,
    /// Frames that have been rendered at least once, so `frame.image` is
    /// `Some` and the residency policy stops wanting their moments.
    pub(super) textured: std::collections::HashSet<usize>,
    /// `LOOP_DECODED_LOOKAHEAD_FRAMES`: how far ahead of the playhead a
    /// textured frame still keeps its moments. Desktop is `Some(1)`.
    pub(super) lookahead: Option<u64>,
    /// Whether the residency sweep publishes what it wants decoded. `false`
    /// is this tree before the repair.
    pub(super) publish: bool,
    /// Volumes filed per round by something the ceiling does not gate — the
    /// pane fetch, the auto-poll, the chunk feed. `cache_scan` admits all of
    /// them, which is what puts the cache over the ceiling in the first place.
    pub(super) ungated: usize,
}

impl Rig {
    pub(super) fn new(frames_held: usize, playhead: usize) -> Self {
        let mut mgr = LoopDownloadManager::new();
        for idx in 0..FRAMES {
            mgr.cache_archive(SITE, ts(idx as u32), Arc::new(vec![0u8; 4096]));
        }
        mgr.set_plan(
            0,
            FramePlan::new(
                SITE.to_string(),
                (0..FRAMES).map(|i| ts(i as u32)).collect(),
            ),
        );
        let one = crate::scan_size::scan_bytes(&priced_volume().0);
        Self {
            mgr,
            laps: 0,
            ceiling: one * frames_held,
            playhead,
            textured: std::collections::HashSet::new(),
            lookahead: Some(1),
            publish: false,
            ungated: 0,
        }
    }

    /// `App`'s `decoded_keep`, restated: untextured, or inside the lookahead.
    fn wants_moments(&self, idx: usize) -> bool {
        !self.textured.contains(&idx)
            || self
                .lookahead
                .is_some_and(|ahead| distance(idx, self.playhead) <= ahead)
    }

    /// The sweep's publication, built from the same predicate that drives the
    /// residency pass below, so the pump and the eviction cannot disagree.
    fn publish_wants(&mut self) {
        if !self.publish {
            return;
        }
        let mut per_site: std::collections::HashMap<
            String,
            std::collections::HashMap<chrono::NaiveDateTime, u64>,
        > = std::collections::HashMap::new();
        let frames = per_site.entry(SITE.to_string()).or_default();
        for idx in 0..FRAMES {
            if self.wants_moments(idx) {
                frames.insert(ts(idx as u32), distance(idx, self.playhead));
            }
        }
        self.mgr.set_decode_wants(per_site);
    }

    pub(super) fn round(&mut self) {
        self.publish_wants();
        // An ungated arrival: a volume the pane is looking at, filed with no
        // admission check at all, exactly as `cache_scan`'s callers do.
        for n in 0..self.ungated {
            let at = ts((FRAMES + n) as u32);
            self.mgr.cache_scan(SITE, at, priced_volume());
        }
        let mut decodes = 0usize;
        for (site, at) in self.mgr.frames_needing_decode(0) {
            if decodes >= SLOTS || !self.mgr.decoded_room_for(&site, self.ceiling) {
                break;
            }
            if self.mgr.archive_for(&site, &at).is_none() {
                continue;
            }
            self.mgr.mark_decode_in_flight(&site, at);
            self.mgr.complete_download(&site, &at);
            self.mgr.cache_scan(&site, at, priced_volume());
            // The frame renders off the volume that just landed.
            if let Some(idx) = (0..FRAMES).find(|idx| ts(*idx as u32) == at) {
                self.textured.insert(idx);
            }
            self.laps += 1;
            decodes += 1;
        }
        // **The residency pass, which runs every tick and is gated by
        // nothing** — `App::evict_unneeded_loop_scans`.
        let keep: std::collections::HashSet<chrono::NaiveDateTime> = (0..FRAMES)
            .filter(|idx| self.wants_moments(*idx))
            .map(|idx| ts(idx as u32))
            .collect();
        let traded = self.mgr.evict_decoded_except(|_, at, _| keep.contains(at));
        drop(traded);
        let playhead = self.playhead;
        let removed = self.mgr.evict_decoded_to_ceiling(
            self.ceiling,
            |_: &str, at: &chrono::NaiveDateTime| {
                (0..FRAMES)
                    .find(|idx| ts(*idx as u32) == *at)
                    .map_or(u64::MAX, |idx| distance(idx, playhead))
            },
            |_, _, _| false,
        );
        drop(removed);
    }

    pub(super) fn rounds(&mut self, n: usize) {
        for _ in 0..n {
            self.round();
        }
    }

    /// Decoded plan frames, counted independently of the manager's own
    /// figure so the row's level can be checked against a walk.
    pub(super) fn resident(&self) -> usize {
        (0..FRAMES)
            .filter(|idx| self.mgr.is_cached(SITE, &ts(*idx as u32)))
            .count()
    }
}

/// **The pump and the residency sweep no longer run against each other.**
///
/// # The defect
///
/// `App::evict_unneeded_loop_scans` evicts a decoded volume the moment
/// nothing will read its moments — on desktop that is every frame that
/// carries a texture and is not the playhead or one ahead of it — and it runs
/// every tick, gated by nothing. `frames_needing_decode` offered every
/// archived moment whose volume was missing, which is by construction exactly
/// what that pass had just thrown away. So the pump decoded it, the sweep
/// evicted it, and the pump decoded it again, for ever. **Neither ceiling is
/// involved**: this rig's `loop ceiling:` counters read `over 0, evicted 0`
/// throughout, and the lap count is flat across every ceiling from 4 frames
/// to 30.
///
/// Both arms in one test because the contrast is the claim, and the control
/// arm asserts only that the rig can still REACH the defect — never its
/// magnitude, so a repair made somewhere else does not have to keep this
/// treadmill alive to stay green.
///
/// TAMPER: delete the `None => { suppressed += 1; None }` arm from
/// `frames_needing_decode`'s `filter_map` (offer the frame instead) and the
/// repaired arm's lap assertion fails.
#[test]
fn the_pump_does_not_re_decode_what_the_residency_sweep_just_evicted() {
    // --- the control: the sweep publishes nothing, which is this tree before
    // the change. It exists to show the rig reaches the defect at all.
    let mut blind = Rig::new(8, 12);
    blind.rounds(40);
    let (_, suppressed, blind_laps, _, _) = blind.mgr.decode_churn();
    assert_eq!(
        suppressed, 0,
        "the control arm suppressed an offer, so it is not the control",
    );
    assert!(
        blind_laps > 0,
        "the rig no longer reaches the decode treadmill, so the repaired arm \
         below proves nothing: it decoded {blind_laps} volumes twice",
    );

    // --- the repair: the sweep publishes what it wants decoded ---
    let mut rig = Rig::new(8, 12);
    rig.publish = true;
    rig.rounds(40);
    let (_, suppressed, laps, resident, saturated) = rig.mgr.decode_churn();
    assert!(
        !saturated,
        "the seen-set saturated, so laps is a lower bound"
    );
    assert_eq!(
        laps, 0,
        "the pump re-decoded a moment the sweep had already evicted {laps} \
         times in 40 rounds",
    );
    assert_eq!(
        rig.laps, FRAMES as u64,
        "every frame should be decoded exactly once over the run",
    );
    assert!(
        suppressed > 0,
        "nothing was suppressed, so the publication never reached the pump \
         and this arm is green for the wrong reason",
    );
    assert_eq!(
        resident,
        rig.lookahead.map_or(0, |ahead| ahead as usize + 1),
        "the repaired pump should hold the playhead and its lookahead",
    );
    // The row reports `resident` as a LEVEL beside three running totals, and a
    // level read off the wrong map would make the whole ratio unreadable.
    assert_eq!(
        resident,
        rig.resident(),
        "`decode_churn` reports a different resident count than a walk of the \
         plan's own frames finds",
    );
}

/// **The pump serves the playhead first, and a playhead away from frame zero
/// is not starved.**
///
/// The walk took the plan oldest-first and `break`s on the first frame that
/// will not fit, so with the playhead mid-loop every slot went to the frames
/// FURTHEST from the glass: measured on this rig at playhead 12, the frame
/// the user is looking at was never decoded in 40 rounds.
///
/// TAMPER: delete the `wanted.sort_by_key(|(rank, _)| *rank)` line and the
/// playhead assertion fails.
#[test]
fn the_playhead_is_decoded_before_the_frames_furthest_from_it() {
    let mut rig = Rig::new(8, 12);
    rig.publish = true;
    rig.publish_wants();
    let offers = rig.mgr.frames_needing_decode(0);
    assert_eq!(
        offers.first().map(|(_, at)| *at),
        Some(ts(12)),
        "the playhead's own frame was not the pump's first errand: {offers:?}",
    );

    // And over a run it is actually decoded, which is the product property.
    rig.rounds(40);
    assert!(
        rig.mgr.is_cached(SITE, &ts(12)),
        "the frame on the glass was never decoded",
    );
}

/// **An unpublished site keeps exactly the order and the offers it had.** The
/// filter is sound in one direction only: it may never suppress a frame
/// something will read, so a caller that has published nothing — every other
/// test in this file, and every non-loop path — must be unaffected.
///
/// TAMPER: make the `None` arm of the `match wants` return `None` and this
/// fails with an empty offer list.
#[test]
fn a_site_the_sweep_never_published_for_is_offered_its_plan_in_plan_order() {
    let rig = Rig::new(8, 0);
    let offers: Vec<chrono::NaiveDateTime> = rig
        .mgr
        .frames_needing_decode(0)
        .into_iter()
        .map(|(_, at)| at)
        .collect();
    assert_eq!(
        offers,
        (0..FRAMES).map(|i| ts(i as u32)).collect::<Vec<_>>(),
        "an unpublished site's offers changed shape",
    );
    let (offered, suppressed, ..) = rig.mgr.decode_churn();
    assert_eq!(
        (offered, suppressed),
        (FRAMES as u64, 0),
        "an unpublished site had an offer suppressed",
    );
}

/// **The lap counter counts a re-decode and not an arrival.** Its own gate,
/// because a counter that only ever rises with ordinary work would make the
/// row above unreadable.
///
/// TAMPER: drop the `self.decoded_ever.contains(&key)` guard in `cache_scan`
/// and the first assertion fails.
#[test]
fn a_lap_is_a_second_decode_of_the_same_moment_and_a_first_one_is_not() {
    let mut mgr = LoopDownloadManager::new();
    for idx in 0..3u32 {
        mgr.cache_scan(SITE, ts(idx), priced_volume());
    }
    assert_eq!(
        mgr.decode_churn().2,
        0,
        "three first decodes of three moments counted as laps",
    );
    mgr.cache_scan(SITE, ts(0), priced_volume());
    assert_eq!(
        mgr.decode_churn().2,
        1,
        "a second decode of the same moment did not count as a lap",
    );
}
