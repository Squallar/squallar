//! The Downloaded areas screen, driven like a user: opened through the menu,
//! scrolled to, read off the glass and clicked.
//!
//! **Against a real store.** Every figure these tests read comes from an
//! [`FsSegmentStore`](crate::basemap_download::FsSegmentStore) over a real
//! temporary directory, listing real files and stat-ing their real sizes — so
//! a half-held area's held figure is the store's answer rather than a
//! double's. What is faked is only the *content* of a segment: the listing
//! reads a size, never a byte of the archive inside.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::*;
use crate::input_harness::InputHarness;
use crate::ui::SETTINGS_ROWS;
use crate::ui_download_area::PREPARING_LABEL;
use squallar_units::DataSize;

/// The size of the complete area, chosen so its label ("112 MB") appears
/// nowhere else the screen draws — the negative assertion below depends on
/// that.
const COMPLETE_BYTES: u64 = 112_345_678;
const COMPLETE_SIZE_LABEL: &str = "112 MB";

/// What each placed segment occupies on disk.
///
/// **Real bytes, really written**: the held figure a half-held row draws is a
/// sum of `stat` answers, so a fixture of seven-byte files would let a row
/// that printed a constant "0.0 MB" pass. Sized so a three-segment hold labels
/// as something no other figure on the screen spells.
const SEGMENT_PAYLOAD_BYTES: u64 = 1_200_000;

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
    /// contract spells them.
    ///
    /// The *content* is never read — a listing opens no archive — but the
    /// *size* is, so each file is written at [`SEGMENT_PAYLOAD_BYTES`].
    fn place_segments(&self, area_id: &str, count: u32) {
        let payload = vec![0u8; usize::try_from(SEGMENT_PAYLOAD_BYTES).expect("fits a usize")];
        for seg in 0..count {
            std::fs::write(self.0.join(format!("{area_id}.{seg}.pmtiles")), &payload)
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
        terrain: None,
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
                .and_then(|maintenance| maintenance.fact(id))
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

/// Whether the row drew `needle` as a **whole label**, not as a fragment of a
/// longer one.
///
/// The size slot is one `ui.label`, so its painted string is the whole of what
/// that slot says. "112 MB" is a substring of "3.6 MB of 112 MB", and only
/// equality can tell the finished-area figure apart from the pair that
/// contains it — which is the difference the half-download assertions turn on.
fn row_draws_label(h: &InputHarness, needle: &str) -> bool {
    row_text(h).iter().any(|text| text == needle)
}

/// Every mention of the implementation vocabulary the user ruled out, in
/// whatever the row drew. Case-folded, so a capitalised "Parts" is caught too.
fn parts_vocabulary_on_the_glass(h: &InputHarness) -> Vec<String> {
    row_text(h)
        .into_iter()
        .filter(|text| {
            let folded = text.to_ascii_lowercase();
            folded.contains("part") || folded.contains("segment")
        })
        .collect()
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

/// **Users know what MB means.** The screen states sizes and never explains
/// their base.
///
/// The positive half — a real size is on the glass — is what keeps the
/// negative from passing on an empty screen.
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
        parts_vocabulary_on_the_glass(&h).is_empty(),
        "a complete area draws the segment vocabulary: {:?}",
        parts_vocabulary_on_the_glass(&h),
    );
}

/// **The negative assertion.** A half-held area draws a held-of-asked byte
/// pair *in place of* a size, never beside one: the defect this guards is
/// rendering a half-download as done, and only the absence of the bare size
/// figure rules it out. The record still carries the byte total — it says what
/// the download asked for — so a screen that printed it alone would be reading
/// a real number and making a false claim with it.
///
/// The two assertions are a matched pair over **one slot**: the pair is drawn
/// as a whole label and the finished-area figure is drawn as no label at all.
/// Substring matching cannot express that — "112 MB" is inside
/// "3.6 MB of 112 MB" — which is why both go through
/// [`row_draws_label`].
#[test]
fn a_partial_area_draws_held_and_asked_bytes_and_never_its_size_alone() {
    let dir = TempDir::new("partial");
    dir.place_segments("norman", 3);
    let mut h = harness_over(&dir);
    let record = area("norman", 7, 14, &live_generation());
    assert_eq!(
        record.bytes.label(),
        COMPLETE_SIZE_LABEL,
        "the record carries a size, which is exactly what must not be drawn alone",
    );
    h.gui_mut().record_downloaded_area(record);
    open_areas_screen(&mut h);
    settle_statuses(&mut h, &["norman"]);
    h.frame_after(1.0 / 60.0);

    let held = DataSize::from_bytes(3 * SEGMENT_PAYLOAD_BYTES).label();
    let pair = format!("{held} of {COMPLETE_SIZE_LABEL}");
    let text = row_text(&h);
    assert_ne!(
        held, COMPLETE_SIZE_LABEL,
        "the fixture's held and asked figures label the same, so this test \
         could not tell the two apart",
    );
    assert!(
        row_draws_label(&h, &pair),
        "the half-held area does not draw {pair:?}: {text:?}",
    );
    assert!(
        !row_draws_label(&h, COMPLETE_SIZE_LABEL),
        "the half-held area drew its size {COMPLETE_SIZE_LABEL:?} on its own - \
         a half-download rendered as done: {text:?}",
    );
    assert!(
        parts_vocabulary_on_the_glass(&h).is_empty(),
        "the half-held area draws the segment vocabulary: {:?}",
        parts_vocabulary_on_the_glass(&h),
    );
    // Both halves are the store's answer, not the record's: the record asked
    // for seven segments and the directory holds three of them.
    assert_eq!(
        h.gui()
            .area_maintenance
            .as_ref()
            .and_then(|maintenance| maintenance.fact("norman")),
        Some(crate::basemap_areas::AreaFact {
            status: crate::basemap_download::AreaStatus {
                present: 3,
                expected: 7,
            },
            held: DataSize::from_bytes(3 * SEGMENT_PAYLOAD_BYTES),
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
    // Usable: the bare size is drawn, so it reads as an area the device holds
    // whole rather than as one short of its asked-for bytes.
    assert!(
        row_draws_label(&h, COMPLETE_SIZE_LABEL),
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

/// A cap small enough to cut the 419 KB fixture into several segments, so a
/// run can be caught genuinely mid-download rather than already finished.
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

/// A source that answers only as many reads as the test has granted it.
///
/// The engine is serial, so with the budget spent the ledger is **frozen**: no
/// further read starts, nothing increments, and `outcome()` stays `None` — a
/// genuinely in-flight download, held still so the glass and the counters can
/// be compared without a race. Cancellation is the engine's own: dropping it
/// drops the runtime the blocked read is parked on.
///
/// The budget is a shared cell rather than a constructor argument so a test can
/// **top it up**: that is how the same run is caught first with no plan and
/// then with one, which is the before/after the preparing state is about.
struct BudgetedSource {
    inner: crate::basemap_archive::FileRangeSource,
    budget: std::sync::Arc<std::sync::atomic::AtomicI64>,
}

impl BudgetedSource {
    fn over(path: &str, budget: &std::sync::Arc<std::sync::atomic::AtomicI64>) -> Self {
        Self {
            inner: crate::basemap_archive::FileRangeSource::open(std::path::Path::new(path))
                .expect("the fixture opens"),
            budget: std::sync::Arc::clone(budget),
        }
    }
}

impl crate::basemap_archive::RangeSource for BudgetedSource {
    async fn read_range(
        &self,
        offset: u64,
        length: usize,
    ) -> Result<Vec<u8>, crate::basemap_archive::RangeError> {
        use std::sync::atomic::Ordering;
        // Claim one read, or park until the test grants another. Claimed by
        // compare-exchange rather than `fetch_sub`, so a parked read does not
        // drive the counter arbitrarily negative while it waits.
        loop {
            let left = self.budget.load(Ordering::SeqCst);
            if left > 0
                && self
                    .budget
                    .compare_exchange(left, left - 1, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        self.inner.read_range(offset, length).await
    }
}

/// One read: the archive header, and not a byte past it.
///
/// **Measured, not guessed.** The Monaco fixture is small enough that its root
/// directory covers every tile in the area, so a budget of three already
/// planned the whole run (`segments_total: 4`) and there was no preparing
/// state left to catch. Opening the index is the first thing `drive` does and
/// it is squarely inside the phase this label is for.
const READS_BEFORE_A_PLAN: i64 = 1;

/// Enough to plan and land some tile bytes, and short of finishing the run —
/// the sibling suite's figure, for its reason: the run has to still be in
/// flight when the glass is read, or the frame settles it away and there is no
/// block left to assert about.
const READS_TO_A_PLAN_AND_SOME_BYTES: i64 = 8;

/// Freeze `engine` mid-run and answer with the state it is frozen in.
///
/// "Frozen" is proved rather than assumed: two readings a beat apart that
/// agree. What the glass is then compared against is one state, not two.
fn frozen_mid_run(
    engine: &crate::basemap_areas::ActiveDownload,
    what: &str,
    ready: impl Fn(&crate::basemap_download::DownloadProgress) -> bool,
) -> crate::basemap_download::DownloadProgress {
    let start = Instant::now();
    loop {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "the fixture download never reached {what}: {:?}",
            engine.progress(),
        );
        std::thread::sleep(Duration::from_millis(50));
        let first = engine.progress();
        if !ready(&first) {
            continue;
        }
        std::thread::sleep(Duration::from_millis(150));
        let second = engine.progress();
        if first == second {
            assert!(
                engine.outcome().is_none(),
                "the run finished; this is no longer the in-flight case",
            );
            return second;
        }
    }
}

/// The in-flight block draws a bar over the bytes **and the exact byte figures
/// beside it** — never a percentage in place of the numbers, and never a part
/// count.
///
/// The reading comes from a **real engine** over the committed Monaco fixture,
/// held mid-run by a spent read budget. The bar's own percentage is asserted
/// too, because that is the only thing on the glass that carries the
/// *fraction*: it is what proves the bar is filled from the ledger's bytes
/// rather than parked at some constant.
#[test]
fn the_progress_block_draws_a_byte_bar_and_the_exact_byte_figures() {
    const MONACO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/monaco.pmtiles");
    if std::fs::metadata(MONACO).is_err() {
        skipped("the_progress_block_draws_a_byte_bar_and_the_exact_byte_figures");
        return;
    }

    let dir = TempDir::new("progress");
    // Enough to open the index, plan, and land some tile bytes; far short of
    // finishing the run.
    let budget = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(8));
    let store = crate::basemap_download::FsSegmentStore::new(dir.0.clone());

    let mut h = harness_over(&dir);
    let engine = crate::basemap_areas::ActiveDownload::start_with_segment_bytes(
        BudgetedSource::over(MONACO, &budget),
        None::<(crate::basemap_archive::FileRangeSource, String)>,
        store,
        monaco_spec(),
        live_generation(),
        h.ctx().clone(),
        MONACO_SEGMENT_BYTES,
    );

    let progress = frozen_mid_run(&engine, "a plan with bytes on it", |p| {
        p.denominator_known() && p.bytes_done.bytes() > 0
    });
    // The non-triviality floor: all-zero figures would let a block that
    // printed constants pass, and a full bar would not distinguish a fraction
    // from a hard-coded one.
    assert!(
        progress.bytes_total.bytes() > 0
            && progress.bytes_done.bytes() > 0
            && progress.bytes_done < progress.bytes_total,
        "the frozen ledger holds no partial byte figure to draw: {progress:?}",
    );

    h.gui_mut().active_download = Some(engine);
    open_areas_screen(&mut h);
    let text = row_text(&h);

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
    // The exact numbers, both of them, transferred and total.
    assert!(
        text.iter().any(|drawn| drawn.contains(&bytes)),
        "the glass does not show the ledger's byte figures ({bytes:?}): {text:?}",
    );
    // And the bar, filled from those same bytes. **Computed here from the two
    // byte counts, never from `byte_fraction`**: an expectation taken from the
    // function under test would agree with any constant that function
    // returned. `ProgressBar` prints its percentage truncated, which is what
    // the cast reproduces.
    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "reproducing egui's own `(progress * 100.0) as usize` over \
                  the ledger's own two byte counts"
    )]
    let percentage = {
        let fraction = progress.bytes_done.bytes() as f64 / progress.bytes_total.bytes() as f64;
        format!("{}%", (fraction * 100.0) as usize)
    };
    assert_ne!(
        percentage, "0%",
        "the frozen ledger rounds to an empty bar, so a bar filled from a \
         constant zero would pass this",
    );
    assert!(
        text.iter().any(|drawn| drawn == &percentage),
        "the bar is not filled from the ledger's bytes - expected {percentage:?} \
         for {} of {}: {text:?}",
        progress.bytes_done.bytes(),
        progress.bytes_total.bytes(),
    );
    assert!(
        !row_says(&h, PREPARING_LABEL),
        "a run with a denominator still says it is preparing: {text:?}",
    );
    assert!(
        parts_vocabulary_on_the_glass(&h).is_empty(),
        "the in-flight block draws the segment vocabulary: {:?}",
        parts_vocabulary_on_the_glass(&h),
    );
    // An in-flight area is progress, not an entry: it has no record yet, and
    // nothing draws it as one.
    assert!(
        h.gui().downloaded_area("monaco").is_none(),
        "an unfinished download published a record",
    );
}

/// Before the plan exists there is no byte denominator, and the block says so
/// **instead of** drawing a bar at zero.
///
/// This is the reported defect: the cut runs for minutes over a real area, and
/// a bar pinned at 0% through it is indistinguishable from a hang. Both halves
/// are asserted on one run — the preparing state while the plan is out, and
/// its replacement by the real bar once the denominator lands — so a block
/// that simply always said "preparing" would fail the second half.
#[test]
fn the_block_says_it_is_preparing_until_a_byte_denominator_exists() {
    const MONACO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/monaco.pmtiles");
    if std::fs::metadata(MONACO).is_err() {
        skipped("the_block_says_it_is_preparing_until_a_byte_denominator_exists");
        return;
    }

    let dir = TempDir::new("preparing");
    let budget = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(READS_BEFORE_A_PLAN));
    let mut h = harness_over(&dir);
    let engine = crate::basemap_areas::ActiveDownload::start_with_segment_bytes(
        BudgetedSource::over(MONACO, &budget),
        None::<(crate::basemap_archive::FileRangeSource, String)>,
        crate::basemap_download::FsSegmentStore::new(dir.0.clone()),
        monaco_spec(),
        live_generation(),
        h.ctx().clone(),
        MONACO_SEGMENT_BYTES,
    );

    let planning = frozen_mid_run(&engine, "a stalled plan", |p| !p.denominator_known());
    assert_eq!(
        planning.byte_fraction(),
        None,
        "a run with no plan answered with a fraction",
    );

    h.gui_mut().active_download = Some(engine);
    open_areas_screen(&mut h);
    let text = row_text(&h);
    assert!(
        text.iter()
            .any(|drawn| drawn.contains("Downloading monaco")),
        "the in-flight area is not named while it prepares: {text:?}",
    );
    assert!(
        row_says(&h, PREPARING_LABEL),
        "the planning phase is mute - it drew {text:?}",
    );
    assert!(
        !text.iter().any(|drawn| drawn.ends_with('%')),
        "a bar was drawn over a denominator that does not exist yet: {text:?}",
    );

    // Now let it plan. The preparing state is replaced by the real bar and the
    // real figures — which is what keeps the assertion above from passing on a
    // block that never leaves this state.
    budget.store(
        READS_TO_A_PLAN_AND_SOME_BYTES,
        std::sync::atomic::Ordering::SeqCst,
    );
    let start = Instant::now();
    loop {
        let progress = h
            .gui()
            .active_download
            .as_ref()
            .expect("the run is budgeted to stall, so it is still in flight")
            .progress();
        if progress.denominator_known() && progress.bytes_done.bytes() > 0 {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "the fixture download never planned once its budget was topped up: {progress:?}",
        );
        h.frame_after(1.0 / 60.0);
        std::thread::sleep(Duration::from_millis(5));
    }
    h.frame_after(1.0 / 60.0);

    let text = row_text(&h);
    assert!(
        !row_says(&h, PREPARING_LABEL),
        "the preparing state outlived the plan: {text:?}",
    );
    assert!(
        text.iter()
            .any(|drawn| drawn.contains(" of ") && drawn.contains(" MB")),
        "the real byte figures never replaced the preparing state: {text:?}",
    );
    assert!(
        parts_vocabulary_on_the_glass(&h).is_empty(),
        "the in-flight block draws the segment vocabulary: {:?}",
        parts_vocabulary_on_the_glass(&h),
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
        None::<(crate::basemap_archive::FileRangeSource, String)>,
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
        parts_vocabulary_on_the_glass(&h).is_empty(),
        "a complete download draws the segment vocabulary: {:?}",
        parts_vocabulary_on_the_glass(&h),
    );
}

// ---------------------------------------------------------------------------
// The hillshade half on the glass
// ---------------------------------------------------------------------------

/// The terrain half's own cut and byte figure, distinct from the basemap's so
/// no assertion below can pass on the wrong one.
const TERRAIN_SEGMENTS: u32 = 3;
const TERRAIN_BYTES: u64 = 21_000_111;

/// The pair a whole two-archive area's byte figure must label as — the record
/// carries both halves, so the size on the glass is both halves.
fn both_halves_label() -> String {
    DataSize::from_bytes(COMPLETE_BYTES + TERRAIN_BYTES).label()
}

/// [`area`] with a terrain half.
fn area_with_terrain(
    area_id: &str,
    segments: u32,
    max_zoom: u8,
    generation: &str,
) -> DownloadedArea {
    let mut record = area(area_id, segments, max_zoom, generation);
    record.bytes = DataSize::from_bytes(COMPLETE_BYTES + TERRAIN_BYTES);
    record.terrain = Some(crate::basemap_download::TerrainHold {
        segments_expected: TERRAIN_SEGMENTS,
        bytes: DataSize::from_bytes(TERRAIN_BYTES),
        generation: generation.to_owned(),
    });
    record
}

impl TempDir {
    /// Publish `count` **terrain** segments of `area_id`, as the store's own
    /// naming contract spells them — the infix is the only difference, which
    /// is the point.
    fn place_terrain_segments(&self, area_id: &str, count: u32) {
        let payload = vec![0u8; usize::try_from(SEGMENT_PAYLOAD_BYTES).expect("fits a usize")];
        for seg in 0..count {
            std::fs::write(
                self.0.join(format!("{area_id}.{seg}.terrain.pmtiles")),
                &payload,
            )
            .expect("a terrain segment should be writable");
        }
    }
}

/// A whole two-archive area draws **one** size — both halves — and **one**
/// extra line saying it holds the hillshade. Never a second entry, never a
/// second byte figure, and never the segment vocabulary.
#[test]
fn an_area_that_holds_terrain_says_so_in_one_line_and_one_figure() {
    let dir = TempDir::new("with-terrain");
    dir.place_segments("ok-central", 7);
    dir.place_terrain_segments("ok-central", TERRAIN_SEGMENTS);
    let mut h = harness_over(&dir);
    h.gui_mut()
        .record_downloaded_area(area_with_terrain("ok-central", 7, 12, &live_generation()));
    open_areas_screen(&mut h);
    settle_statuses(&mut h, &["ok-central"]);
    h.frame_after(1.0 / 60.0);

    let text = row_text(&h);
    assert!(
        row_says(&h, super::TERRAIN_HELD_NOTE),
        "an area holding the hillshade does not say so: {text:?}",
    );
    assert!(
        row_says(&h, "Towns and main roads"),
        "the terrain note replaced the depth rather than joining it: {text:?}",
    );
    // One area, one entry: the id is drawn once however many archives it holds.
    assert_eq!(
        text.iter()
            .filter(|drawn| drawn.contains("ok-central"))
            .count(),
        1,
        "the two-archive area drew more than one entry: {text:?}",
    );
    assert_eq!(
        text.iter()
            .filter(|drawn| drawn.contains(super::TERRAIN_HELD_NOTE))
            .count(),
        1,
        "the terrain fact was drawn more than once: {text:?}",
    );
    assert!(
        parts_vocabulary_on_the_glass(&h).is_empty(),
        "a two-archive area draws the segment vocabulary: {:?}",
        parts_vocabulary_on_the_glass(&h),
    );

    // The byte figure is both halves, from the store's own listing of both.
    let held = DataSize::from_bytes(u64::from(7 + TERRAIN_SEGMENTS) * SEGMENT_PAYLOAD_BYTES);
    let fact = h
        .gui()
        .area_maintenance
        .as_ref()
        .and_then(|maintenance| maintenance.fact("ok-central"))
        .expect("the store answered");
    assert_eq!(
        fact.held, held,
        "the held figure counts one archive's segments, not the area's",
    );
    assert_eq!(
        fact.status,
        crate::basemap_download::AreaStatus {
            present: 7 + TERRAIN_SEGMENTS,
            expected: 7 + TERRAIN_SEGMENTS,
        },
        "the status is not both halves against both cuts",
    );
    assert!(
        row_draws_label(&h, &both_halves_label()),
        "a whole two-archive area does not draw its combined size {:?}: {text:?}",
        both_halves_label(),
    );
}

/// **An area whose hillshade went missing is a half-held area**, drawn as the
/// held-of-asked pair — never as a finished area with a note.
///
/// This is the silent-partial-success shape the second archive introduces: the
/// base map is all there, so a reconcile that only asked about the base map
/// would draw this area as done while the map showed no shading.
#[test]
fn an_area_missing_only_its_hillshade_never_renders_as_done() {
    let dir = TempDir::new("lost-terrain");
    dir.place_segments("ok-central", 7);
    dir.place_terrain_segments("ok-central", 1);
    let mut h = harness_over(&dir);
    h.gui_mut()
        .record_downloaded_area(area_with_terrain("ok-central", 7, 12, &live_generation()));
    open_areas_screen(&mut h);
    settle_statuses(&mut h, &["ok-central"]);
    h.frame_after(1.0 / 60.0);

    let text = row_text(&h);
    let held = DataSize::from_bytes(8 * SEGMENT_PAYLOAD_BYTES).label();
    let pair = format!("{held} of {}", both_halves_label());
    assert_ne!(
        held,
        both_halves_label(),
        "the fixture's held and asked figures label the same, so this test could \
         not tell the two apart",
    );
    assert!(
        row_draws_label(&h, &pair),
        "the area missing its hillshade does not draw {pair:?}: {text:?}",
    );
    assert!(
        !row_draws_label(&h, &both_halves_label()),
        "an area whose hillshade is gone drew its size alone - a half-download \
         rendered as done: {text:?}",
    );
    assert!(
        parts_vocabulary_on_the_glass(&h).is_empty(),
        "the half-held area draws the segment vocabulary: {:?}",
        parts_vocabulary_on_the_glass(&h),
    );
    assert!(
        row_says(&h, "Resume"),
        "an area short of a half offers no way to complete it: {text:?}",
    );
}

/// A **basemap-only** area still draws exactly as it did: no terrain line, no
/// terrain figure, and its own size. The device that downloaded before terrain
/// existed sees no change.
#[test]
fn a_basemap_only_area_draws_no_terrain_fact() {
    let dir = TempDir::new("no-terrain");
    dir.place_segments("ok-central", 7);
    // A terrain artifact belonging to a DIFFERENT area, in the same directory:
    // the listing must not sweep it into this record's figures.
    dir.place_terrain_segments("norman", 4);
    let mut h = harness_over(&dir);
    h.gui_mut()
        .record_downloaded_area(area("ok-central", 7, 12, &live_generation()));
    open_areas_screen(&mut h);
    settle_statuses(&mut h, &["ok-central"]);
    h.frame_after(1.0 / 60.0);

    let text = row_text(&h);
    assert!(
        !row_says(&h, super::TERRAIN_HELD_NOTE),
        "a basemap-only area claims a hillshade it does not hold: {text:?}",
    );
    assert!(
        row_draws_label(&h, COMPLETE_SIZE_LABEL),
        "a basemap-only area does not draw its own size: {text:?}",
    );
    assert_eq!(
        h.gui()
            .area_maintenance
            .as_ref()
            .and_then(|maintenance| maintenance.fact("ok-central"))
            .expect("the store answered")
            .held,
        DataSize::from_bytes(7 * SEGMENT_PAYLOAD_BYTES),
        "another area's terrain segments were counted into this one",
    );
}
