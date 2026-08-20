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

/// The 18 channel pairs of record, in field order. Removal is legal;
/// addition never is.
const HUB_BASE_NAMES: &[&str] = &[
    "scan",
    "render",
    "section",
    "voxel",
    "level3",
    "overlay_fetch",
    "overlay_render",
    "loop_scan_list",
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
    ("dispatch_pane_renders", PumpPhase::Dispatch),
    ("dispatch_section_renders", PumpPhase::Dispatch),
    ("dispatch_loop_renders", PumpPhase::Dispatch),
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
