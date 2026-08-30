//! The download arm of the region drag: what a picked box becomes, how deep
//! it is stored, and what that costs to the byte.
//!
//! # A second arm, not a second gesture
//!
//! [`crate::ui_region::RegionDrag`] takes its bounds from whoever armed it, so
//! this arm hands it [`download_pick_bounds`] and the 3D pick hands it the
//! voxel resampler's. The two arms share the drag, the corner math and the
//! hint chip; they share nothing about what a legal box is. A town under the
//! resampler's 10 km floor is an ordinary download area, and the floor here
//! exists only to refuse a click.
//!
//! # Detail is framed by what you can make out
//!
//! Three levels, named for what is legible at each — never for a zoom number,
//! which is an implementation fact the user has no way to picture. The
//! deepest is the **archive's own** `max_zoom`, read off its header, so the
//! offline ceiling and the render ceiling (`tile_source`'s clamp to
//! `source_max_zoom`) are one number rather than two that can drift; a future
//! archive that goes deeper makes this level deeper without an edit here.
//!
//! # The figure is exact, and it is never computed on the frame thread
//!
//! [`AreaSizeProbe`] is the whole of that: the frame path calls
//! [`AreaSizeProbe::poll`], which reads a `OnceLock` and may *start* a task on
//! the download IO runtime — it never awaits, never opens an archive and never
//! sums a byte. The sum itself is [`crate::pmt_index`]'s distinct
//! `(offset, length)` total for the enumerated tiles, which is what a
//! range-coalescing download actually transfers. Figures already measured stay
//! on screen while another is computed, so a level list never blanks.

use std::sync::{Arc, OnceLock};

use squallar_units::DataSize;

use crate::basemap_archive::ArchiveRangeSource;
// The vocabulary is `basemap_areas`', not this module's: the manage screen
// reads a stored area's depth back through the same three names, and two
// tables would be two answers.
pub(crate) use crate::basemap_areas::{DETAIL_LEVELS, DetailLevel};
use crate::basemap_download::{
    AreaArchive, AreaSpec, DownloadProgress, OfflineQuota, area_tiles_to,
};
use crate::pmt_index::{DownloadBytes, PmtIndex};
use crate::tile_source::runtime;
use crate::ui_region::DragBoundsKm;

/// The download arm's blue: the box in flight, its size chip, and the armed
/// hint chip — one colour, so the chip advertises exactly the box the drag
/// will draw. Deliberately not the 3D pick's yellow: two modal drags that
/// paint the same colour are one drag as far as the eye is concerned.
pub(crate) const DOWNLOAD_ARM_COLOR: egui::Color32 = egui::Color32::from_rgb(130, 200, 255);

/// What the armed hint chip says while the download arm waits for a drag.
pub(crate) const DOWNLOAD_ARM_HINT: &str = "Drag a square to make it available offline";

/// The narrowest half-width the download arm will commit, kilometres.
///
/// **Not a floor on what is worth downloading** — a town is a legitimate area
/// and the resampler's 10 km minimum has no business here. It refuses a
/// *click*: a 1 km box is under half a tile across at any archive's deepest
/// zoom, so nothing below it addresses a different tile set, and a press with
/// no drag would otherwise commit an area.
pub(crate) const MIN_DOWNLOAD_HALF_WIDTH_KM: f64 = 0.5;

/// The widest half-width the download arm will grow to, kilometres — a 2000 km
/// box, which holds any US state and more country than a device is likely to
/// have room for. The size figure is what actually governs; this only keeps
/// the preview and the tile enumeration finite.
pub(crate) const MAX_DOWNLOAD_HALF_WIDTH_KM: f64 = 1000.0;

/// What a size figure that has not landed yet says. Never a number: an
/// estimate dressed as an exact figure is the one thing this feature must not
/// do.
pub(crate) const MEASURING_LABEL: &str = "measuring...";

/// What a size figure says when the archive would not answer. Distinct from
/// [`MEASURING_LABEL`] because the two call for different things from the
/// reader: one is waiting, the other is not.
pub(crate) const UNAVAILABLE_LABEL: &str = "size unavailable";

/// The bounds this arm hands [`crate::ui_region::RegionDrag::begin`].
pub(crate) fn download_pick_bounds() -> DragBoundsKm {
    DragBoundsKm {
        min_half_width_km: MIN_DOWNLOAD_HALF_WIDTH_KM,
        max_half_width_km: MAX_DOWNLOAD_HALF_WIDTH_KM,
    }
}

// ---------------------------------------------------------------------------
// The picked box
// ---------------------------------------------------------------------------

/// A square of ground the user picked, as the drag described it — the raw
/// centre and half-width [`crate::ui_region::RegionDrag::commit`] answers
/// with, before any detail level is applied.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PickedBox {
    /// The box's centre.
    pub centre: squallar_geo::GeoPoint,
    /// Half its width, kilometres.
    pub half_width_km: f64,
}

impl PickedBox {
    /// The box a committed drag describes, or `None` if it describes none.
    pub fn new(centre: squallar_geo::GeoPoint, half_width_km: f64) -> Option<Self> {
        (centre.is_on_earth() && half_width_km.is_finite() && half_width_km > 0.0).then_some(Self {
            centre,
            half_width_km,
        })
    }

    /// The box's extent as the corner math and the box painter speak it.
    pub(crate) fn half_extent(self) -> squallar_radar::voxel::HalfExtentKm {
        squallar_radar::voxel::HalfExtentKm::square(self.half_width_km)
    }

    /// How wide the box is, kilometres — what the chip says.
    pub(crate) fn across_km(self) -> f64 {
        2.0 * self.half_width_km
    }

    /// This box at `level`, against an archive whose ceiling is
    /// `archive_max_zoom`, or `None` for a box with no bounding box.
    ///
    /// **This is the whole of "the drag becomes an area"**: the bbox is the
    /// drag's own corners, and the ceiling is the archive's own header. The
    /// voxel resampler is not consulted at either end.
    pub fn area_spec(self, archive_max_zoom: u8, level: DetailLevel) -> Option<AreaSpec> {
        let max_zoom = level.zoom_in(archive_max_zoom);
        let (nw, se) = crate::ui_region::corners_for(self.centre, self.half_extent())?;
        Some(AreaSpec {
            area_id: self.area_id(max_zoom),
            west: nw.lon,
            south: se.lat,
            east: se.lon,
            north: nw.lat,
            max_zoom,
        })
    }

    /// A stable, filename- and URL-safe identity for this box at `max_zoom`.
    ///
    /// **The zoom is in the id on purpose.** Segments are resumed by index
    /// against a plan, and a plan cut at one depth numbers its segments
    /// differently from a plan cut at another; two depths sharing an id would
    /// let a resume graft one plan's segment onto the other's. Two depths are
    /// two downloads, and the record for each replaces only itself.
    ///
    /// The centre is quantised to 10⁻⁴° (~11 m) and the half-width to 10 m, so
    /// the same box picked twice resumes rather than duplicating. `s` prefixes
    /// a negative rather than `-`, which would read as a second separator.
    fn area_id(self, max_zoom: u8) -> String {
        fn field(value: f64, scale: f64) -> String {
            let bounded = (value * scale).round().clamp(-1e15, 1e15) as i64;
            if bounded < 0 {
                format!("s{}", bounded.unsigned_abs())
            } else {
                format!("{bounded}")
            }
        }
        format!(
            "area-{}-{}-{}-z{max_zoom}",
            field(self.centre.lat, 10_000.0),
            field(self.centre.lon, 10_000.0),
            field(self.half_width_km, 100.0),
        )
    }
}

// ---------------------------------------------------------------------------
// The exact size figure
// ---------------------------------------------------------------------------

/// How many measured figures the probe keeps.
///
/// Three levels of the box in hand plus the previous box's three, **per
/// archive**, so nudging a box back to where it was answers from memory rather
/// than re-measuring, and flipping the terrain checkbox does not throw away the
/// half already read.
const KNOWN_FIGURES: usize = 12;

/// One measurement in flight on the IO runtime.
struct Inflight {
    /// What is being measured, or `None` for the opening read that only wants
    /// the archive's ceiling.
    area: Option<AreaSpec>,
    /// Which archive is being read.
    archive: AreaArchive,
    slot: Arc<OnceLock<Result<Measured, String>>>,
    /// Owns the task. Dropping it cancels the read outright — the same whole
    /// cancel protocol `BasemapDownload` has, for the same reason.
    _runtime: runtime::Runtime,
}

/// What one task read: the archive's ceiling, and the figure it was asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Measured {
    ceiling: u8,
    bytes: Option<DownloadBytes>,
}

/// The live, exact size figure for the box in hand — measured off the frame
/// thread, always.
///
/// One reading at a time, so a box moved across the map cannot queue a hundred
/// archive reads; an ask for a box nobody wants any more is cancelled by
/// dropping its runtime.
pub(crate) struct AreaSizeProbe {
    /// The **base map** archive's own `max_zoom`, once a read has reported it.
    /// `None` until then, which is why the level list draws its rows as
    /// measuring rather than inventing a depth.
    ///
    /// The terrain archive's ceiling is not here and never reaches a detail
    /// level: the levels are named against the ground the download is cut for,
    /// and the terrain half is clamped to its own header inside the read
    /// ([`crate::basemap_download::area_tiles_to`]) exactly as the engine
    /// clamps it.
    ceiling: Option<u8>,
    /// The box every figure here belongs to.
    picked: Option<PickedBox>,
    /// Whether the figures the probe answers with include the hillshade.
    terrain: bool,
    /// Figures measured, newest first, per `(area, archive)`.
    known: Vec<(AreaSpec, AreaArchive, DownloadBytes)>,
    inflight: Option<Inflight>,
}

impl Default for AreaSizeProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl AreaSizeProbe {
    pub(crate) fn new() -> Self {
        Self {
            ceiling: None,
            picked: None,
            terrain: false,
            known: Vec::new(),
            inflight: None,
        }
    }

    /// Point the probe at `picked` — or at nothing, which stops the size work
    /// without discarding what was already measured. The ceiling read is not
    /// a size and is not stopped.
    pub(crate) fn set_box(&mut self, picked: Option<PickedBox>) {
        if self.picked == picked {
            return;
        }
        self.picked = picked;
        self.cancel_stale();
    }

    /// Tell the probe whether the figures it answers with include the terrain
    /// hillshade.
    ///
    /// Flipping this throws **nothing** away: both halves stay in `known`, so a
    /// checkbox toggled twice re-answers from memory. What it changes is which
    /// of them [`Self::figure`] sums, which is why the figure on the glass
    /// moves the moment the box is ticked and is still the archives' own
    /// answer rather than a scaled one.
    pub(crate) fn set_terrain(&mut self, terrain: bool) {
        if self.terrain == terrain {
            return;
        }
        self.terrain = terrain;
        self.cancel_stale();
    }

    /// Drop a reading nobody is waiting for any more.
    fn cancel_stale(&mut self) {
        // A measurement for a box nobody is looking at any more is cancelled
        // rather than waited out: the drop is the cancel. A *terrain* reading
        // is stale the moment the checkbox clears, for the same reason.
        let stale = self.inflight.as_ref().is_some_and(|flight| {
            flight.area.as_ref().is_some_and(|area| !self.wants(area))
                || (flight.archive == AreaArchive::Terrain && !self.terrain)
        });
        if stale {
            self.inflight = None;
        }
    }

    /// The archive's detail ceiling, once a read has reported it.
    pub(crate) fn ceiling(&self) -> Option<u8> {
        self.ceiling
    }

    /// The area the box in hand describes at `level`.
    pub(crate) fn area_spec(&self, level: DetailLevel) -> Option<AreaSpec> {
        self.picked?.area_spec(self.ceiling?, level)
    }

    /// The exact figure for the box in hand at `level` **over the archives the
    /// download will actually fetch**, or `None` while any of them is still
    /// being measured.
    ///
    /// The sum is per-archive exactness added, never re-derived: each half is
    /// [`crate::pmt_index`]'s distinct `(offset, length)` total for that
    /// archive's own tiles, and the two archives are separate files, so no span
    /// can be counted in both. Terrain not yet read answers `None` rather than
    /// the basemap figure alone — a figure short by a whole archive is the
    /// estimate this feature exists not to quote.
    pub(crate) fn figure(&self, level: DetailLevel) -> Option<DownloadBytes> {
        let wanted = self.area_spec(level)?;
        let mut total = self.figure_for(&wanted, AreaArchive::Basemap)?;
        if self.terrain {
            let terrain = self.figure_for(&wanted, AreaArchive::Terrain)?;
            total.bytes += terrain.bytes;
            total.present += terrain.present;
            total.absent += terrain.absent;
        }
        Some(total)
    }

    /// One archive's own figure for `area`, if it has been read.
    fn figure_for(&self, area: &AreaSpec, archive: AreaArchive) -> Option<DownloadBytes> {
        self.known
            .iter()
            .find(|(held, kind, _)| held == area && *kind == archive)
            .map(|(_, _, bytes)| *bytes)
    }

    /// The archives a download of the box in hand would fetch, in read order.
    fn wanted_archives(&self) -> &'static [AreaArchive] {
        if self.terrain {
            &[AreaArchive::Basemap, AreaArchive::Terrain]
        } else {
            &[AreaArchive::Basemap]
        }
    }

    /// [`Self::figure`] as the size it names.
    pub(crate) fn size(&self, level: DetailLevel) -> Option<DataSize> {
        self.figure(level)
            .map(|figure| DataSize::from_bytes(figure.bytes))
    }

    /// Every level whose figure is in hand, in list order — what the quota
    /// arithmetic measures against.
    pub(crate) fn sizes(&self) -> Vec<(DetailLevel, DataSize)> {
        DETAIL_LEVELS
            .into_iter()
            .filter_map(|level| self.size(level).map(|size| (level, size)))
            .collect()
    }

    /// Hand the probe the archive's ceiling without an archive.
    ///
    /// **The tests' seam.** A harness Gui builds inert tile sources and opens
    /// no archive, so nothing would read a header; a test that needs stored
    /// depths named against a ceiling states the ceiling it stands in for.
    #[cfg(test)]
    pub(crate) fn seed_ceiling(&mut self, ceiling: u8) {
        self.ceiling = Some(ceiling);
    }

    /// Hand the probe a ceiling and a figure without an archive.
    ///
    /// **The tests' seam and nothing else.** The probe's own reading is pinned
    /// against committed fixtures in this module's suite; the Gui-level tests
    /// are about what the level list draws with figures in hand, and driving
    /// them through a network the test harness does not have would pin the
    /// absence of a network instead.
    #[cfg(test)]
    pub(crate) fn seed(&mut self, ceiling: u8, level: DetailLevel, bytes: u64) {
        self.seed_archive(ceiling, level, AreaArchive::Basemap, bytes);
    }

    /// [`Self::seed`] for one named archive — the terrain half's seam.
    #[cfg(test)]
    pub(crate) fn seed_archive(
        &mut self,
        ceiling: u8,
        level: DetailLevel,
        archive: AreaArchive,
        bytes: u64,
    ) {
        self.ceiling = Some(ceiling);
        let Some(area) = self.area_spec(level) else {
            return;
        };
        self.remember(
            area,
            archive,
            DownloadBytes {
                bytes,
                present: 0,
                absent: 0,
            },
        );
    }

    /// Whether a read is running right now.
    fn is_measuring(&self) -> bool {
        self.inflight.is_some()
    }

    /// What a level's row says where its size goes.
    ///
    /// **Three states and no fourth**: the exact figure, the fact that it is
    /// being read, or the fact that it is not available. Never a number the
    /// archive did not answer with, and never the word "measuring" over a
    /// probe that has given up — a reader who cannot tell those apart cannot
    /// tell whether waiting will help.
    pub(crate) fn size_label(&self, level: DetailLevel) -> String {
        match self.size(level) {
            Some(size) => size.label(),
            None if self.is_measuring() => MEASURING_LABEL.to_owned(),
            None => UNAVAILABLE_LABEL.to_owned(),
        }
    }

    /// One frame's bookkeeping.
    ///
    /// **Nothing here touches the archive.** It reads a `OnceLock`, files the
    /// answer, and may hand a fresh task to the IO runtime — which is a
    /// `spawn`, not an await. There is no `.await`, no `block_on` and no byte
    /// sum on this path, which is the whole of the off-frame-thread rule for
    /// this feature and is pinned as such by
    /// [`tests::the_frame_facing_poll_does_no_archive_work`].
    ///
    /// Each source closure is called only when a read of **that** archive is
    /// actually about to start, so a frame with nothing to measure builds no
    /// client and parses no URL, and a download with terrain switched off never
    /// touches the hillshade archive at all.
    pub(crate) fn poll<S, T, F, G>(&mut self, ctx: &egui::Context, source: F, terrain_source: G)
    where
        S: ArchiveRangeSource,
        T: ArchiveRangeSource,
        F: FnOnce() -> Option<S>,
        G: FnOnce() -> Option<T>,
    {
        self.harvest();
        if self.inflight.is_some() {
            return;
        }
        let Some((next, archive)) = self.next_ask() else {
            return;
        };

        let slot: Arc<OnceLock<Result<Measured, String>>> = Arc::new(OnceLock::new());
        // One arm per archive rather than one boxed source, because the two
        // sources are different types on some targets and a `dyn` here would
        // put an allocation on the frame path for a value the task owns.
        let runtime = match archive {
            AreaArchive::Basemap => {
                let Some(source) = source() else {
                    return;
                };
                runtime::spawn(read_into(
                    Arc::clone(&slot),
                    ctx.clone(),
                    source,
                    next.clone(),
                ))
            }
            AreaArchive::Terrain => {
                let Some(source) = terrain_source() else {
                    return;
                };
                runtime::spawn(read_into(
                    Arc::clone(&slot),
                    ctx.clone(),
                    source,
                    next.clone(),
                ))
            }
        };
        self.inflight = Some(Inflight {
            area: next,
            archive,
            slot,
            _runtime: runtime,
        });
    }

    /// File a finished read, if one finished.
    fn harvest(&mut self) {
        let Some(flight) = &self.inflight else {
            return;
        };
        let Some(answer) = flight.slot.get() else {
            return;
        };
        let answer = answer.clone();
        let area = flight.area.clone();
        let archive = flight.archive;
        self.inflight = None;
        match answer {
            Ok(measured) => {
                // The detail levels are named against the BASE MAP's depth, so
                // only its header moves the ceiling. The terrain archive is
                // shallower and naming a level against it would rename every
                // row the moment the checkbox was ticked.
                if archive == AreaArchive::Basemap {
                    self.ceiling = Some(measured.ceiling);
                }
                if let (Some(area), Some(bytes)) = (area, measured.bytes) {
                    self.remember(area, archive, bytes);
                }
            }
            Err(error) => log::warn!(
                "the offline area's size could not be measured, so the level list keeps the \
                 figures it already has: {error}"
            ),
        }
    }

    /// What to read next, and out of which archive: the base map's ceiling if
    /// it is not known, otherwise the first `(level, archive)` pair of the box
    /// in hand whose own figure is not.
    fn next_ask(&self) -> Option<(Option<AreaSpec>, AreaArchive)> {
        // The ceiling is read whether or not a box is picked: the manage
        // screen names a stored area's depth against it too, and one header
        // plus root read per session over the archive the ground is already
        // being drawn from is not a cost worth deferring behind a gesture.
        if self.ceiling.is_none() {
            return Some((None, AreaArchive::Basemap));
        }
        for level in DETAIL_LEVELS {
            let Some(area) = self.area_spec(level) else {
                continue;
            };
            for &archive in self.wanted_archives() {
                if self.figure_for(&area, archive).is_none() {
                    return Some((Some(area), archive));
                }
            }
        }
        None
    }

    /// Whether `area` is one of the box in hand's three.
    fn wants(&self, area: &AreaSpec) -> bool {
        DETAIL_LEVELS
            .into_iter()
            .any(|level| self.area_spec(level).as_ref() == Some(area))
    }

    /// Keep `bytes` for `area`, newest first, bounded at [`KNOWN_FIGURES`].
    fn remember(&mut self, area: AreaSpec, archive: AreaArchive, bytes: DownloadBytes) {
        self.known
            .retain(|(held, kind, _)| *held != area || *kind != archive);
        self.known.insert(0, (area, archive, bytes));
        self.known.truncate(KNOWN_FIGURES);
    }
}

/// The spawned task itself: read, file the answer, ask for a repaint.
///
/// A named function rather than an inline `async move` block deliberately —
/// the `.await` belongs to the task and not to [`AreaSizeProbe::poll`], and
/// keeping the two in separate bodies is what lets
/// [`tests::the_frame_facing_poll_does_no_archive_work`] pin the frame-facing
/// one by reading it.
async fn read_into<S: ArchiveRangeSource>(
    slot: Arc<OnceLock<Result<Measured, String>>>,
    ctx: egui::Context,
    source: S,
    area: Option<AreaSpec>,
) {
    let _ = slot.set(measure(source, area).await);
    ctx.request_repaint();
}

/// One read: open the index, take the archive's ceiling off its header, and —
/// when there is an area to measure — sum the distinct `(offset, length)`
/// pairs its tiles address.
///
/// The sum is [`crate::pmt_index`]'s and only its: this function enumerates
/// tiles and hands them over. There is no second byte arithmetic anywhere in
/// this module, which is what makes "the figure equals `download_bytes`" an
/// identity rather than an agreement.
async fn measure<S: ArchiveRangeSource>(
    source: S,
    area: Option<AreaSpec>,
) -> Result<Measured, String> {
    let index = PmtIndex::open(source)
        .await
        .map_err(|error| error.to_string())?;
    let ceiling = index.header().max_zoom;
    let bytes = match &area {
        Some(area) => Some(
            index
                // Clamped to THIS archive's own ceiling, the engine's own
                // enumeration (`plan_area`): a figure summed over tiles the
                // download would never ask for is not the download's figure.
                .download_bytes(area_tiles_to(area, ceiling))
                .await
                .map_err(|error| error.to_string())?,
        ),
        None => None,
    };
    Ok(Measured { ceiling, bytes })
}

// ---------------------------------------------------------------------------
// Quota
// ---------------------------------------------------------------------------

/// A chosen level that will not fit, stated as two quantities and a level that
/// will.
///
/// There is no message field and no severity: a notice the reader cannot act
/// on is a defect in the code, so what this carries is the arithmetic and the
/// alternative, and the view spends them as a figure and a button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct QuotaShortfall {
    /// What the chosen level costs.
    pub needs: DataSize,
    /// What the origin has left.
    pub free: DataSize,
    /// The deepest measured level that fits, or `None` when none does.
    pub alternative: Option<DetailLevel>,
}

/// `chosen`'s shortfall against `free`, or `None` when it fits — **and also
/// `None` when there is no free-space figure**, because an unknown quota is
/// not a refusal and must never be shown as one.
///
/// `sizes` is what the probe has actually measured; a level with no figure yet
/// is not offered as an alternative, since offering an unmeasured level would
/// be offering a guess.
pub(crate) fn quota_shortfall(
    sizes: &[(DetailLevel, DataSize)],
    chosen: DetailLevel,
    free: Option<DataSize>,
) -> Option<QuotaShortfall> {
    let free = free?;
    let needs = sizes
        .iter()
        .find(|(level, _)| *level == chosen)
        .map(|(_, size)| *size)?;
    if needs <= free {
        return None;
    }
    Some(QuotaShortfall {
        needs,
        free,
        alternative: sizes
            .iter()
            .filter(|(level, size)| *level != chosen && *size <= free)
            .max_by_key(|(level, _)| *level)
            .map(|(level, _)| *level),
    })
}

/// The shortfall as one line: **two quantities, no apology**.
pub(crate) fn shortfall_line(short: QuotaShortfall) -> String {
    format!(
        "Needs {} - {} free",
        short.needs.label(),
        short.free.label()
    )
}

/// The action that answers a shortfall: switch to the level that does fit.
pub(crate) fn shortfall_action_label(level: DetailLevel) -> String {
    format!("Use {}", level.label())
}

/// Free space as `quota` reports it, or `None` when it does not.
pub(crate) fn free_space(quota: Option<OfflineQuota>) -> Option<DataSize> {
    quota?.free()
}

// ---------------------------------------------------------------------------
// The in-flight block
// ---------------------------------------------------------------------------

/// What the in-flight block says before there is anything to fill a bar with.
///
/// **The plan is cut before a byte moves** and that cut runs for minutes over
/// a large area. A bar pinned at 0% through it is indistinguishable from a
/// hang, and a fabricated percentage would be worse; this says which of the
/// two states the run is in, and the spinner beside it says it is alive.
pub(crate) const PREPARING_LABEL: &str = "Preparing download...";

/// The exact byte counter that sits beside the bar.
///
/// **The figures stay on the glass, in full.** The bar is how far along the
/// run is at a glance; these are what it is measuring, and a percentage is not
/// a substitute for either. The denominator is this run's own work — a resume
/// fetches only what is missing — which is why the line says so.
pub(crate) fn progress_bytes_line(progress: DownloadProgress) -> String {
    format!(
        "{} of {} fetched this run",
        progress.bytes_done.label(),
        progress.bytes_total.label()
    )
}

/// The one in-flight block both surfaces draw: the map panel's and the manage
/// screen's.
///
/// One function rather than two call sites, so the two views of the *same*
/// download cannot come to two answers about where it stands — and so the
/// no-part-counts rule has one place to hold.
pub(crate) fn render_download_progress(ui: &mut egui::Ui, progress: DownloadProgress) {
    match progress.byte_fraction() {
        Some(fraction) => {
            ui.add(egui::ProgressBar::new(fraction).show_percentage());
            ui.label(egui::RichText::new(progress_bytes_line(progress)).small());
        }
        None => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(egui::RichText::new(PREPARING_LABEL).small());
            });
        }
    }
}

#[cfg(test)]
#[path = "ui_download_area/tests.rs"]
mod tests;
