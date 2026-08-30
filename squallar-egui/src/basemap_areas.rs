//! What the manage screen needs that a frame must not compute itself.
//!
//! Three things live here, none of them drawing: the detail vocabulary an
//! area's stored zoom is read back through, the generation fact an area from
//! an older archive states about itself, and the worker that answers "is this
//! area still all there" — a store listing, which is filesystem IO and so
//! never on the frame thread.
//!
//! # Completeness is asked, never remembered
//!
//! [`AreaMaintenance`] holds the answers [`DownloadedArea::reconcile`] gave,
//! keyed by area id, and nothing else. It is not persisted and does not
//! survive a launch: every session asks the store again, which is what keeps
//! the named silent-partial-success defect structurally impossible rather
//! than guarded against. An id it has no answer for reads as *unknown*, and
//! the screen draws that as unknown rather than as either outcome.
//!
//! # An older generation expires nothing
//!
//! A downloaded segment is a standalone archive carrying its own header and
//! directories, so a new archive generation cannot invalidate a byte of it.
//! [`generation_note`] therefore states a **fact** — which month's map data
//! the area holds, and whether a newer cut exists — and never a warning; the
//! re-download is a button the user may press, never an action anything here
//! takes.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use egui::Context;
// The portable channel, as `tile_source` uses for the same reason: tokio's
// `sync` is a native-only dependency of this crate, and the worker runs on
// both targets.
use futures::StreamExt as _;
use futures::channel::mpsc::{UnboundedSender, unbounded};

use crate::basemap_download::{
    AreaSpec, AreaStatus, BasemapDownload, DownloadOutcome, DownloadProgress, DownloadedArea,
    SegmentStore,
};
use crate::tile_source::runtime;

// ---------------------------------------------------------------------------
// The detail vocabulary
// ---------------------------------------------------------------------------

/// What each stored depth lets you make out, deepest zoom first matched.
///
/// **Framed by what is legible, never as a zoom number** — the decision taken
/// with the user. The deepest level is `z14` because the published archives
/// stop there: street-and-building detail on the glass comes from over-zoom
/// rendering of `z14` tiles, so a level below this one would be copy
/// promising something no download could deliver.
pub(crate) const DETAIL_LEVELS: &[(u8, &str)] = &[
    (10, "Cities and highways"),
    (12, "Towns and main roads"),
    (14, "Every street"),
];

/// What an area cut to `max_zoom` lets you make out.
///
/// A record carries the depth its download asked for, which need not be one of
/// [`DETAIL_LEVELS`]' exact zooms — a hand-placed area, or a level table that
/// moved after the download. The shallowest level that covers the depth is the
/// honest label; anything past the deepest gets the deepest, because there is
/// no data past it to describe.
pub(crate) fn detail_label(max_zoom: u8) -> &'static str {
    let deepest = DETAIL_LEVELS[DETAIL_LEVELS.len() - 1].1;
    DETAIL_LEVELS
        .iter()
        .find(|(zoom, _)| max_zoom <= *zoom)
        .map_or(deepest, |(_, label)| label)
}

// ---------------------------------------------------------------------------
// The generation fact
// ---------------------------------------------------------------------------

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// Which month's map data a generation was cut from, as `(year, month)`.
///
/// The generation is
/// [`generation_for_url`](crate::basemap_archive::block_cache::generation_for_url)'s
/// encoding of the archive's path, and the published archives carry the cut
/// date in that path (`basemap/omt-20260828.pmtiles`). Read as the last run of
/// exactly eight digits that is a plausible date, so a path that also carries
/// a build hash is not mistaken for one.
///
/// `None` for a generation this cannot date — including the empty string a
/// record written before generations were stored carries. Nothing is claimed
/// about such an area, which is the point: an unsourced vintage is not a fact.
fn generation_month(generation: &str) -> Option<(i32, u32)> {
    let bytes = generation.as_bytes();
    let mut dated = None;
    let mut at = 0;
    while at < bytes.len() {
        if !bytes[at].is_ascii_digit() {
            at += 1;
            continue;
        }
        let start = at;
        while at < bytes.len() && bytes[at].is_ascii_digit() {
            at += 1;
        }
        let run = &generation[start..at];
        if run.len() != 8 {
            continue;
        }
        // Eight ASCII digits: every one of these parses, so a failure here is
        // impossible rather than handled.
        let (Ok(year), Ok(month), Ok(day)) = (
            run[0..4].parse::<i32>(),
            run[4..6].parse::<u32>(),
            run[6..8].parse::<u32>(),
        ) else {
            continue;
        };
        if (2000..=2999).contains(&year) && (1..=12).contains(&month) && (1..=31).contains(&day) {
            dated = Some((year, month));
        }
    }
    dated
}

/// What an area says about the map data it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GenerationNote {
    /// The month the stored map data was cut, e.g. `"August 2026"`.
    pub(crate) vintage: String,
    /// Whether the archive this build reads is a **strictly newer** cut.
    ///
    /// Strictly: two generations that merely differ say nothing about which
    /// came first, and "update available" beside a *newer* stored area would
    /// be false. A difference this cannot order states the vintage alone.
    pub(crate) update_available: bool,
}

impl GenerationNote {
    /// The one line the screen draws — a fact and, when there is one, the
    /// thing the user may choose to do about it.
    pub(crate) fn line(&self) -> String {
        if self.update_available {
            format!("Map data {} \u{b7} update available", self.vintage)
        } else {
            format!("Map data {}", self.vintage)
        }
    }
}

/// What `stored` holds and whether `live` is newer, or `None` when `stored`
/// carries no datable generation and there is therefore nothing to state.
pub(crate) fn generation_note(stored: &str, live: &str) -> Option<GenerationNote> {
    let (year, month) = generation_month(stored)?;
    let vintage = format!("{} {year}", MONTHS[(month - 1) as usize]);
    let update_available = generation_month(live).is_some_and(|newer| newer > (year, month));
    Some(GenerationNote {
        vintage,
        update_available,
    })
}

// ---------------------------------------------------------------------------
// The in-flight download
// ---------------------------------------------------------------------------

/// The one offline-area download this device is running.
///
/// One at a time by construction: a second [`Gui`](crate::Gui) field would be
/// a second bulk pull competing for the same connection this feature exists to
/// spend carefully.
pub(crate) struct ActiveDownload {
    /// What is being downloaded. **No record exists for it yet** — only a
    /// `Complete` outcome makes one ([`DownloadedArea::from_outcome`]), which
    /// is why an in-flight area draws as progress rather than as an entry.
    pub(crate) spec: AreaSpec,
    /// The archive generation this run is cutting from, so the record written
    /// when it completes dates the bytes it actually holds.
    pub(crate) generation: String,
    engine: BasemapDownload,
}

impl ActiveDownload {
    /// Start `spec` against `source`, publishing into `store`.
    pub(crate) fn start<S, St>(
        source: S,
        store: St,
        spec: AreaSpec,
        generation: String,
        ctx: Context,
    ) -> Self
    where
        S: crate::basemap_archive::ArchiveRangeSource,
        St: SegmentStore,
    {
        let engine = BasemapDownload::start(source, store, spec.clone(), ctx);
        Self {
            spec,
            generation,
            engine,
        }
    }

    /// [`Self::start`] with the segment cap handed in — the engine's own
    /// test-only door, for the same reason: the committed fixture is 419 KB
    /// and a multi-segment plan cannot be cut out of it at the production cap.
    #[cfg(test)]
    pub(crate) fn start_with_segment_bytes<S, St>(
        source: S,
        store: St,
        spec: AreaSpec,
        generation: String,
        ctx: Context,
        segment_bytes: u64,
    ) -> Self
    where
        S: crate::basemap_archive::ArchiveRangeSource,
        St: SegmentStore,
    {
        let engine =
            BasemapDownload::with_segment_bytes(source, store, spec.clone(), ctx, segment_bytes);
        Self {
            spec,
            generation,
            engine,
        }
    }

    /// Where the run stands, off the engine's own always-on counters. See
    /// [`DownloadProgress`] for which figure carries which denominator.
    pub(crate) fn progress(&self) -> DownloadProgress {
        self.engine.progress()
    }

    /// How the run ended, once it has.
    pub(crate) fn outcome(&self) -> Option<DownloadOutcome> {
        self.engine.outcome()
    }
}

// ---------------------------------------------------------------------------
// The maintenance worker
// ---------------------------------------------------------------------------

/// What the worker is asked to do. Both jobs are store IO.
enum AreaJob {
    /// Reconcile these records against the store's own listing.
    Reconcile(Vec<DownloadedArea>),
    /// Remove every artifact of this area, finished or not.
    Delete(String),
}

/// The answers the worker has published so far. An id absent from this is one
/// the store has not been asked about yet, or one it would not answer for.
#[derive(Default)]
struct AreaFacts {
    statuses: HashMap<String, AreaStatus>,
}

/// The manage screen's off-frame-thread arm: it lists and deletes, and the
/// screen reads its latest answer.
///
/// One long-lived task on the tile fetch task's own runtime, fed by an
/// unbounded channel — the [`HttpsTiles`](crate::tile_source::HttpsTiles)
/// shape, and it drives the UI the same way: it holds an [`egui::Context`] and
/// repaints through it, so nothing here crosses the App seam.
pub(crate) struct AreaMaintenance {
    jobs: UnboundedSender<AreaJob>,
    facts: Arc<Mutex<AreaFacts>>,
    /// Ids already sent for reconciliation. Without it a frame that finds an
    /// unknown status would ask again on the next frame, and a store that
    /// cannot answer would be asked at frame rate forever.
    asked: HashSet<String>,
    /// Owns the task. Dropping it stops the worker.
    _runtime: runtime::Runtime,
}

impl AreaMaintenance {
    /// A worker over `store`, repainting `ctx` whenever an answer lands.
    pub(crate) fn start<St: SegmentStore>(store: St, ctx: Context) -> Self {
        let (jobs, mut inbox) = unbounded();
        let facts = Arc::new(Mutex::new(AreaFacts::default()));
        let task = {
            let facts = Arc::clone(&facts);
            async move {
                while let Some(job) = inbox.next().await {
                    match job {
                        AreaJob::Reconcile(areas) => {
                            for area in areas {
                                let id = &area.spec.area_id;
                                match store.existing_segments(id).await {
                                    Ok(present) => {
                                        let status = area.reconcile(&present);
                                        if let Ok(mut facts) = facts.lock() {
                                            facts.statuses.insert(id.clone(), status);
                                        }
                                    }
                                    // Unknown, and left unknown: a listing
                                    // that failed is not evidence the bytes
                                    // are gone, and writing a zero here would
                                    // draw a complete area as a lost one.
                                    Err(error) => {
                                        log::warn!(
                                            "{id}: the offline store would not list: {error}"
                                        );
                                    }
                                }
                            }
                        }
                        AreaJob::Delete(area_id) => {
                            if let Err(error) = store.remove_area(&area_id).await {
                                log::warn!(
                                    "{area_id}: the offline store would not delete: {error}"
                                );
                            }
                            if let Ok(mut facts) = facts.lock() {
                                facts.statuses.remove(&area_id);
                            }
                        }
                    }
                    ctx.request_repaint();
                }
            }
        };

        Self {
            jobs,
            facts,
            asked: HashSet::new(),
            _runtime: runtime::spawn(task),
        }
    }

    /// Ask the store about every area this worker has not been asked about.
    /// Idempotent per id, so a frame may call it unconditionally.
    pub(crate) fn reconcile_unknown(&mut self, areas: &[DownloadedArea]) {
        let wanted: Vec<DownloadedArea> = areas
            .iter()
            .filter(|area| !self.asked.contains(&area.spec.area_id))
            .cloned()
            .collect();
        if wanted.is_empty() {
            return;
        }
        for area in &wanted {
            self.asked.insert(area.spec.area_id.clone());
        }
        let _ = self.jobs.unbounded_send(AreaJob::Reconcile(wanted));
    }

    /// What the store last said about `area_id`, or `None` for an area it has
    /// not answered for.
    pub(crate) fn status(&self, area_id: &str) -> Option<AreaStatus> {
        self.facts.lock().ok()?.statuses.get(area_id).copied()
    }

    /// Remove every artifact of `area_id` from the store.
    pub(crate) fn delete(&mut self, area_id: &str) {
        self.asked.remove(area_id);
        let _ = self
            .jobs
            .unbounded_send(AreaJob::Delete(area_id.to_owned()));
    }

    /// Forget what was known about `area_id`, so the next frame asks again —
    /// what a finished download run leaves behind.
    pub(crate) fn recheck(&mut self, area_id: &str) {
        self.asked.remove(area_id);
        if let Ok(mut facts) = self.facts.lock() {
            facts.statuses.remove(area_id);
        }
    }
}

#[cfg(test)]
#[path = "basemap_areas/tests.rs"]
mod tests;
