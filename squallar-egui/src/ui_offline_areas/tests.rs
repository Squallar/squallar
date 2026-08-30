//! The Downloaded areas screen, driven like a user: opened through the menu,
//! scrolled to, read off the glass and clicked.
//!
//! **Against a real store.** Every status these tests read comes from an
//! [`FsSegmentStore`](crate::basemap_download::FsSegmentStore) over a real
//! temporary directory, listing real files — so "3 of 7 parts" is the store's
//! answer rather than a double's. What is faked is only the *content* of a
//! segment: `existing_segments` is a listing, and it does not read a byte.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::*;
use crate::input_harness::InputHarness;
use crate::ui::SETTINGS_ROWS;
use squallar_units::DataSize;

/// The size of the complete area, chosen so its label ("112 MB") appears
/// nowhere else the screen draws — the negative assertion below depends on
/// that.
const COMPLETE_BYTES: u64 = 112_345_678;
const COMPLETE_SIZE_LABEL: &str = "112 MB";

/// A per-test directory under the OS temp dir, removed on drop — the
/// `basemap_download` suite's shape.
struct TempDir(PathBuf);

impl TempDir {
    fn new(test: &str) -> Self {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "squallar-offline-areas-{}-{test}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("a temp dir should be creatable");
        Self(path)
    }

    /// Publish `count` segments of `area_id`, as the store's own naming
    /// contract spells them. The bytes are never read by a listing.
    fn place_segments(&self, area_id: &str, count: u32) {
        for seg in 0..count {
            std::fs::write(self.0.join(format!("{area_id}.{seg}.pmtiles")), b"segment")
                .expect("a segment should be writable");
        }
    }

    fn names(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(&self.0) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .flatten()
            .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
            .collect();
        names.sort();
        names
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A record for `area_id`, cut into `segments` at `max_zoom`, from
/// `generation`.
fn area(area_id: &str, segments: u32, max_zoom: u8, generation: &str) -> DownloadedArea {
    DownloadedArea {
        spec: AreaSpec {
            area_id: area_id.to_owned(),
            west: -98.25,
            south: 34.75,
            east: -96.50,
            north: 36.25,
            max_zoom,
        },
        segments_expected: segments,
        bytes: DataSize::from_bytes(COMPLETE_BYTES),
        generation: generation.to_owned(),
    }
}

/// The generation the shipped archive carries, so an area cut from it reads as
/// current. Derived, never pasted: a URL change must not silently turn every
/// test area into an out-of-date one.
fn live_generation() -> String {
    crate::basemap_archive::block_cache::generation_for_url(&crate::tiles::archive_url())
}

/// A generation strictly older than the shipped one — the step-11 case.
const OLDER_GENERATION: &str = "basemap_2Fomt-20250701.pmtiles";

/// A screen tall enough to hold the whole screen without scrolling it out
/// from under an assertion. The width is the harness default; only the height
/// matters here, and the parity walk is what covers the narrow classes.
const TALL_SCREEN: egui::Vec2 = egui::vec2(1024.0, 1600.0);

/// A harness on [`TALL_SCREEN`] pointed at `dir` for offline areas.
/// The published archive's detail ceiling, which this harness stands in for.
///
/// **Stated here rather than compiled into the app.** A harness Gui builds
/// inert tile sources and opens no archive, so no header read happens and
/// `detail_label` has no ceiling to name a stored depth against — the levels
/// are steps below the archive's own `max_zoom`, never fixed zooms. A test
/// that asserts a level therefore has to say which archive it is asserting
/// against, and this is that statement.
const HARNESS_ARCHIVE_CEILING: u8 = 14;

fn harness_over(dir: &TempDir) -> InputHarness {
    let mut h = InputHarness::with_screen(TALL_SCREEN);
    h.use_basemap_dir(dir.0.clone());
    h.seed_archive_ceiling(HARNESS_ARCHIVE_CEILING);
    h
}

/// Where a scroll gesture has to land to move the settings body: over the
/// inspector, never the map — the parity walk's own `inspector_scroll_pos`.
fn scroll_pos(h: &InputHarness) -> egui::Pos2 {
    h.inspector_rect()
        .expect("the inspector must be on screen to be scrolled")
        .center()
}

/// Open Settings and scroll the Downloaded areas row onto the glass, the way
/// the parity walk reaches a row.
fn open_areas_screen(h: &mut InputHarness) {
    h.open_settings();
    assert!(
        SETTINGS_ROWS.contains(&"offline.areas"),
        "the row table no longer lists offline.areas"
    );
    let pos = scroll_pos(h);
    let found = h.scroll_until(pos, egui::vec2(0.0, -160.0), 120, |h| {
        h.settings_row("offline.areas")
            .is_some_and(|drawn| h.screen_rect().contains(drawn.rect.center()))
    });
    assert!(found, "the Downloaded areas row never reached the glass");
}

/// Frame until the store has answered for every listed area, or give up. The
/// worker is a real task on its own runtime; this is how a test waits for it
/// without asserting on a clock.
fn settle_statuses(h: &mut InputHarness, ids: &[&str]) {
    let start = Instant::now();
    loop {
        let answered = ids.iter().all(|id| {
            h.gui()
                .area_maintenance
                .as_ref()
                .and_then(|maintenance| maintenance.status(id))
                .is_some()
        });
        if answered {
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "the offline store never answered for {ids:?}"
        );
        h.frame_after(1.0 / 60.0);
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// The band the Downloaded areas row occupies: its own vertical extent,
/// narrowed to the inspector's column.
///
/// The narrowing is load-bearing — a settings row's probe carries the whole
/// content width, so its bare rect also spans the map beside the panel and
/// would sweep up the attribution and the timeline as if the screen had drawn
/// them.
fn row_rect(h: &InputHarness) -> egui::Rect {
    let row = h.settings_row("offline.areas").expect("the row drew").rect;
    let column = h
        .inspector_rect()
        .expect("the inspector is on screen while settings are open");
    egui::Rect::from_x_y_ranges(column.x_range(), row.y_range()).expand(4.0)
}

/// Every text the settings row drew, for the assertions below.
fn row_text(h: &InputHarness) -> Vec<String> {
    h.painted_text_strings_in(row_rect(h))
}

/// Whether `needle` appears anywhere the settings row drew.
fn row_says(h: &InputHarness, needle: &str) -> bool {
    row_text(h).iter().any(|text| text.contains(needle))
}

// ---------------------------------------------------------------------------
// The empty state
// ---------------------------------------------------------------------------

/// A device that has downloaded nothing gets a screen that says so.
///
/// The parity walk proves the *row* is drawn and on screen at every width, but
/// it is satisfied by the section break alone — a heading over nothing would
/// pass it. This is the assertion that the empty state itself exists, and the
/// reason it must: a screen that vanishes when it is empty cannot tell a user
/// the feature is there.
#[test]
fn a_device_with_no_areas_draws_an_empty_state_rather_than_nothing() {
    let mut h = InputHarness::with_screen(TALL_SCREEN);
    assert!(h.gui().downloaded_areas().is_empty());
    open_areas_screen(&mut h);

    assert!(
        row_says(&h, NO_AREAS_NOTE),
        "the empty screen never drew {NO_AREAS_NOTE:?}; it drew {:?}",
        row_text(&h),
    );
    assert!(
        row_says(&h, DOWNLOADED_AREAS_HEADING),
        "the screen drew no heading"
    );
}

/// The one place the byte denominator is named — the header, not a row.
///
/// Decimal MB/GB is a property of every figure the screen prints, so stating
/// it per row would be the same fact repeated N times and dropped when N is
//// **Users know what MB means.** The screen states sizes and never explains
/// their base. The positive half — a real size is on the glass — is what keeps
/// the negative from passing on an empty screen.
#[test]
fn the_screen_states_sizes_without_explaining_their_denominator() {
    let dir = TempDir::new("denominator");
    dir.place_segments("tulsa", 4);
    let mut h = harness_over(&dir);
    h.gui_mut()
        .record_downloaded_area(area("tulsa", 4, 12, &live_generation()));
    open_areas_screen(&mut h);
    settle_statuses(&mut h, &["tulsa"]);
    h.frame_after(1.0 / 60.0);

    let drawn = row_text(&h);
    assert!(
        drawn
            .iter()
            .any(|row| row.contains(" MB") || row.contains(" GB")),
        "no size reached the glass, so the negative below proves nothing; it drew {drawn:?}",
    );
    assert!(
        !drawn
            .iter()
            .any(|row| row.contains("decimal") || row.contains("1,000,000")),
        "the screen explained its byte denominator; it drew {drawn:?}",
    );
}

/// The screen describes what these areas hold, and does not imply the app
/// works without a connection. There is no offline mode here and this is not
/// the change that builds one.
#[test]
fn the_screen_does_not_promise_an_offline_app() {
    let note = AREAS_SCOPE_NOTE.to_ascii_lowercase();
    assert!(
        note.contains("base map") && note.contains("reference layers"),
        "the header does not say what a downloaded area actually holds",
    );
    assert!(
        note.contains("fetched live"),
        "the header does not say what still needs a connection",
    );
    for promise in [
        "works offline",
        "offline mode",
        "use the app offline",
        "without internet",
    ] {
        assert!(
            !note.contains(promise),
            "the header reads {AREAS_SCOPE_NOTE:?}, which promises {promise:?} - \
             there is no offline mode in this app",
        );
    }
}

// ---------------------------------------------------------------------------
// Complete, partial, and the figure that must not appear
// ---------------------------------------------------------------------------

/// A complete area shows its exact size, its detail level and its vintage.
#[test]
fn a_complete_area_shows_its_size_and_detail_level() {
    let dir = TempDir::new("complete");
    dir.place_segments("ok-central", 7);
    let mut h = harness_over(&dir);
    h.gui_mut()
        .record_downloaded_area(area("ok-central", 7, 12, &live_generation()));
    open_areas_screen(&mut h);
    settle_statuses(&mut h, &["ok-central"]);
    h.frame_after(1.0 / 60.0);

    let text = row_text(&h);
    assert!(
        row_says(&h, "ok-central"),
        "the area is not named: {text:?}"
    );
    assert!(
        row_says(&h, COMPLETE_SIZE_LABEL),
        "a complete area does not show its size: {text:?}",
    );
    assert!(
        row_says(&h, "Towns and main roads"),
        "a complete area does not show its detail level: {text:?}",
    );
    assert!(
        row_says(&h, "Map data August 2026"),
        "a complete area does not state which month's map data it holds: {text:?}",
    );
    assert!(
        !row_says(&h, CHECKING_NOTE),
        "the row is still checking after the store answered: {text:?}",
    );
    assert!(
        !row_says(&h, "parts"),
        "a complete area draws a part count: {text:?}",
    );
}

/// **The negative assertion.** A half-held area draws its part counts *in
/// place of* a size, never beside one: the defect this guards is rendering a
/// half-download as done, and only the absence of the size figure rules it
/// out. The record still carries the byte total — it says what the download
/// asked for — so a screen that printed it would be reading a real number and
/// making a false claim with it.
#[test]
fn a_partial_area_draws_its_part_counts_and_no_size() {
    let dir = TempDir::new("partial");
    dir.place_segments("norman", 3);
    let mut h = harness_over(&dir);
    let record = area("norman", 7, 14, &live_generation());
    assert_eq!(
        record.bytes.label(),
        COMPLETE_SIZE_LABEL,
        "the record carries a size, which is exactly what must not be drawn",
    );
    h.gui_mut().record_downloaded_area(record);
    open_areas_screen(&mut h);
    settle_statuses(&mut h, &["norman"]);
    h.frame_after(1.0 / 60.0);

    let text = row_text(&h);
    assert!(
        row_says(&h, "3 of 7 parts"),
        "the half-held area does not draw its counts: {text:?}",
    );
    assert!(
        !row_says(&h, COMPLETE_SIZE_LABEL),
        "the half-held area drew its size {COMPLETE_SIZE_LABEL:?} - a \
         half-download rendered as done: {text:?}",
    );
    // The counts are the store's answer, not the record's: the record asked
    // for seven and the directory holds three.
    assert_eq!(
        h.gui()
            .area_maintenance
            .as_ref()
            .and_then(|maintenance| maintenance.status("norman")),
        Some(crate::basemap_download::AreaStatus {
            present: 3,
            expected: 7
        }),
    );
}

/// Resume is offered on a half-held area and nothing takes it: a fresh launch
/// that auto-resumed a 400 MB pull on a metered connection is the exact
/// opposite of what this feature is for.
#[test]
fn a_partial_area_offers_a_resume_and_no_frame_takes_it() {
    let dir = TempDir::new("resume-offer");
    dir.place_segments("norman", 3);
    let mut h = harness_over(&dir);
    h.gui_mut()
        .record_downloaded_area(area("norman", 7, 14, &live_generation()));
    open_areas_screen(&mut h);
    settle_statuses(&mut h, &["norman"]);
    h.frames_for(20, 1.0 / 60.0);

    assert!(
        row_says(&h, "Resume"),
        "no resume is offered: {:?}",
        row_text(&h)
    );
    assert!(
        h.gui().active_download.is_none(),
        "a download started without anyone pressing anything",
    );
    assert_eq!(
        dir.names(),
        vec![
            "norman.0.pmtiles".to_owned(),
            "norman.1.pmtiles".to_owned(),
            "norman.2.pmtiles".to_owned(),
        ],
        "the store moved while nobody asked it to",
    );
}

// ---------------------------------------------------------------------------
// The generation fact
// ---------------------------------------------------------------------------

/// An area from an older archive generation stays usable — it keeps its size,
/// not a part count — states its vintage as a fact with the update beside it,
/// and nothing re-downloads it.
///
/// **Keep, never force re-download**: expiring a 300 MB download because OSM
/// data refreshed is precisely wrong for the only user this feature has.
#[test]
fn an_older_generation_area_stays_usable_and_only_offers_an_update() {
    let dir = TempDir::new("older-generation");
    dir.place_segments("ok-central", 7);
    let mut h = harness_over(&dir);
    h.gui_mut()
        .record_downloaded_area(area("ok-central", 7, 12, OLDER_GENERATION));
    open_areas_screen(&mut h);
    settle_statuses(&mut h, &["ok-central"]);
    h.frames_for(20, 1.0 / 60.0);

    let text = row_text(&h);
    // Usable: the size is drawn, so it reads as an area the device holds.
    assert!(
        row_says(&h, COMPLETE_SIZE_LABEL),
        "an older-generation area lost its size - it reads as not held: {text:?}",
    );
    assert!(
        row_says(&h, "Map data July 2025 \u{b7} update available"),
        "the vintage and the offer are not on the glass: {text:?}",
    );
    assert!(row_says(&h, "Update"), "no update is offered: {text:?}");
    assert!(
        !row_says(&h, "Resume"),
        "a complete older-generation area is offered a resume: {text:?}",
    );
    // Nothing was taken on the user's behalf.
    assert!(
        h.gui().active_download.is_none(),
        "an older generation auto-started a re-download",
    );
    assert!(
        h.gui().downloaded_area("ok-central").is_some(),
        "an older generation expired the record",
    );
    assert_eq!(dir.names().len(), 7, "an older generation dropped segments");
}

// ---------------------------------------------------------------------------
// Delete
// ---------------------------------------------------------------------------

/// Delete removes the bytes and the record, and a reopen does not bring the
/// area back — reopen is exactly 1:1 over what is actually there.
#[test]
fn delete_removes_the_segments_and_the_record_and_survives_a_reopen() {
    let dir = TempDir::new("delete");
    dir.place_segments("ok-central", 7);
    dir.place_segments("norman", 2);
    let mut h = harness_over(&dir);
    h.gui_mut()
        .record_downloaded_area(area("ok-central", 7, 12, &live_generation()));
    h.gui_mut()
        .record_downloaded_area(area("norman", 2, 10, &live_generation()));
    open_areas_screen(&mut h);
    settle_statuses(&mut h, &["ok-central", "norman"]);
    h.frame_after(1.0 / 60.0);

    let row = h.settings_row("offline.areas").expect("the row drew").rect;
    let name = h
        .text_rect_in(row.expand(4.0), "ok-central")
        .expect("the area is named on the glass");
    // The first Delete under the first area's name is that area's.
    let delete = h
        .painted_text_rects()
        .into_iter()
        .filter(|(rect, text)| text == "Delete" && rect.top() >= name.top())
        .min_by(|a, b| a.0.top().total_cmp(&b.0.top()))
        .expect("a Delete button under the first area")
        .0;
    h.mouse_click(delete.center());
    h.frames_for(20, 1.0 / 60.0);

    assert!(
        h.gui().downloaded_area("ok-central").is_none(),
        "the record survived the delete",
    );
    assert!(
        h.gui().downloaded_area("norman").is_some(),
        "the delete took an area nobody asked about",
    );

    // The bytes go too, through the store's own `remove_area`.
    let start = Instant::now();
    while dir
        .names()
        .iter()
        .any(|name| name.starts_with("ok-central"))
    {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "the segments outlived the delete: {:?}",
            dir.names(),
        );
        h.frame_after(1.0 / 60.0);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        dir.names(),
        vec!["norman.0.pmtiles".to_owned(), "norman.1.pmtiles".to_owned()],
        "the delete took the wrong bytes",
    );

    // Reopen: what persists is the list without it.
    let store = squallar_kv::MemoryKvStore::default();
    h.gui_mut().save_ui_config(&store);
    let mut reopened = crate::Gui::new();
    assert!(reopened.load_ui_config(&store));
    let ids: Vec<&str> = reopened
        .downloaded_areas()
        .iter()
        .map(|held| held.spec.area_id.as_str())
        .collect();
    assert_eq!(ids, ["norman"], "a deleted area came back on reopen");
}

// ---------------------------------------------------------------------------
// Progress
// ---------------------------------------------------------------------------

/// A generous bbox around Monaco, matching the download engine's own suite:
/// the engine enumerates from the bbox and counts what the archive does not
/// hold as absent, so this covers the fixture rather than tracing it.
fn monaco_spec() -> AreaSpec {
    AreaSpec {
        area_id: "monaco".to_owned(),
        west: 7.35,
        south: 43.70,
        east: 7.50,
        north: 43.78,
        max_zoom: 14,
    }
}

/// A cap small enough to cut the 419 KB fixture into several segments, so
/// "N of M parts" has an M worth drawing.
const MONACO_SEGMENT_BYTES: u64 = 120_000;

/// The shouted skip the sibling suites use — straight at stderr, because
/// libtest swallows `eprintln!` on a passing test.
fn skipped(test: &str) {
    use std::io::Write as _;
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(
        stderr,
        "\n\
         ###########################################################################\n\
         ## SKIPPED, NOT PASSED: {test}\n\
         ##   no PMTiles archive at squallar-egui/testdata/monaco.pmtiles\n\
         ##   this test asserted NOTHING. Restore the committed fixture before\n\
         ##   reading this suite as covering the progress block.\n\
         ###########################################################################"
    );
}

/// A source that answers `budget` reads and then never answers again.
///
/// The engine is serial, so once the budget is spent the ledger is **frozen**:
/// no further read starts, nothing increments, and `outcome()` stays `None` —
/// a genuinely in-flight download, held still so the glass and the counters
/// can be compared without a race. Cancellation is the engine's own: dropping
/// it drops the runtime the blocked read is parked on.
struct BudgetedSource {
    inner: crate::basemap_archive::FileRangeSource,
    budget: std::sync::atomic::AtomicI64,
}

impl crate::basemap_archive::RangeSource for BudgetedSource {
    async fn read_range(
        &self,
        offset: u64,
        length: usize,
    ) -> Result<Vec<u8>, crate::basemap_archive::RangeError> {
        use std::sync::atomic::Ordering;
        if self.budget.fetch_sub(1, Ordering::SeqCst) <= 0 {
            loop {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        self.inner.read_range(offset, length).await
    }
}

/// The in-flight block draws the engine's own counters, each figure beside the
/// denominator it is against.
///
/// The reading comes from a **real engine** over the committed Monaco fixture,
/// held mid-run by a spent read budget. The ledger is proved quiescent — two
/// readings a beat apart that agree — before the glass is read, so what is
/// compared is one state, not two.
#[test]
fn the_progress_block_draws_the_engines_own_ledger() {
    const MONACO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/monaco.pmtiles");
    if std::fs::metadata(MONACO).is_err() {
        skipped("the_progress_block_draws_the_engines_own_ledger");
        return;
    }

    let dir = TempDir::new("progress");
    let source = BudgetedSource {
        inner: crate::basemap_archive::FileRangeSource::open(std::path::Path::new(MONACO))
            .expect("the fixture opens"),
        // Enough to open the index, plan, and land some tile bytes; far short
        // of finishing the run.
        budget: std::sync::atomic::AtomicI64::new(8),
    };
    let store = crate::basemap_download::FsSegmentStore::new(dir.0.clone());

    let mut h = harness_over(&dir);
    let engine = crate::basemap_areas::ActiveDownload::start_with_segment_bytes(
        source,
        store,
        monaco_spec(),
        live_generation(),
        h.ctx().clone(),
        MONACO_SEGMENT_BYTES,
    );

    // Wait for the plan, then for the ledger to stop moving.
    let start = Instant::now();
    let progress = loop {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "the fixture download never planned"
        );
        std::thread::sleep(Duration::from_millis(50));
        let first = engine.progress();
        if first.segments_total == 0 {
            continue;
        }
        std::thread::sleep(Duration::from_millis(150));
        let second = engine.progress();
        if first == second {
            break second;
        }
    };
    assert!(
        engine.outcome().is_none(),
        "the run finished; this is no longer the in-flight case",
    );
    // The non-triviality floor: all-zero figures would let a block that
    // printed constants pass.
    assert!(
        progress.segments_total > 1
            && progress.bytes_total.bytes() > 0
            && progress.bytes_done.bytes() > 0,
        "the frozen ledger holds nothing to draw: {progress:?}",
    );

    h.gui_mut().active_download = Some(engine);
    open_areas_screen(&mut h);
    let text = row_text(&h);

    let segments = format!(
        "{} of {} parts stored",
        progress.segments_done, progress.segments_total
    );
    let bytes = format!(
        "{} of {} fetched this run",
        progress.bytes_done.label(),
        progress.bytes_total.label()
    );
    assert!(
        text.iter()
            .any(|drawn| drawn.contains("Downloading monaco")),
        "the in-flight area is not named: {text:?}",
    );
    assert!(
        text.iter().any(|drawn| drawn.contains(&segments)),
        "the glass does not show the ledger's segment figures ({segments:?}): {text:?}",
    );
    assert!(
        text.iter().any(|drawn| drawn.contains(&bytes)),
        "the glass does not show the ledger's byte figures ({bytes:?}): {text:?}",
    );
    // An in-flight area is progress, not an entry: it has no record yet, and
    // nothing draws it as one.
    assert!(
        h.gui().downloaded_area("monaco").is_none(),
        "an unfinished download published a record",
    );
}

/// A finished run publishes its record and the engine is let go of — on the
/// frame, not on the screen, because a download completes whether or not
/// anyone is watching it.
#[test]
fn a_finished_run_publishes_its_record_dated_to_the_archive_it_cut_from() {
    const MONACO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/monaco.pmtiles");
    if std::fs::metadata(MONACO).is_err() {
        skipped("a_finished_run_publishes_its_record_dated_to_the_archive_it_cut_from");
        return;
    }

    let dir = TempDir::new("publish");
    let mut h = harness_over(&dir);
    let engine = crate::basemap_areas::ActiveDownload::start_with_segment_bytes(
        crate::basemap_archive::FileRangeSource::open(std::path::Path::new(MONACO))
            .expect("the fixture opens"),
        crate::basemap_download::FsSegmentStore::new(dir.0.clone()),
        monaco_spec(),
        live_generation(),
        h.ctx().clone(),
        MONACO_SEGMENT_BYTES,
    );
    let start = Instant::now();
    while engine.outcome().is_none() {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "the fixture download never finished"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let segments = match engine.outcome() {
        Some(crate::basemap_download::DownloadOutcome::Complete { segments, .. }) => segments,
        other => panic!("the fixture download did not complete: {other:?}"),
    };

    h.gui_mut().active_download = Some(engine);
    h.frames_for(3, 1.0 / 60.0);

    assert!(
        h.gui().active_download.is_none(),
        "the finished engine is still held",
    );
    let record = h
        .gui()
        .downloaded_area("monaco")
        .expect("a completed run publishes a record");
    assert_eq!(record.segments_expected, segments);
    assert_eq!(
        record.generation,
        live_generation(),
        "the record does not date the archive it was cut from",
    );

    // And the screen reads it back as an area the device holds.
    open_areas_screen(&mut h);
    settle_statuses(&mut h, &["monaco"]);
    h.frame_after(1.0 / 60.0);
    assert!(
        row_says(&h, "Map data August 2026"),
        "the freshly published area states no vintage: {:?}",
        row_text(&h),
    );
    assert!(
        !row_says(&h, "parts"),
        "a complete download draws a part count: {:?}",
        row_text(&h),
    );
}
