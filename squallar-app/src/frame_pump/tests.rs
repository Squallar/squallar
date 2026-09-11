//! The pump table's contract with the `ChannelHub`.

use super::{FRAME_PUMP, PumpPhase};
use std::collections::BTreeMap;

/// channels.rs, read at compile time — the same handle the other source
/// probes in this crate use.
const CHANNELS: &str = include_str!("../channels.rs");

/// The receiver field a line of channels.rs declares, if it declares one.
fn declared_receiver_field(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix("pub ")?;
    let (name, ty) = rest.split_once(':')?;
    let name = name.trim();
    if !name.ends_with("_receiver") {
        return None;
    }
    if !ty.trim_start().starts_with("Receiver<") {
        return None;
    }
    Some(name)
}

/// The channel-pair field a line declares — `(base name, is_receiver)` — at
/// **any** visibility, so a pair cannot slip the shrink pin by narrowing.
fn declared_pair_field(line: &str) -> Option<(&str, bool)> {
    let mut rest = line.trim_start();
    if rest.starts_with("//") {
        return None;
    }
    if let Some(after_pub) = rest.strip_prefix("pub") {
        rest = match after_pub.strip_prefix('(') {
            Some(restriction) => restriction.split_once(')')?.1,
            None => after_pub,
        }
        .trim_start();
    }
    let (name, ty) = rest.split_once(':')?;
    let name = name.trim();
    let ty = ty.trim_start();
    if let Some(base) = name.strip_suffix("_sender") {
        return ty.starts_with("Sender<").then_some((base, false));
    }
    if let Some(base) = name.strip_suffix("_receiver") {
        return ty.starts_with("Receiver<").then_some((base, true));
    }
    None
}

/// Every receiver the hub declares is owned by exactly one pump row.
#[test]
fn every_hub_receiver_is_drained_by_exactly_one_row() {
    for (line, expected) in [
        (
            "    pub scan_receiver: Receiver<ScanResponse>,",
            Some("scan_receiver"),
        ),
        (
            "pub melting_layer_receiver: Receiver<MeltingLayerResponse>,",
            Some("melting_layer_receiver"),
        ),
        (
            "    pub scan_receiver : Receiver<ScanResponse>,",
            Some("scan_receiver"),
        ),
        ("    pub scan_sender: Sender<ScanResponse>,", None),
        ("    /// pub scan_receiver: Receiver<ScanResponse>,", None),
        ("    // pub scan_receiver: Receiver<ScanResponse>,", None),
        ("    pub scan_receiver: Sender<ScanResponse>,", None),
        ("    pub scan_sender: Receiver<ScanResponse>,", None),
        (
            "    pub(crate) scan_receiver: Receiver<ScanResponse>,",
            None,
        ),
        ("            scan_receiver: rx,", None),
    ] {
        assert_eq!(
            declared_receiver_field(line),
            expected,
            "the receiver scanner misreads `{line}`",
        );
    }

    let declared: Vec<&str> = CHANNELS
        .lines()
        .filter_map(declared_receiver_field)
        .collect();
    assert!(
        !declared.is_empty(),
        "presence control: channels.rs declares no receiver fields the \
         scanner can see — the hub moved or changed shape, so this guard is \
         reading nothing; re-anchor it in the land that moved the hub",
    );

    let mut drained: BTreeMap<&str, usize> = BTreeMap::new();
    for row in FRAME_PUMP {
        for field in row.drains {
            *drained.entry(field).or_default() += 1;
        }
    }

    for field in &declared {
        match drained.get(field).copied().unwrap_or(0) {
            1 => {}
            0 => panic!(
                "ChannelHub declares `{field}` but no FRAME_PUMP row drains \
                 it — its arrivals sit in the channel until the sender's \
                 next result happens to share a row, which is a leak with a \
                 delay, not a crash. Give it a row (or delete the channel \
                 and shrink the pin).",
            ),
            n => panic!(
                "`{field}` is drained by {n} FRAME_PUMP rows — which row \
                 sees an arrival is now a race between them. A row can own \
                 several receivers (see poll_level3_results); a receiver \
                 can never be owned by several rows.",
            ),
        }
    }
    for field in drained.keys() {
        assert!(
            declared.iter().any(|d| d == field),
            "FRAME_PUMP claims to drain `{field}`, which ChannelHub does \
             not declare — the row outlived its channel (or the field was \
             renamed); fix the row's `drains` in the same land",
        );
    }
}

/// The 17 channel pairs of record, in field order. Removal is legal;
/// addition never is.
///
/// **18 at WO-E3, 17 since WO-M12b**, and never anything else: the
/// `loop_scan_list` pair went when the archive listing moved behind the frame
/// contract and a radar frame listing began arriving on the one source path.
/// WO-M12b lowered `HUB_RECEIVER_MAX` in the land that earned it but left the
/// retired name sitting here, where it went on excusing the very channel that
/// land deleted; WO-M13b verified the pin and removed it. Shrinking this list
/// is the only edit it takes.
const HUB_BASE_NAMES: &[&str] = &[
    "scan",
    "render",
    "section",
    "voxel",
    "level3",
    "overlay_fetch",
    "overlay_render",
    "loop_scan_download",
    "loop_l3_list",
    "loop_l3_fetch",
    "loop_render",
    "loop_section",
    "chunk",
    "sounding",
    "melting_layer",
    "storm_motion",
    "site_catalogue",
];

/// The hub only ever shrinks.
#[test]
fn the_channel_hub_only_ever_shrinks() {
    for (line, expected) in [
        (
            "    pub scan_sender: Sender<ScanResponse>,",
            Some(("scan", false)),
        ),
        (
            "    pub scan_receiver: Receiver<ScanResponse>,",
            Some(("scan", true)),
        ),
        (
            "    pub(crate) loop_l3_fetch_receiver: Receiver<LoopL3FetchResponse>,",
            Some(("loop_l3_fetch", true)),
        ),
        (
            "    storm_motion_sender: Sender<StormMotionResponse>,",
            Some(("storm_motion", false)),
        ),
        ("    /// pub scan_sender: Sender<ScanResponse>,", None),
        (
            "    // a comment about the scan_sender: Sender<...> field",
            None,
        ),
        ("    pub scan_sender: Receiver<ScanResponse>,", None),
        ("            scan_receiver: rx,", None),
        ("    pub catalogue: Option<SiteCatalogue>,", None),
    ] {
        assert_eq!(
            declared_pair_field(line),
            expected,
            "the pair scanner misreads `{line}`",
        );
    }

    let scraped: Vec<(&str, bool)> = CHANNELS.lines().filter_map(declared_pair_field).collect();
    assert!(
        !scraped.is_empty(),
        "presence control: channels.rs declares no channel-pair fields the \
         scanner can see — the hub moved or changed shape; re-anchor this \
         pin in the land that moved it",
    );

    let mut pairs: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for (base, is_receiver) in scraped {
        assert!(
            HUB_BASE_NAMES.contains(&base),
            "channels.rs declares a `{base}` channel that is not in the \
             pinned list: new sources never add a ChannelHub channel — \
             arrivals ride the unified overlay channel (SourceEvent at \
             M13); if deleting radar plumbing, shrink this list.",
        );
        let entry = pairs.entry(base).or_default();
        if is_receiver {
            entry.1 += 1;
        } else {
            entry.0 += 1;
        }
    }
    for (base, (senders, receivers)) in &pairs {
        assert_eq!(
            (*senders, *receivers),
            (1, 1),
            "the `{base}` channel is not exactly one sender + one receiver \
             ({senders} sender(s), {receivers} receiver(s)) — a half-deleted \
             pair strands whichever half remains",
        );
    }
}

/// The table's order of record, `(name, phase)` per row, as a literal.
const EXPECTED_ROWS: &[(&str, PumpPhase)] = &[
    ("poll_scan_results", PumpPhase::Ingest),
    ("poll_chunk_results", PumpPhase::Ingest),
    ("drive_chunk_feeds", PumpPhase::Ingest),
    ("poll_voxel_results", PumpPhase::Ingest),
    ("publish_base_volumes", PumpPhase::Ingest),
    ("poll_overlay_fetch_results", PumpPhase::Ingest),
    ("poll_render_results", PumpPhase::Apply),
    ("poll_section_results", PumpPhase::Apply),
    ("poll_level3_results", PumpPhase::Apply),
    ("poll_site_catalogue", PumpPhase::Apply),
    ("poll_overlay_render_results", PumpPhase::Apply),
    ("accept_loop_scan_listings", PumpPhase::Apply),
    ("poll_loop_scan_download_results", PumpPhase::Apply),
    ("poll_loop_l3_list_results", PumpPhase::Apply),
    ("poll_loop_l3_fetch_results", PumpPhase::Apply),
    ("poll_loop_render_results", PumpPhase::Apply),
    ("poll_loop_section_results", PumpPhase::Apply),
    ("poll_extract_results", PumpPhase::Apply),
    ("advance_loop_playback", PumpPhase::Advance),
    ("refill_unserved_loop_windows", PumpPhase::Dispatch),
    ("dispatch_pane_renders", PumpPhase::Dispatch),
    ("dispatch_section_renders", PumpPhase::Dispatch),
    ("dispatch_loop_renders", PumpPhase::Dispatch),
    ("dispatch_overlay_loop_renders", PumpPhase::Dispatch),
];

#[test]
fn the_pump_rows_are_in_the_pinned_order() {
    let actual: Vec<(&str, PumpPhase)> =
        FRAME_PUMP.iter().map(|row| (row.name, row.phase)).collect();
    assert_eq!(
        actual.as_slice(),
        EXPECTED_ROWS,
        "the FRAME_PUMP order moved. Three edges are load-bearing: \
         results-apply before advance — a frame's last result is IN the \
         frame that advances onto it; advance before dispatch — the \
         dispatchers measure a budget that is not being spent on stale \
         panes; Ingest at handle_redraw's early position — a new volume \
         becomes the drawn one before `evict_unshown_scans` runs. If a row \
         genuinely moves, move its argument comment with it and re-pin \
         here.",
    );
}

/// The body of `name` in `source`, braces balanced.
fn fn_body<'a>(source: &'a str, name: &str) -> &'a str {
    let start = source
        .find(name)
        .unwrap_or_else(|| panic!("{name} is gone"));
    let rest = &source[start..];
    let open = rest.find('{').expect("a body");
    let mut depth = 0usize;
    for (i, c) in rest[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &rest[open..open + i];
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced braces in {name}");
}

/// **Every unbounded arrival drain of `Ingest` consults the frame's budget.**
///
/// The three `while let Ok(..) = try_recv_arrival()` loops apply whatever a
/// burst delivered on whichever frame follows it. Measured on the Mac against
/// the `frame worst:` latch, the worst of a 300-frame latched sample spent
/// 36,017 µs of 39,567 in `pre_ingest`, on the frame after fourteen overlay
/// payloads and four Level III products landed together.
///
/// A source probe and not a behavioural one for the reason the chunk drain's
/// own run-moment pin is: an `App` cannot be stood up in a unit test, so what
/// is checkable here is that the deadline is set around the phase, cleared
/// after it, and read by each drain. It gates the wiring, not the duration —
/// said plainly so the next reader does not take it for a timing test.
#[test]
fn every_ingest_arrival_drain_consults_the_frame_budget() {
    let app = include_str!("../app.rs");
    let chunks = include_str!("../app_chunks.rs");

    let poll = fn_body(app, "fn poll_data_channels(");
    assert!(
        poll.contains("ingest_deadline = Some(") && poll.contains("INGEST_BUDGET_PER_FRAME"),
        "poll_data_channels no longer opens the frame's arrival budget, so \
         every drain below reads `None` and is unbounded again"
    );
    assert!(
        poll.contains("ingest_deadline = None"),
        "the deadline outlives the Ingest phase, so a drain called from \
         anywhere else would silently inherit this frame's budget"
    );

    for (source, name, what) in [
        (app, "fn poll_scan_results(", "scan"),
        (chunks, "fn poll_chunk_results(", "chunk"),
        (app, "fn poll_overlay_fetch_results(", "overlay fetch"),
    ] {
        let body = fn_body(source, name);
        assert!(
            body.contains("ingest_budget_spent()") || body.contains("ingest_deadline"),
            "the {what} drain no longer consults the frame's arrival budget, \
             so one burst is applied whole on one frame"
        );
        assert!(
            body.contains("drained_one"),
            "the {what} drain lost its always-one-arrival guarantee, so a \
             drain ahead of it in the phase can starve it for the length of \
             a burst"
        );
    }
}

/// **The budget is read BEFORE the take, in every one of those three drains.**
///
/// `try_recv_arrival` is destructive. A drain spelled `while let Ok(msg) =
/// try_recv_arrival() { if spent { break; } .. }` has already taken the
/// message it then abandons, so the arrival that crosses the budget boundary
/// is destroyed rather than left queued — the opposite of what
/// `INGEST_BUDGET_PER_FRAME` documents ("what is left stays queued and the
/// window is asked for another frame") and of what the action queue beside it
/// does, which defers the action it cannot afford.
///
/// It cost one dropped archive volume to find: a full-workspace run caught a
/// pane holding the moment it was parked at, and 33 later runs did not, because
/// the window is one arrival wide — 66 µs at p50 against a 1 ms budget.
/// `an_arrival_the_budget_cannot_afford_is_deferred_rather_than_dropped` pins
/// the scan drain behaviourally; this is what carries the other two.
#[test]
fn every_ingest_arrival_drain_reads_its_budget_before_taking_the_message() {
    let app = include_str!("../app.rs");
    let chunks = include_str!("../app_chunks.rs");

    for (source, name, budget, what) in [
        (
            app,
            "fn poll_scan_results(",
            "ingest_budget_spent()",
            "scan",
        ),
        (
            chunks,
            "fn poll_chunk_results(",
            "ingest_budget_spent()",
            "chunk",
        ),
        (
            app,
            "fn poll_overlay_fetch_results(",
            "deadline.is_some_and(",
            "overlay fetch",
        ),
    ] {
        let body = fn_body(source, name);
        let take = body.find("try_recv_arrival()").unwrap_or_else(|| {
            panic!(
                "the {what} drain no longer takes its messages through \
                 the counted spelling, so what this pin reads is gone"
            )
        });
        let read = body
            .find(budget)
            .unwrap_or_else(|| panic!("the {what} drain no longer reads the frame's budget"));
        assert!(
            read < take,
            "the {what} drain reads its budget {} bytes AFTER the \
             `try_recv_arrival()` that takes the message, so the arrival that \
             crosses the boundary is taken and then dropped at the `break` \
             instead of being left on the channel for the next frame",
            read - take,
        );
    }
}
