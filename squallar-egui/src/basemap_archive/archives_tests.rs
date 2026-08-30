//! Tests for [`super::BasemapArchives`] — the local-first composition.
//!
//! **Every assertion here is a read count or a byte comparison**, because the
//! feature is about what is *not* fetched. "The tile arrived" is true whichever
//! source answered, so it is never the evidence; the evidence is that the live
//! source was untouched while it happened, with a control proving the same live
//! source does serve that tile when nothing is in front of it.
//!
//! The local sub-archives are **built by the real download engine** from the
//! committed Monaco fixture rather than hand-assembled, so what is composed
//! here is the artifact the feature actually writes — a segment whose header,
//! bbox and zoom band are the writer's, not a test's idea of them.
//!
//! The area every test downloads is one tile's **own centre point**, which
//! `basemap_download::area_tiles` turns into exactly the ancestor chain of that
//! tile, one tile per zoom. That is what makes "a tile outside the area" a
//! neighbour rather than a distant guess: the tile next door is in the fixture
//! and provably not in the download.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use egui::Context;

use super::tests::{CountingSource, archive_path, block_on, no_archive_banner};
use super::{BasemapArchives, FileRangeSource};
use crate::basemap_download::{
    AreaArchive, AreaSpec, BasemapDownload, DownloadOutcome, FsSegmentStore, OfflineSegments,
};
use crate::pmt_index::{PmtIndex, tile_id_to_zxy};

/// A segment cap small enough to cut the fixture's own tiles into more than
/// one segment where a test wants several.
const SMALL_SEGMENT_BYTES: u64 = 8_000;

/// Long enough that a loaded box does not red-gate a local file copy.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30);

/// A per-test directory under the OS temp dir, removed on drop.
///
/// Named with the test as well as the pid because a filtered run shares a
/// process and two tests sharing a download directory would compose each
/// other's segments.
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(test: &str) -> Self {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "squallar-basemap-compose-{}-{test}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The fixture, or `None` with the sibling suite's shouted skip.
fn fixture() -> Option<std::path::PathBuf> {
    let path = archive_path();
    path.is_file().then_some(path)
}

/// A source over the fixture file.
fn fixture_source(path: &std::path::Path) -> FileRangeSource {
    FileRangeSource::open(path).expect("the fixture opens")
}

/// The `z/x/y` of every tile the fixture holds at its deepest zoom, in id
/// order.
///
/// Read out of the archive rather than assumed, so the tiles these tests
/// compose over are ones it demonstrably has — a hardcoded coordinate that
/// happened to be absent would make every read-count assertion trivially true.
fn deepest_tiles(path: &std::path::Path) -> Vec<(u8, u32, u32)> {
    let index =
        block_on(PmtIndex::open(fixture_source(path))).expect("the fixture is a v3 archive");
    let deepest = index.header().max_zoom;
    let entries = block_on(index.tile_entries()).expect("the fixture's directories walk");

    let mut tiles: Vec<(u8, u32, u32)> = entries
        .iter()
        .flat_map(|entry| entry.tile_id..entry.tile_id + entry.run_length)
        .filter_map(tile_id_to_zxy)
        .filter(|(z, _, _)| *z == deepest)
        .collect();
    tiles.sort_unstable();
    tiles.dedup();
    tiles
}

/// The centre of tile `z/x/y` in degrees, checked to land back inside it.
///
/// The round-trip assertion is what stops this being a silent source of
/// off-by-one areas: if the two conversions disagreed the download would cover
/// a different tile than the one every assertion below names, and every test
/// would still pass on a different reading of the word "inside".
fn tile_centre(z: u8, x: u32, y: u32) -> (f64, f64) {
    let side = f64::from(1u32 << z);
    let lon = (f64::from(x) + 0.5) / side * 360.0 - 180.0;
    let lat = (std::f64::consts::PI * (1.0 - 2.0 * (f64::from(y) + 0.5) / side))
        .sinh()
        .atan()
        .to_degrees();
    assert_eq!(
        (
            squallar_geo::lon_to_tile_x(lon, z),
            squallar_geo::lat_to_tile_y(lat, z)
        ),
        (x, y),
        "the centre of {z}/{x}/{y} must fall back inside it"
    );
    (lon, lat)
}

/// The area covering exactly `z/x/y`'s ancestor chain, `z0..=max_zoom`.
fn area_over(area_id: &str, z: u8, x: u32, y: u32, max_zoom: u8) -> AreaSpec {
    let (lon, lat) = tile_centre(z, x, y);
    AreaSpec {
        area_id: area_id.to_owned(),
        west: lon,
        south: lat,
        east: lon,
        north: lat,
        max_zoom,
    }
}

/// Run the real download engine over the fixture into `dir`, and assert it
/// finished — a partial download would leave a different set of segments than
/// the test is reasoning about.
fn download(path: &std::path::Path, dir: &std::path::Path, area: AreaSpec, segment_bytes: u64) {
    let engine = BasemapDownload::with_segment_bytes(
        fixture_source(path),
        FsSegmentStore::new(dir.to_path_buf()),
        area,
        Context::default(),
        segment_bytes,
    );
    let start = Instant::now();
    let outcome = loop {
        if let Some(outcome) = engine.outcome() {
            break outcome;
        }
        assert!(
            start.elapsed() < DOWNLOAD_TIMEOUT,
            "the download did not finish in {DOWNLOAD_TIMEOUT:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(
        matches!(outcome, DownloadOutcome::Complete { .. }),
        "the fixture download must complete, got {outcome:?}"
    );
}

/// The composition over a counted live source, with every segment in `dir`
/// attached through the store's own listing — the production route.
///
/// Returns the composition and the live read counter, **zeroed after open**:
/// opening reads the header and the root directory, and counting those would
/// make "zero network reads for a local tile" a claim about the wrong reads.
fn composed(
    path: &std::path::Path,
    dir: &std::path::Path,
) -> (
    BasemapArchives<CountingSource<FileRangeSource>, FileRangeSource>,
    Arc<AtomicUsize>,
) {
    let (live, reads) = CountingSource::new(fixture_source(path));
    let mut archives = block_on(BasemapArchives::open(live)).expect("the fixture opens");
    let store = FsSegmentStore::new(dir.to_path_buf());
    for (label, source) in block_on(store.open_all(AreaArchive::Basemap)) {
        block_on(archives.attach_offline(label, source));
    }
    reads.store(0, Ordering::Relaxed);
    (archives, reads)
}

/// The live archive alone, counted — the control every "it came from local"
/// assertion needs, because it is what proves the live archive holds the tile
/// and was still not asked.
fn live_only(
    path: &std::path::Path,
) -> (
    BasemapArchives<CountingSource<FileRangeSource>, FileRangeSource>,
    Arc<AtomicUsize>,
) {
    let (live, reads) = CountingSource::new(fixture_source(path));
    let archives = block_on(BasemapArchives::open(live)).expect("the fixture opens");
    reads.store(0, Ordering::Relaxed);
    (archives, reads)
}

// ---------------------------------------------------------------------------
// The walk
// ---------------------------------------------------------------------------

#[test]
fn a_tile_inside_a_downloaded_area_costs_the_network_nothing() {
    const TEST: &str = "a_tile_inside_a_downloaded_area_costs_the_network_nothing";
    let Some(path) = fixture() else {
        return no_archive_banner(TEST, &archive_path());
    };
    let tiles = deepest_tiles(&path);
    let (z, x, y) = tiles[tiles.len() / 2];

    // The control first: the live archive alone does serve this tile, and
    // serving it costs reads. Without this the zero below could be a tile
    // nothing holds.
    let (only_live, live_reads) = live_only(&path);
    let from_live = block_on(only_live.tile(z, x, y)).expect("the fixture reads");
    assert!(
        from_live.is_present(),
        "the control must be a tile the live archive holds"
    );
    let control_reads = live_reads.load(Ordering::Relaxed);
    assert!(
        control_reads > 0,
        "reading {z}/{x}/{y} from the network must cost reads, or this suite measures nothing"
    );

    let dir = TempDir::new(TEST);
    download(
        &path,
        &dir.0,
        area_over("inside", z, x, y, z),
        SMALL_SEGMENT_BYTES,
    );
    let (archives, reads) = composed(&path, &dir.0);
    assert!(
        archives.offline_count() > 0,
        "the store's listing must find the segments the engine published"
    );

    let served = block_on(archives.tile(z, x, y)).expect("the composition reads");
    assert_eq!(
        served.bytes(),
        from_live.bytes(),
        "a locally served tile must be byte-identical to the live one"
    );
    assert_eq!(
        reads.load(Ordering::Relaxed),
        0,
        "a tile a downloaded area holds must cost the network nothing, and cost {control_reads} \
         without one"
    );
}

#[test]
fn a_tile_outside_the_downloaded_area_falls_through_to_the_network() {
    const TEST: &str = "a_tile_outside_the_downloaded_area_falls_through_to_the_network";
    let Some(path) = fixture() else {
        return no_archive_banner(TEST, &archive_path());
    };
    let tiles = deepest_tiles(&path);
    assert!(
        tiles.len() >= 2,
        "the fixture must hold at least two deep tiles for an inside/outside pair"
    );
    let (z, x, y) = tiles[0];
    let (oz, ox, oy) = *tiles.last().expect("checked non-empty");

    let dir = TempDir::new(TEST);
    download(
        &path,
        &dir.0,
        area_over("outside", z, x, y, z),
        SMALL_SEGMENT_BYTES,
    );
    let (archives, reads) = composed(&path, &dir.0);

    let served = block_on(archives.tile(oz, ox, oy)).expect("the composition reads");
    assert!(
        served.is_present(),
        "a tile outside the download must still arrive"
    );
    assert!(
        reads.load(Ordering::Relaxed) > 0,
        "a tile outside the download must be fetched from the live archive"
    );
}

#[test]
fn a_viewport_straddling_the_boundary_serves_each_tile_from_its_own_source() {
    const TEST: &str = "a_viewport_straddling_the_boundary_serves_each_tile_from_its_own_source";
    let Some(path) = fixture() else {
        return no_archive_banner(TEST, &archive_path());
    };
    let tiles = deepest_tiles(&path);
    assert!(
        tiles.len() >= 4,
        "a straddle needs a viewport of several tiles"
    );
    let (z, x, y) = tiles[0];

    let dir = TempDir::new(TEST);
    download(
        &path,
        &dir.0,
        area_over("straddle", z, x, y, z),
        SMALL_SEGMENT_BYTES,
    );
    let (archives, reads) = composed(&path, &dir.0);

    // Walk the whole "viewport" one tile at a time, watching the live counter
    // move only on the tiles the download does not hold. Nothing anywhere
    // asks whether the viewport is covered; coverage is per tile and is never
    // a number this code computes.
    let mut local = 0usize;
    let mut remote = 0usize;
    for &(tz, tx, ty) in &tiles {
        let before = reads.load(Ordering::Relaxed);
        let served = block_on(archives.tile(tz, tx, ty)).expect("no tile in a straddle errors");
        assert!(served.is_present(), "every fixture tile must arrive");
        if reads.load(Ordering::Relaxed) == before {
            local += 1;
        } else {
            remote += 1;
        }
    }

    assert_eq!(local, 1, "exactly the downloaded tile is served locally");
    assert_eq!(
        remote,
        tiles.len() - 1,
        "every other tile of the viewport comes from the live archive"
    );
}

#[test]
fn a_local_hit_wins_even_though_the_live_archive_holds_the_tile_too() {
    const TEST: &str = "a_local_hit_wins_even_though_the_live_archive_holds_the_tile_too";
    let Some(path) = fixture() else {
        return no_archive_banner(TEST, &archive_path());
    };
    let tiles = deepest_tiles(&path);
    let (z, x, y) = tiles[0];

    let dir = TempDir::new(TEST);
    download(
        &path,
        &dir.0,
        area_over("ordering", z, x, y, z),
        SMALL_SEGMENT_BYTES,
    );

    // Both archives are the same fixture, so both hold the tile and the bytes
    // cannot say which answered. What says it is the counter: the live source
    // is a source that would have answered, and was not asked.
    let (archives, reads) = composed(&path, &dir.0);
    assert!(
        block_on(archives.tile(z, x, y))
            .expect("the composition reads")
            .is_present()
    );
    assert_eq!(
        reads.load(Ordering::Relaxed),
        0,
        "local must win over a live archive that holds the same tile"
    );
}

#[test]
fn over_zoom_past_a_downloaded_area_does_not_move_the_render_ceiling() {
    const TEST: &str = "over_zoom_past_a_downloaded_area_does_not_move_the_render_ceiling";
    let Some(path) = fixture() else {
        return no_archive_banner(TEST, &archive_path());
    };
    let tiles = deepest_tiles(&path);
    let (z, x, y) = tiles[0];
    assert!(
        z >= 2,
        "the fixture must be deep enough to download a shallower slice"
    );

    // A download that stops two zooms short of the archive's own ceiling.
    let dir = TempDir::new(TEST);
    let shallow = z - 2;
    download(
        &path,
        &dir.0,
        area_over("shallow", z, x, y, shallow),
        SMALL_SEGMENT_BYTES,
    );
    let (archives, reads) = composed(&path, &dir.0);
    assert!(archives.offline_count() > 0, "the shallow area must attach");

    assert_eq!(
        archives.max_zoom(),
        z,
        "a shallower downloaded area must not lower the render ceiling"
    );

    // Past the local ceiling the tile simply comes from the live archive, at
    // the same coordinate the ceiling always allowed. There is no second
    // over-zoom rule to exercise, which is the point being pinned: whatever
    // the deep tile does, it does because of the live archive.
    let served = block_on(archives.tile(z, x, y)).expect("the composition reads");
    assert!(served.is_present());
    assert!(
        reads.load(Ordering::Relaxed) > 0,
        "a zoom past the downloaded area's maximum is the live archive's to answer"
    );
}

#[test]
fn a_truncated_local_segment_is_skipped_and_the_tile_still_arrives() {
    const TEST: &str = "a_truncated_local_segment_is_skipped_and_the_tile_still_arrives";
    let Some(path) = fixture() else {
        return no_archive_banner(TEST, &archive_path());
    };
    let tiles = deepest_tiles(&path);
    let (z, x, y) = tiles[0];

    let dir = TempDir::new(TEST);
    download(
        &path,
        &dir.0,
        area_over("corrupt", z, x, y, z),
        SMALL_SEGMENT_BYTES,
    );

    // Truncate every published segment to a stub that is not a v3 header.
    let mut truncated = 0usize;
    for entry in std::fs::read_dir(&dir.0)
        .expect("the store directory lists")
        .flatten()
    {
        if entry.path().extension().is_some_and(|ext| ext == "pmtiles") {
            std::fs::write(entry.path(), b"PMTiles").expect("the segment truncates");
            truncated += 1;
        }
    }
    assert!(truncated > 0, "there must be a segment to corrupt");

    let (archives, reads) = composed(&path, &dir.0);
    assert_eq!(
        archives.offline_count(),
        0,
        "a segment that will not open must be skipped, not held"
    );

    let served = block_on(archives.tile(z, x, y)).expect("a corrupt local segment is not an error");
    assert!(
        served.is_present(),
        "the tile must degrade to the network rather than fail"
    );
    assert!(
        reads.load(Ordering::Relaxed) > 0,
        "and the network is what served it"
    );
}

#[test]
fn walking_many_local_segments_costs_no_range_reads() {
    const TEST: &str = "walking_many_local_segments_costs_no_range_reads";
    let Some(path) = fixture() else {
        return no_archive_banner(TEST, &archive_path());
    };
    let tiles = deepest_tiles(&path);
    let (z, x, y) = tiles[0];

    let dir = TempDir::new(TEST);
    // Four separate areas, each its own set of segments, so the walk has
    // several archives to reject rather than one.
    for (n, &(tz, tx, ty)) in tiles.iter().take(4).enumerate() {
        download(
            &path,
            &dir.0,
            area_over(&format!("area{n}"), tz, tx, ty, tz),
            SMALL_SEGMENT_BYTES,
        );
    }

    // Attach each segment over its own counter, so the walk's cost is visible
    // per archive rather than in aggregate.
    let (live, _live_reads) = CountingSource::new(fixture_source(&path));
    let mut archives = block_on(BasemapArchives::open(live)).expect("the fixture opens");
    let store = FsSegmentStore::new(dir.0.clone());
    let mut counters = Vec::new();
    for (label, source) in block_on(store.open_all(AreaArchive::Basemap)) {
        let (counted, reads) = CountingSource::new(source);
        assert!(block_on(archives.attach_offline(label, counted)));
        counters.push(reads);
    }
    assert!(
        counters.len() >= 4,
        "four areas must publish at least four segments, got {}",
        counters.len()
    );
    for reads in &counters {
        reads.store(0, Ordering::Relaxed);
    }

    let served = block_on(archives.tile(z, x, y)).expect("the composition reads");
    assert!(served.is_present());

    // One archive answers, and it pays for the body it hands back. Every
    // other archive in the walk is decided from its own header and root
    // directory, both already in memory: not one range read between them.
    let paying = counters
        .iter()
        .filter(|reads| reads.load(Ordering::Relaxed) > 0)
        .count();
    assert_eq!(
        paying,
        1,
        "exactly the archive that held the tile may read; {} archives were walked",
        counters.len()
    );
}

// ---------------------------------------------------------------------------
// The store's read-back
// ---------------------------------------------------------------------------

#[test]
fn the_store_lists_published_segments_and_never_a_part_file() {
    const TEST: &str = "the_store_lists_published_segments_and_never_a_part_file";
    let Some(path) = fixture() else {
        return no_archive_banner(TEST, &archive_path());
    };
    let tiles = deepest_tiles(&path);
    let (z, x, y) = tiles[0];

    let dir = TempDir::new(TEST);
    download(
        &path,
        &dir.0,
        area_over("listing", z, x, y, z),
        SMALL_SEGMENT_BYTES,
    );
    // A half-written artifact beside the finished ones. It is a complete
    // archive by content and would open cleanly, so only the naming rule can
    // keep it out — which is exactly the rule being pinned.
    std::fs::copy(&path, dir.0.join("listing.99.part")).expect("the part file writes");

    let store = FsSegmentStore::new(dir.0.clone());
    let listed = block_on(store.open_all(AreaArchive::Basemap));
    assert!(!listed.is_empty(), "the published segments must be listed");
    assert!(
        listed.iter().all(|(label, _)| label.ends_with(".pmtiles")),
        "a .part is not a segment: {:?}",
        listed.iter().map(|(label, _)| label).collect::<Vec<_>>()
    );
}

#[test]
fn a_store_directory_that_does_not_exist_lists_nothing_rather_than_failing() {
    let dir =
        TempDir::new("a_store_directory_that_does_not_exist_lists_nothing_rather_than_failing");
    let store = FsSegmentStore::new(dir.0.join("never-created"));
    assert!(block_on(store.open_all(AreaArchive::Basemap)).is_empty());
}

#[test]
fn a_hand_placed_archive_is_composed_like_a_downloaded_one() {
    const TEST: &str = "a_hand_placed_archive_is_composed_like_a_downloaded_one";
    let Some(path) = fixture() else {
        return no_archive_banner(TEST, &archive_path());
    };
    let tiles = deepest_tiles(&path);
    let (z, x, y) = tiles[0];

    // The whole fixture, dropped into the basemap directory by hand under a
    // name no download ever wrote. The directory listing is the authority on
    // what is here, so this is a segment.
    let dir = TempDir::new(TEST);
    std::fs::create_dir_all(&dir.0).expect("the store directory is created");
    std::fs::copy(&path, dir.0.join("hand-placed.pmtiles")).expect("the archive copies");

    let (archives, reads) = composed(&path, &dir.0);
    assert_eq!(archives.offline_count(), 1);
    assert!(
        block_on(archives.tile(z, x, y))
            .expect("the composition reads")
            .is_present()
    );
    assert_eq!(
        reads.load(Ordering::Relaxed),
        0,
        "a hand-placed archive serves the tiles it holds"
    );
}

#[test]
fn a_segment_whose_bodies_are_not_the_archives_kind_is_refused() {
    const TEST: &str = "a_segment_whose_bodies_are_not_the_archives_kind_is_refused";
    let Some(path) = fixture() else {
        return no_archive_banner(TEST, &archive_path());
    };

    // The decoder every tile the composition serves goes through is chosen
    // from the LIVE header, once, at the top of the IO loop. A local archive
    // of another kind would have its bodies handed to that decoder, so it is
    // refused rather than tried — this is the hand-placement hazard, and the
    // file below is a perfectly valid archive that is simply the wrong kind.
    let dir = TempDir::new(TEST);
    std::fs::create_dir_all(&dir.0).expect("the store directory is created");
    let raster = dir.0.join("raster.pmtiles");
    std::fs::write(&raster, one_tile_archive(pmtiles::TileType::Png))
        .expect("the raster archive writes");

    let (mut archives, _) = live_only(&path);
    assert_eq!(
        archives.tile_type(),
        pmtiles::TileType::Mvt,
        "the fixture is the vector archive this refusal is measured against"
    );

    // The control: the same call with a same-kind archive is taken, so the
    // refusal below is about the kind and not about the plumbing.
    assert!(
        block_on(archives.attach_offline("same-kind".to_owned(), fixture_source(&path))),
        "a segment of the archive's own kind attaches"
    );
    assert!(
        !block_on(archives.attach_offline(
            "raster".to_owned(),
            FileRangeSource::open(&raster).expect("the raster archive opens"),
        )),
        "a segment of another kind must be refused"
    );
    assert_eq!(archives.offline_count(), 1, "and refusing must not hold it");
}

/// A one-tile archive of `kind`, written with the same writer the download
/// engine uses — a real archive, so what refuses it can only be the kind.
fn one_tile_archive(kind: pmtiles::TileType) -> Vec<u8> {
    use std::io::Cursor;

    let mut sink = Cursor::new(Vec::new());
    let mut writer = pmtiles::PmTilesWriter::new(kind)
        .tile_compression(pmtiles::Compression::None)
        .min_zoom(0)
        .max_zoom(0)
        .bounds(-180.0, -85.0, 180.0, 85.0)
        .create(&mut sink)
        .expect("the writer opens");
    writer
        .add_tile(
            pmtiles::TileCoord::new(0, 0, 0).expect("0/0/0 is a tile"),
            b"not really a png".as_slice(),
        )
        .expect("the tile writes");
    writer.finalize().expect("the archive finalizes");
    sink.into_inner()
}

// ---------------------------------------------------------------------------
// The rejection rule itself
// ---------------------------------------------------------------------------

/// The bbox and zoom band a downloaded area over one tile declares.
fn coverage_over(z: u8, x: u32, y: u32, min_zoom: u8, max_zoom: u8) -> super::Coverage {
    let (lon, lat) = tile_centre(z, x, y);
    super::Coverage {
        min_zoom,
        max_zoom,
        west: lon,
        south: lat,
        east: lon,
        north: lat,
    }
}

#[test]
fn coverage_rejects_a_neighbour_and_a_zoom_outside_the_band() {
    let (z, x, y) = (14u8, 8526u32, 5975u32);
    let coverage = coverage_over(z, x, y, 0, z);

    assert!(coverage.holds(z, x, y), "the tile it was cut over");
    assert!(!coverage.holds(z, x + 1, y), "the column next door");
    assert!(!coverage.holds(z, x, y + 1), "the row below");
    assert!(!coverage.holds(z + 1, x * 2, y * 2), "past the zoom band");
    assert!(
        coverage_over(z, x, y, 5, z).holds(z, x, y) && !coverage_over(z, x, y, 5, z).holds(4, 0, 0),
        "and below it"
    );
}

#[test]
fn coverage_holds_every_ancestor_of_the_tile_it_was_cut_over() {
    // What the rejection rule must never do is reject a tile the segment
    // really holds: an area over one tile stores that tile's whole ancestor
    // chain, so every ancestor has to pass.
    let (z, x, y) = (14u8, 8526u32, 5975u32);
    let coverage = coverage_over(z, x, y, 0, z);
    for ancestor in 0..=z {
        let shift = z - ancestor;
        assert!(
            coverage.holds(ancestor, x >> shift, y >> shift),
            "zoom {ancestor}'s ancestor of {z}/{x}/{y} must not be rejected"
        );
    }
}

#[test]
fn a_coordinate_off_the_grid_is_refused_once_rather_than_per_source() {
    const TEST: &str = "a_coordinate_off_the_grid_is_refused_once_rather_than_per_source";
    let Some(path) = fixture() else {
        return no_archive_banner(TEST, &archive_path());
    };
    let (archives, reads) = live_only(&path);
    let error = block_on(archives.tile(1, 9, 0)).expect_err("9 is off the grid at zoom 1");
    assert!(matches!(
        error,
        super::ArchiveError::Coordinate { z: 1, x: 9, y: 0 }
    ));
    assert_eq!(
        reads.load(Ordering::Relaxed),
        0,
        "an off-grid coordinate must not reach a source at all"
    );
}

// ---------------------------------------------------------------------------
// The hillshade half
// ---------------------------------------------------------------------------
//
// The same claim as the basemap's, over the second archive: a tile a
// downloaded area holds is served with the network untouched, and the control
// is the live archive serving that same tile at a cost. What is different is
// that both halves live in ONE store, so each test here also has to show the
// composition took only its own.

use crate::basemap_download::tests::{SegmentBytesSource, raster_archive};

/// A raster archive over `area`, and its own header ceiling.
///
/// Deliberately shallower than the area asks for — the terrain build stops
/// above the basemap's, and a composition that only ever saw equal ceilings
/// would not be exercising the real shape.
const TERRAIN_CEILING: u8 = 8;

/// The area both halves of these tests are cut for, and a tile inside it.
fn terrain_area(area_id: &str) -> AreaSpec {
    AreaSpec {
        area_id: area_id.to_owned(),
        west: 7.40,
        south: 43.70,
        east: 7.50,
        north: 43.76,
        max_zoom: 11,
    }
}

/// Download `area`'s terrain half out of `terrain` into `dir`, asserting it
/// finished — a partial run leaves a different set than the test reasons about.
fn download_terrain(
    terrain: &Arc<Vec<u8>>,
    dir: &std::path::Path,
    area: AreaSpec,
) -> crate::basemap_download::AreaHoldings {
    let engine = BasemapDownload::with_terrain_and_segment_bytes(
        SegmentBytesSource(Arc::clone(terrain)),
        Some(SegmentBytesSource(Arc::clone(terrain))),
        FsSegmentStore::new(dir.to_path_buf()),
        area,
        Context::default(),
        SMALL_SEGMENT_BYTES,
    );
    let start = Instant::now();
    let outcome = loop {
        if let Some(outcome) = engine.outcome() {
            break outcome;
        }
        assert!(
            start.elapsed() < DOWNLOAD_TIMEOUT,
            "the terrain download did not finish in {DOWNLOAD_TIMEOUT:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(
        matches!(outcome, DownloadOutcome::Complete { .. }),
        "the terrain download must complete, got {outcome:?}"
    );
    engine.holdings().expect("a finished run reports its cut")
}

/// The terrain composition over a counted live source, with the store's
/// **terrain** segments in front — the production route
/// (`from_terrain_archive_url` hands the same store the same archive kind).
///
/// Zeroed after open, for [`composed`]'s reason.
fn composed_terrain(
    terrain: &Arc<Vec<u8>>,
    dir: &std::path::Path,
) -> (
    BasemapArchives<CountingSource<SegmentBytesSource>, FileRangeSource>,
    Arc<AtomicUsize>,
) {
    let (live, reads) = CountingSource::new(SegmentBytesSource(Arc::clone(terrain)));
    let mut archives = block_on(BasemapArchives::open(live)).expect("the raster fixture opens");
    let store = FsSegmentStore::new(dir.to_path_buf());
    for (label, source) in block_on(store.open_all(AreaArchive::Terrain)) {
        block_on(archives.attach_offline(label, source));
    }
    reads.store(0, Ordering::Relaxed);
    (archives, reads)
}

/// The live terrain archive alone, counted — the control.
fn live_terrain_only(
    terrain: &Arc<Vec<u8>>,
) -> (
    BasemapArchives<CountingSource<SegmentBytesSource>, FileRangeSource>,
    Arc<AtomicUsize>,
) {
    let (live, reads) = CountingSource::new(SegmentBytesSource(Arc::clone(terrain)));
    let archives = block_on(BasemapArchives::open(live)).expect("the raster fixture opens");
    reads.store(0, Ordering::Relaxed);
    (archives, reads)
}

/// **The whole point of downloading terrain**: a hillshade tile a downloaded
/// area holds costs the network nothing, and the control proves the live
/// archive would have served that same tile at a cost.
#[test]
fn a_hillshade_tile_inside_a_downloaded_area_costs_the_network_nothing() {
    const TEST: &str = "a_hillshade_tile_inside_a_downloaded_area_costs_the_network_nothing";
    let area = terrain_area("hillshade-inside");
    let terrain = Arc::new(raster_archive(&area, TERRAIN_CEILING));

    // The deepest tile the terrain archive actually holds inside the area —
    // read off the enumeration rather than guessed, so a coordinate nothing
    // holds cannot make the zero below trivially true.
    let (z, x, y) = *crate::basemap_download::area_tiles_to(&area, TERRAIN_CEILING)
        .iter()
        .rfind(|(z, _, _)| *z == TERRAIN_CEILING)
        .expect("the fixture holds tiles at its own ceiling");

    // The control first.
    let (only_live, live_reads) = live_terrain_only(&terrain);
    let from_live = block_on(only_live.tile(z, x, y)).expect("the fixture reads");
    assert!(
        from_live.is_present(),
        "the control must be a tile the live terrain archive holds"
    );
    let control_reads = live_reads.load(Ordering::Relaxed);
    assert!(
        control_reads > 0,
        "reading {z}/{x}/{y} from the network must cost reads, or this test measures nothing"
    );

    let dir = TempDir::new(TEST);
    download_terrain(&terrain, &dir.0, area);
    let (archives, reads) = composed_terrain(&terrain, &dir.0);
    assert!(
        archives.offline_count() > 0,
        "the store's terrain listing must find the segments the engine published"
    );

    let served = block_on(archives.tile(z, x, y)).expect("the composition reads");
    assert_eq!(
        served.bytes(),
        from_live.bytes(),
        "a locally served hillshade tile must be byte-identical to the live one"
    );
    assert_eq!(
        reads.load(Ordering::Relaxed),
        0,
        "a hillshade tile a downloaded area holds must cost the network nothing, and \
         cost {control_reads} without one"
    );
}

/// Each reader takes **only its own** half out of the shared store: the
/// basemap composition never sees a terrain segment and the terrain
/// composition never sees a basemap one.
///
/// The read-count assertion above cannot catch this on its own — a composition
/// that took everything would still serve the tile locally. What it would cost
/// is an open, a tile-type rejection and a warning per segment per launch.
#[test]
fn each_composition_lists_only_its_own_archives_segments() {
    const TEST: &str = "each_composition_lists_only_its_own_archives_segments";
    let Some(path) = fixture() else {
        return no_archive_banner(TEST, &archive_path());
    };
    let area = terrain_area("both-halves");
    let terrain = Arc::new(raster_archive(&area, TERRAIN_CEILING));
    let dir = TempDir::new(TEST);

    // Both halves of one area, into one directory, through the real engine.
    let engine = BasemapDownload::with_terrain_and_segment_bytes(
        fixture_source(&path),
        Some(SegmentBytesSource(Arc::clone(&terrain))),
        FsSegmentStore::new(dir.0.clone()),
        area.clone(),
        Context::default(),
        SMALL_SEGMENT_BYTES,
    );
    let start = Instant::now();
    let outcome = loop {
        if let Some(outcome) = engine.outcome() {
            break outcome;
        }
        assert!(
            start.elapsed() < DOWNLOAD_TIMEOUT,
            "the download did not finish in {DOWNLOAD_TIMEOUT:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(matches!(outcome, DownloadOutcome::Complete { .. }));

    let store = FsSegmentStore::new(dir.0.clone());
    let basemap = block_on(store.open_all(AreaArchive::Basemap));
    let hillshade = block_on(store.open_all(AreaArchive::Terrain));
    assert!(
        !basemap.is_empty() && !hillshade.is_empty(),
        "both halves must be on disk, or the disjointness below is vacuous"
    );
    let names = |listed: &[(String, FileRangeSource)]| -> Vec<String> {
        listed.iter().map(|(label, _)| label.clone()).collect()
    };
    let basemap_names = names(&basemap);
    let hillshade_names = names(&hillshade);
    assert!(
        basemap_names
            .iter()
            .all(|name| !hillshade_names.contains(name)),
        "a segment was listed as both halves: {basemap_names:?} / {hillshade_names:?}"
    );
    assert!(
        hillshade_names
            .iter()
            .all(|name| name.ends_with(".terrain.pmtiles")),
        "the terrain listing took an artifact that is not one: {hillshade_names:?}"
    );
    assert!(
        basemap_names.iter().all(|name| !name.contains(".terrain.")),
        "the basemap listing took a hillshade artifact: {basemap_names:?}"
    );
}

/// **A corrupt hillshade segment degrades to the live archive, never to a
/// blank map and never to a notice.** The basemap half is untouched by it.
#[test]
fn a_corrupt_hillshade_segment_falls_through_to_the_live_archive() {
    const TEST: &str = "a_corrupt_hillshade_segment_falls_through_to_the_live_archive";
    let area = terrain_area("corrupt-hillshade");
    let terrain = Arc::new(raster_archive(&area, TERRAIN_CEILING));
    let (z, x, y) = *crate::basemap_download::area_tiles_to(&area, TERRAIN_CEILING)
        .iter()
        .rfind(|(z, _, _)| *z == TERRAIN_CEILING)
        .expect("the fixture holds tiles at its own ceiling");

    let dir = TempDir::new(TEST);
    download_terrain(&terrain, &dir.0, area);

    // Every published hillshade artifact, overwritten with bytes that are not
    // an archive. Nothing here may panic and nothing may reach the glass.
    let mut wrecked = 0;
    for entry in std::fs::read_dir(&dir.0)
        .expect("the store lists")
        .flatten()
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".terrain.pmtiles") {
            std::fs::write(entry.path(), b"not a PMTiles archive at all")
                .expect("the wreck writes");
            wrecked += 1;
        }
    }
    assert!(
        wrecked > 0,
        "nothing was corrupted, so this test proves nothing"
    );

    let (archives, reads) = composed_terrain(&terrain, &dir.0);
    assert_eq!(
        archives.offline_count(),
        0,
        "a segment that will not open must be skipped at attach, not composed"
    );
    let served = block_on(archives.tile(z, x, y)).expect("the composition still reads");
    assert!(
        served.is_present(),
        "a corrupt local segment cost the tile: the live archive must serve it"
    );
    assert!(
        reads.load(Ordering::Relaxed) > 0,
        "the tile did not come from the live archive, so the fall-through did not happen"
    );
}
