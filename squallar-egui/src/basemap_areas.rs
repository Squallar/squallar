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

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use egui::Context;
use squallar_units::DataSize;
// The portable channel, as `tile_source` uses for the same reason: tokio's
// `sync` is a native-only dependency of this crate, and the worker runs on
// both targets.
use futures::StreamExt as _;
use futures::channel::mpsc::{UnboundedSender, unbounded};

use crate::basemap_download::{
    AreaArchive, AreaHoldings, AreaSpec, AreaStatus, BasemapDownload, DownloadOutcome,
    DownloadProgress, DownloadedArea, SegmentStore,
};
use crate::tile_source::runtime;

// ---------------------------------------------------------------------------
// The detail vocabulary
// ---------------------------------------------------------------------------

/// How deep an area is stored, named for what the user can make out at it.
///
/// **Framed by what is legible, never as a zoom number** — the decision taken
/// with the user. There are three and not four because the deepest is the
/// archive's own ceiling: a level below it would be copy promising data no
/// download can fetch, which is "never warn about the unfixable" in its
/// positive form. Street and building detail past the ceiling comes from
/// over-zoom rendering of the deepest stored tile, exactly as it does for the
/// live archive.
///
/// **The zooms are not in this type.** They are [`Self::zoom_in`]'s answer
/// against an archive's own `max_zoom`, so the offline ceiling and
/// `tile_source`'s render clamp come from one number rather than two that can
/// drift, and an archive that goes deeper makes every level deeper without an
/// edit here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DetailLevel {
    /// The shape of the place and its highways.
    CitiesAndHighways,
    /// The roads you would actually drive.
    TownsAndMainRoads,
    /// Everything the archive has.
    EveryStreet,
}

/// The three levels, shallowest first — the order the list draws in.
pub const DETAIL_LEVELS: [DetailLevel; 3] = [
    DetailLevel::CitiesAndHighways,
    DetailLevel::TownsAndMainRoads,
    DetailLevel::EveryStreet,
];

/// Zoom levels between one detail level and the next.
///
/// **Measured against where content enters a production OMT-schema style**,
/// not chosen for roundness: rivers at z9, major road labels at z11, highway
/// and major-road casings at z12, service roads and taxiways at z13, streams
/// at z14. Two levels apart is the smallest step across which the *legible*
/// content changes, which is what the three names describe. Against the
/// shipped ceiling of 14 this reproduces the z10 / z12 / z14 the design table
/// names.
pub const DETAIL_LEVEL_STEP: u8 = 2;

impl DetailLevel {
    /// The user-facing name. The only vocabulary this feature has for depth.
    pub fn label(self) -> &'static str {
        match self {
            Self::CitiesAndHighways => "Cities and highways",
            Self::TownsAndMainRoads => "Towns and main roads",
            Self::EveryStreet => "Every street",
        }
    }

    /// Zoom levels below the archive's ceiling this level sits at.
    fn steps_below_ceiling(self) -> u8 {
        match self {
            Self::CitiesAndHighways => 2 * DETAIL_LEVEL_STEP,
            Self::TownsAndMainRoads => DETAIL_LEVEL_STEP,
            Self::EveryStreet => 0,
        }
    }

    /// The deepest zoom this level stores in an archive whose ceiling is
    /// `archive_max_zoom` — **the header's own figure**, never a constant.
    ///
    /// Relative to the ceiling rather than pinned to absolute zooms, because
    /// the ceiling is the one thing an archive declares about its depth: an
    /// archive built deeper is deeper because it carries more at the bottom,
    /// and each name should keep meaning a fixed step of detail below
    /// everything-there-is rather than a schema figure the deeper build may
    /// not share.
    ///
    /// An archive shallower than `2 * DETAIL_LEVEL_STEP` collapses the top
    /// levels onto zoom 0 — three names over fewer than three depths. That is
    /// a property of such an archive rather than something to paper over, and
    /// no archive this app ships is one.
    pub fn zoom_in(self, archive_max_zoom: u8) -> u8 {
        archive_max_zoom.saturating_sub(self.steps_below_ceiling())
    }

    /// The persisted spelling. A token rather than a derived `Serialize`, so a
    /// config naming a level this build does not know costs the *choice* and
    /// not the whole file — `DownloadedAreaConfig::restore`'s discipline.
    pub(crate) fn token(self) -> &'static str {
        match self {
            Self::CitiesAndHighways => "cities_and_highways",
            Self::TownsAndMainRoads => "towns_and_main_roads",
            Self::EveryStreet => "every_street",
        }
    }

    /// [`Self::token`]'s inverse, or `None` for a spelling this build has no
    /// level for.
    pub(crate) fn from_token(token: &str) -> Option<Self> {
        DETAIL_LEVELS
            .into_iter()
            .find(|level| level.token() == token)
    }
}

impl Default for DetailLevel {
    /// The middle level: the roads you would drive, which is what a person
    /// downloading a place for offline use is nearly always after, and the
    /// only one of the three that is not an extreme.
    fn default() -> Self {
        Self::TownsAndMainRoads
    }
}

/// What an area cut to `max_zoom` lets you make out, against an archive whose
/// ceiling is `archive_max_zoom`.
///
/// A record carries the depth its download asked for, which need not be one of
/// the levels' exact zooms — a hand-placed area, or a ceiling that moved after
/// the download. The shallowest level that covers the depth is the honest
/// label; anything past the deepest gets the deepest, because there is no data
/// past it to describe.
///
/// **The ceiling is the archive's, not a constant**, for
/// [`DetailLevel::zoom_in`]'s reason. `None` — no header read this session —
/// reads as the deepest label rather than as an invented zoom, and refines the
/// moment one lands.
pub(crate) fn detail_label(max_zoom: u8, archive_max_zoom: Option<u8>) -> &'static str {
    let Some(ceiling) = archive_max_zoom else {
        return DetailLevel::EveryStreet.label();
    };
    DETAIL_LEVELS
        .into_iter()
        .find(|level| max_zoom <= level.zoom_in(ceiling))
        .unwrap_or(DetailLevel::EveryStreet)
        .label()
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
    /// The basemap archive generation this run is cutting from, so the record
    /// written when it completes dates the bytes it actually holds.
    pub(crate) generation: String,
    /// The terrain archive generation, or `None` for a run the user did not
    /// ask the hillshade of. The two archives are released on their own
    /// cadences, so one string could never date both.
    pub(crate) terrain_generation: Option<String>,
    engine: BasemapDownload,
}

impl ActiveDownload {
    /// Start `spec` against `source` — and `terrain` when the user asked for
    /// the hillshade — publishing into `store`.
    pub(crate) fn start<S, T, St>(
        source: S,
        terrain: Option<(T, String)>,
        store: St,
        spec: AreaSpec,
        generation: String,
        ctx: Context,
    ) -> Self
    where
        S: crate::basemap_archive::ArchiveRangeSource,
        T: crate::basemap_archive::ArchiveRangeSource,
        St: SegmentStore,
    {
        let (terrain_source, terrain_generation) = match terrain {
            Some((source, generation)) => (Some(source), Some(generation)),
            None => (None, None),
        };
        let engine =
            BasemapDownload::start_with_terrain(source, terrain_source, store, spec.clone(), ctx);
        Self {
            spec,
            generation,
            terrain_generation,
            engine,
        }
    }

    /// [`Self::start`] with the segment cap handed in — the engine's own
    /// test-only door, for the same reason: the committed fixture is 419 KB
    /// and a multi-segment plan cannot be cut out of it at the production cap.
    #[cfg(test)]
    pub(crate) fn start_with_segment_bytes<S, T, St>(
        source: S,
        terrain: Option<(T, String)>,
        store: St,
        spec: AreaSpec,
        generation: String,
        ctx: Context,
        segment_bytes: u64,
    ) -> Self
    where
        S: crate::basemap_archive::ArchiveRangeSource,
        T: crate::basemap_archive::ArchiveRangeSource,
        St: SegmentStore,
    {
        let (terrain_source, terrain_generation) = match terrain {
            Some((source, generation)) => (Some(source), Some(generation)),
            None => (None, None),
        };
        let engine = BasemapDownload::with_terrain_and_segment_bytes(
            source,
            terrain_source,
            store,
            spec.clone(),
            ctx,
            segment_bytes,
        );
        Self {
            spec,
            generation,
            terrain_generation,
            engine,
        }
    }

    /// Where the run stands, off the engine's own always-on counters. See
    /// [`DownloadProgress`] for which figure carries which denominator.
    pub(crate) fn progress(&self) -> DownloadProgress {
        self.engine.progress()
    }

    /// How the run ended, once it has — both archives summed.
    pub(crate) fn outcome(&self) -> Option<DownloadOutcome> {
        self.engine.outcome()
    }

    /// Each archive's own cut, once the run has ended — what the record needs
    /// to reconcile the two halves apart.
    pub(crate) fn holdings(&self) -> Option<AreaHoldings> {
        self.engine.holdings()
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

/// What the store answered about one area: how much of its cut is here, and
/// what those stored segments occupy.
///
/// The held figure is the **artifacts'** size — tile data plus each segment's
/// own header, directories and metadata copy — because that is what a store
/// can answer from a listing. The asked-for figure a row pairs it with is the
/// record's tile-byte total, so the pair is a little short of like-for-like;
/// closing that would mean re-planning the area against the live archive on
/// every launch, which is a network walk of every tile in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AreaFact {
    /// Segments of this area's cut the store holds.
    pub(crate) status: AreaStatus,
    /// What those segments occupy.
    pub(crate) held: DataSize,
}

/// The answers the worker has published so far. An id absent from this is one
/// the store has not been asked about yet, or one it would not answer for.
#[derive(Default)]
struct AreaFacts {
    facts: HashMap<String, AreaFact>,
}

/// Both of an area's halves as the store lists them.
///
/// The terrain listing is asked for **only when the record says the area has
/// one**: an area downloaded without the hillshade must not spend a listing
/// discovering that, and an empty map is the same answer it would get.
///
/// One `Err` for the pair, on purpose — a half-answered reconcile would put a
/// figure short by one archive on the glass, which is the held-of-asked pair
/// lying in the direction that reads as data loss.
async fn listed_halves<St: SegmentStore>(
    store: &St,
    area: &DownloadedArea,
) -> Result<(BTreeMap<u32, u64>, BTreeMap<u32, u64>), crate::basemap_download::StoreError> {
    let id = &area.spec.area_id;
    let basemap = store
        .existing_segment_bytes(id, AreaArchive::Basemap)
        .await?;
    let terrain = match area.terrain {
        Some(_) => {
            store
                .existing_segment_bytes(id, AreaArchive::Terrain)
                .await?
        }
        None => BTreeMap::new(),
    };
    Ok((basemap, terrain))
}

/// What `sized`'s segments below `expected` occupy. See [`AreaFact`] for the
/// denominator.
fn held_bytes(sized: &BTreeMap<u32, u64>, expected: u32) -> u64 {
    sized
        .iter()
        .filter(|&(&seg, _)| seg < expected)
        .map(|(_, &bytes)| bytes)
        .sum()
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
                                match listed_halves(&store, &area).await {
                                    Ok((basemap, terrain)) => {
                                        let status = area.reconcile_all(
                                            &basemap.keys().copied().collect(),
                                            &terrain.keys().copied().collect(),
                                        );
                                        // Summed over the same segments the
                                        // count is taken over, per archive: a
                                        // store left holding a longer cut from
                                        // an earlier plan must not read as
                                        // holding more of this one.
                                        let terrain_expected = area
                                            .terrain
                                            .as_ref()
                                            .map_or(0, |hold| hold.segments_expected);
                                        let held = DataSize::from_bytes(
                                            held_bytes(&basemap, area.segments_expected)
                                                + held_bytes(&terrain, terrain_expected),
                                        );
                                        if let Ok(mut facts) = facts.lock() {
                                            facts
                                                .facts
                                                .insert(id.clone(), AreaFact { status, held });
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
                                facts.facts.remove(&area_id);
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
    pub(crate) fn fact(&self, area_id: &str) -> Option<AreaFact> {
        self.facts.lock().ok()?.facts.get(area_id).copied()
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
            facts.facts.remove(area_id);
        }
    }
}

#[cfg(test)]
#[path = "basemap_areas/tests.rs"]
mod tests;
