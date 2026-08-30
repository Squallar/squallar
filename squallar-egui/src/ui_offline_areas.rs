//! The Downloaded areas screen: one settings row, every area a sub-row.
//!
//! # One id, many areas
//!
//! [`SETTINGS_ROWS`](super::settings::SETTINGS_ROWS) is `&'static`, so the
//! areas cannot each be a row id — the list is a runtime fact and the table is
//! not. Everything here draws inside the single `offline.areas` arm, and the
//! parity walk's claim is therefore that *the screen* is reachable at every
//! width, not that each area is.
//!
//! # It draws an empty state, never nothing
//!
//! `parity_walk::walk_settings` asserts every row is drawn and on screen from
//! a fresh `Gui` at every width class, phone included, and a fresh `Gui` has
//! no areas. An arm that drew nothing would fail that walk — and "No
//! downloaded areas." is the honest design regardless: a screen that vanishes
//! when it is empty cannot tell a user the feature exists.
//!
//! # A half-download is unrenderable as done
//!
//! An area whose segments the store no longer all holds draws **"3 of 7
//! parts" in place of its size**. Not beside it: a size is what a finished
//! area has, and printing one next to a part count would be the same claim the
//! counts exist to refuse. The counts come from
//! [`AreaMaintenance`](crate::basemap_areas::AreaMaintenance), recomputed off
//! the store's own listing, never from a flag.
//!
//! # What is offline is these areas, not the app
//!
//! There is no offline mode in this app and this screen does not imply one.
//! The header says what the areas hold and what still needs a connection; that
//! boundary is a decision, not a gap.

use egui::RichText;

use crate::basemap_areas::{ActiveDownload, AreaMaintenance, detail_label, generation_note};
use crate::basemap_download::{AreaSpec, AreaStatus, DownloadedArea};

const AREA_ROW_SPACING: f32 = 6.0;

/// The screen's heading.
pub(crate) const DOWNLOADED_AREAS_HEADING: &str = "Downloaded areas";

/// What the screen draws when this device holds none.
pub(crate) const NO_AREAS_NOTE: &str = "No downloaded areas.";

/// The one header note, carrying both things that must be said once rather
/// than per row: what a downloaded area actually makes available, and the
/// denominator every size below is in.
///
/// **Decimal MB and GB (10⁶ / 10⁹)** — [`DataSize`](squallar_units::DataSize)'s
/// denominator, and what a phone's storage screen counts in. Named here, never
/// re-derived on a row.
pub(crate) const AREAS_SCOPE_NOTE: &str = "A downloaded area holds the base map \
    and the static reference layers for that rectangle, so they draw without a \
    connection. Radar, alerts and forecasts are always fetched live. Sizes are \
    decimal MB and GB - what your device's storage screen counts in.";

/// What a row draws in the size slot while the store has not answered for it.
pub(crate) const CHECKING_NOTE: &str = "Checking storage...";

/// What the user asked a row's buttons to do, decided while the area list is
/// borrowed and applied once it is not.
enum AreaCommand {
    /// Drop the record and remove the bytes.
    Delete(String),
    /// Fetch what this area is missing, or re-cut it at the current
    /// generation. One start either way: resume *is* the set difference.
    Download(AreaSpec),
}

impl super::Gui {
    /// The Downloaded areas settings row, sub-rows and all.
    pub(in crate::ui) fn render_downloaded_areas(&mut self, ui: &mut egui::Ui) {
        ui.heading(DOWNLOADED_AREAS_HEADING);
        ui.add_space(4.0);
        ui.label(RichText::new(AREAS_SCOPE_NOTE).small().weak());
        ui.add_space(AREA_ROW_SPACING);

        let store_reachable = self.ensure_area_maintenance(ui.ctx());
        if let Some(maintenance) = self.area_maintenance.as_mut() {
            maintenance.reconcile_unknown(&self.downloaded_areas);
        }

        self.render_active_download(ui);

        if self.downloaded_areas.is_empty() {
            ui.label(NO_AREAS_NOTE);
            return;
        }

        let live_generation =
            crate::basemap_archive::block_cache::generation_for_url(&crate::tiles::archive_url());
        // The archive's own ceiling, from the one header read the size probe
        // makes each session. A stored depth is named against it rather than
        // against a constant, so a deeper archive renames every row without an
        // edit; `None` until the read lands, which reads as the deepest level
        // and refines rather than inventing a zoom.
        let archive_max_zoom = self.download_size.ceiling();
        let mut command = None;
        for area in &self.downloaded_areas {
            let status = self
                .area_maintenance
                .as_ref()
                .and_then(|maintenance| maintenance.status(&area.spec.area_id));
            let asked = render_area(
                ui,
                area,
                status,
                &live_generation,
                store_reachable,
                archive_max_zoom,
            );
            // First press wins. Only one button can be clicked in a frame, so
            // this decides nothing in practice - it just refuses to let a
            // later row silently discard an earlier row's command.
            command = command.or(asked);
            ui.add_space(AREA_ROW_SPACING);
        }

        match command {
            Some(AreaCommand::Delete(area_id)) => self.delete_downloaded_area(&area_id),
            Some(AreaCommand::Download(spec)) => self.start_area_download(spec, ui.ctx()),
            None => {}
        }
    }

    /// The in-flight run, if there is one: the engine's own counters, each
    /// figure beside the denominator it is against.
    fn render_active_download(&self, ui: &mut egui::Ui) {
        let Some(active) = self.active_download.as_ref() else {
            return;
        };
        let progress = active.progress();
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(format!("Downloading {}", active.spec.area_id));
        });
        let fraction = if progress.bytes_total.bytes() == 0 {
            0.0
        } else {
            #[expect(
                clippy::cast_precision_loss,
                reason = "a bar's fill is a fraction of its width; the exact \
                          figures are the labels below it"
            )]
            {
                progress.bytes_done.bytes() as f32 / progress.bytes_total.bytes() as f32
            }
        };
        ui.add(egui::ProgressBar::new(fraction).show_percentage());
        ui.label(
            RichText::new(format!(
                "{} of {} parts stored",
                progress.segments_done, progress.segments_total
            ))
            .small(),
        );
        ui.label(
            RichText::new(format!(
                "{} of {} fetched this run",
                progress.bytes_done.label(),
                progress.bytes_total.label()
            ))
            .small()
            .weak(),
        );
        ui.add_space(AREA_ROW_SPACING);
    }

    /// Build the maintenance worker if this platform has a store and one is
    /// not running yet, answering whether there is a store at all.
    ///
    /// Lazy because the store is chosen from
    /// [`basemap_dir`](Self::basemap_dir), which arrives at construction while
    /// the area list arrives at config load: there is no single earlier
    /// instant where both are known.
    fn ensure_area_maintenance(&mut self, ctx: &egui::Context) -> bool {
        if self.area_maintenance.is_none() {
            self.area_maintenance = crate::tiles::offline_store(self.basemap_dir.as_deref())
                .map(|store| AreaMaintenance::start(store, ctx.clone()));
        }
        self.area_maintenance.is_some()
    }

    /// Drop `area_id`'s record, then its bytes.
    ///
    /// **Record first**, [`Gui::forget_downloaded_area`](Self::forget_downloaded_area)'s
    /// written-down order: an area whose bytes outlive its record is invisible,
    /// where the reverse is a listed area with nothing behind it.
    fn delete_downloaded_area(&mut self, area_id: &str) {
        self.forget_downloaded_area(area_id);
        if let Some(maintenance) = self.area_maintenance.as_mut() {
            maintenance.delete(area_id);
        }
        if self
            .active_download
            .as_ref()
            .is_some_and(|active| active.spec.area_id == area_id)
        {
            self.active_download = None;
        }
    }

    /// Start (or resume, or re-cut) `spec`'s download against the archive this
    /// build reads.
    ///
    /// **Only ever from a press.** Nothing calls this on a launch, on a
    /// reconcile or on a generation difference: auto-resuming a 400 MB
    /// download on a metered connection is the exact opposite of what this
    /// feature is for. Resume needs no separate path — the engine skips the
    /// segments the store already holds, so a resume *is* a start.
    pub(in crate::ui) fn start_area_download(&mut self, spec: AreaSpec, ctx: &egui::Context) {
        let Some(store) = crate::tiles::offline_store(self.basemap_dir.as_deref()) else {
            return;
        };
        // The size figure and the download read the archive through one
        // constructor, so a quoted figure and the bytes that arrive cannot
        // come from two different clients or two different URLs.
        let source = match crate::tiles::archive_range_source() {
            Ok(source) => source,
            Err(error) => {
                log::error!("the basemap archive is not a usable URL to download from: {error}");
                return;
            }
        };
        let generation =
            crate::basemap_archive::block_cache::generation_for_url(&crate::tiles::archive_url());
        self.active_download = Some(ActiveDownload::start(
            source,
            store,
            spec,
            generation,
            ctx.clone(),
        ));
    }

    /// Publish a finished run and let go of its engine — the per-frame drive's
    /// one call into this screen's machinery.
    ///
    /// A `Complete` outcome is the only one that yields a record
    /// ([`DownloadedArea::from_outcome`]), so a `Partial` or `Failed` run
    /// leaves the area's *previous* record — or no record at all — exactly as
    /// it was. Either way the store is re-asked, because what it holds moved.
    pub(in crate::ui) fn settle_offline_download(&mut self) {
        let Some(active) = self.active_download.as_ref() else {
            return;
        };
        let Some(outcome) = active.outcome() else {
            return;
        };
        let area_id = active.spec.area_id.clone();
        if let Some(record) =
            DownloadedArea::from_outcome(active.spec.clone(), active.generation.clone(), &outcome)
        {
            self.record_downloaded_area(record);
        }
        self.active_download = None;
        if let Some(maintenance) = self.area_maintenance.as_mut() {
            maintenance.recheck(&area_id);
        }
    }
}

/// One area's sub-row, and the command its buttons asked for.
fn render_area(
    ui: &mut egui::Ui,
    area: &DownloadedArea,
    status: Option<AreaStatus>,
    live_generation: &str,
    store_reachable: bool,
    // The live archive's own detail ceiling, once a header read has reported
    // one - what a stored depth is named against, rather than a constant.
    archive_max_zoom: Option<u8>,
) -> Option<AreaCommand> {
    let mut command = None;
    // The name and the figure on one line, in the panel's own left-to-right
    // flow. **Not right-aligned**: a trailing layout inside the settings body
    // takes the whole content width rather than the panel's, which lands the
    // figure over the map beside it - measured, not feared.
    ui.horizontal(|ui| {
        ui.label(RichText::new(&area.spec.area_id).strong());
        ui.label(size_or_parts(area, status));
    });
    ui.label(
        RichText::new(detail_label(area.spec.max_zoom, archive_max_zoom))
            .small()
            .weak(),
    );
    let note = generation_note(&area.generation, live_generation);
    if let Some(note) = note.as_ref() {
        ui.label(RichText::new(note.line()).small().weak());
    }

    // Resume and Update are the same start against a different shortfall, so a
    // row offers exactly one of them: an area missing segments needs those
    // before it needs a newer cut.
    let incomplete = status.is_some_and(|status| !status.is_complete());
    let updatable = note.is_some_and(|note| note.update_available);
    ui.add_enabled_ui(store_reachable, |ui| {
        ui.horizontal(|ui| {
            if incomplete && ui.button("Resume").clicked() {
                command = Some(AreaCommand::Download(area.spec.clone()));
            }
            if !incomplete && updatable && ui.button("Update").clicked() {
                command = Some(AreaCommand::Download(area.spec.clone()));
            }
            if ui.button("Delete").clicked() {
                command = Some(AreaCommand::Delete(area.spec.area_id.clone()));
            }
        });
    });
    command
}

/// The figure a row shows for how much of the area is here.
///
/// Three states, and the middle one is the whole point: a size **only** for an
/// area the store holds whole, part counts **in place of** a size for one it
/// does not, and neither for one it has not answered about.
fn size_or_parts(area: &DownloadedArea, status: Option<AreaStatus>) -> String {
    match status {
        None => CHECKING_NOTE.to_owned(),
        Some(status) if status.is_complete() => area.bytes.label(),
        Some(status) => format!("{} of {} parts", status.present, status.expected),
    }
}

#[cfg(test)]
#[path = "ui_offline_areas/tests.rs"]
mod tests;
