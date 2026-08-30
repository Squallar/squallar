//! The height reader, round-tripped end to end against a fixture archive.
//!
//! # What is proven here, and what is owed
//!
//! No terrain-RGB archive has been published. Everything below therefore runs
//! against an archive this file writes: a real PMTiles v3 file, written by the
//! same `pmtiles` writer the download engine uses, declaring `tile_type = 2`
//! (PNG) and holding one body whose pixels are packed elevation. What that
//! proves is the **reader**, whole:
//!
//! * [`super::HEIGHT_ARCHIVE_URL_ENV`] is honoured by
//!   [`super::height_archive_url`];
//! * the chain the plan names —
//!   `HttpRangeSource` → `BlockCachedSource` → `BasemapArchives::open` →
//!   `.tile(z, x, y)` — reaches **undecoded bytes**, byte-identical to what was
//!   written;
//! * the `.partNNN` probe selects parts mode, and the bare archive path — which
//!   404s by design on a published archive — is never asked for;
//! * the height archive's block-cache generation survives
//!   `gc_stale_generations` **with bytes already in it**, while a generation
//!   that is not in the live list is deleted in the same pass — the whole
//!   point of the [`super::live_archive_urls`] change.
//!
//! That last one is only worth anything because both generation directories
//! are **pre-populated before the first read**. The GC runs once, inside
//! `ensure_open`, on that first read; a height generation directory that did
//! not exist yet would be created by the block writes that follow whatever the
//! GC did, so "the directory is there afterwards" proves nothing. Each
//! directory therefore gets a dot-file marker first: `recover_totals` skips
//! dot-files, and the only thing in the tree that removes one is the GC's own
//! `remove_dir_all`. Surviving marker means survived; missing marker means
//! deleted.
//!
//! What is **not** proven is that the published archives exist, that they carry
//! terrain-RGB rather than hillshade pixels, or that their generations are the
//! ones compiled in. Those are owed to a real archive; see
//! [`super::HEIGHT_ARCHIVE_GENERATION_PLACEHOLDER`].
//!
//! # Why the round trip runs in a child process
//!
//! [`super::height_archive_url`] reads the process environment, and the URL it
//! must read is a loopback port that does not exist until a server is bound.
//! `std::env::set_var` is `unsafe` in this edition precisely because libtest
//! runs tests on many threads while other tests in this crate call
//! `std::env::var`, so the parent binds the port, serves the fixture, and
//! re-executes **this test binary** with the variable set through
//! `Command::env` — a safe write, into a process that has not started.
//!
//! The child prints [`ROUND_TRIP_PROOF`] only after the last assertion. The
//! parent requires that line, so a child that skipped, or that exited zero
//! having asserted nothing, reddens the parent.
//!
//! # The client is not `archive_client`
//!
//! [`super::height_range_source`] builds its source with
//! `basemap_archive::archive_client`, which sets `https_only` — pinned by
//! `the_archive_client_refuses_cleartext`. A cleartext loopback URL fails that
//! check at the builder, so the child spells the *same construction* with the
//! harness's plain client. The difference between the two is one argument, and
//! it is the argument that decides the scheme, not the read path.

use std::path::{Path, PathBuf};

use crate::basemap_archive::tests as harness;
use crate::basemap_archive::{BasemapArchives, HttpRangeSource, PART_BYTES, TileBytes};

/// The one tile the fixture archive holds, and the one the child reads back.
///
/// z1 rather than z0 so that [`ABSENT_TILE`] can exist: a zoom with exactly
/// one tile in it has no neighbour, and `TileCoord::new` refuses the
/// coordinate rather than the reader answering `Absent` for it.
const TILE: (u8, u32, u32) = (1, 0, 0);

/// A tile at the fixture's own zoom that the fixture does not hold.
///
/// The control on the round trip: `Absent` is a positive answer reached by
/// reading the directories, so a reader that returned the same bytes for every
/// coordinate would fail here.
const ABSENT_TILE: (u8, u32, u32) = (1, 1, 1);

/// Side of the fixture tile, in pixels. Small on purpose: this is an archive
/// reader test, and the body's only job is to be a distinctive byte string.
const FIXTURE_SIDE: u32 = 4;

/// The line the child prints once, after its last assertion.
///
/// The parent's non-triviality floor. Without it a child that took the skip
/// path — or one whose test filter matched nothing, which libtest exits `0`
/// for — would be indistinguishable from a child that read the tile.
const ROUND_TRIP_PROOF: &str = "SQUALLAR-HEIGHT-ROUND-TRIP-OK";

/// The child half of the round trip, named for the harness filter.
const CHILD_TEST: &str = "tiles::height_tests::the_height_round_trip_child_half";

/// A generation directory that is in nobody's live set.
///
/// The control for the cache assertion: the height generation surviving the
/// GC means nothing unless something in the same pass is deleted.
const STALE_GENERATION: &str = "a-generation-nothing-reads";

/// The file planted in a generation directory before the first read, to tell
/// "survived the GC" from "was recreated by the writes after it".
///
/// A dot-file on purpose. `recover_totals` skips dot-files — it neither counts
/// nor deletes them — and the block writer only ever creates `{index:010}`
/// names, so nothing in the tree can put this back. The only code that removes
/// it is `gc_stale_generations`' `remove_dir_all` of the whole directory.
const SURVIVOR_MARKER: &str = ".planted-before-the-gc-ran";

/// Create `dir` and plant [`SURVIVOR_MARKER`] in it.
fn plant(dir: &Path) {
    std::fs::create_dir_all(dir).expect("the generation directory is created");
    std::fs::write(dir.join(SURVIVOR_MARKER), b"planted before the first read")
        .expect("the marker writes");
}

/// One fixture pixel, as a terrain-RGB triple.
///
/// The unpack itself belongs to the elevation crate, not here; all this needs
/// is a body that could not be mistaken for anything else and that is a
/// genuine PNG, so `tile_type = 2` on the archive is true rather than
/// convenient.
fn fixture_pixel(x: u32, y: u32) -> [u8; 3] {
    [(11 + x * 3) as u8, (200 - y * 17) as u8, (7 + x * y) as u8]
}

/// The fixture tile body: a real PNG carrying [`fixture_pixel`].
///
/// Deterministic across the two processes because it is the same encoder in
/// the same binary — the parent writes it into the archive it serves, the
/// child asserts the bytes it read are equal to it.
fn terrain_rgb_png() -> Vec<u8> {
    let mut bitmap = image::RgbImage::new(FIXTURE_SIDE, FIXTURE_SIDE);
    for y in 0..FIXTURE_SIDE {
        for x in 0..FIXTURE_SIDE {
            bitmap.put_pixel(x, y, image::Rgb(fixture_pixel(x, y)));
        }
    }

    let mut encoded = std::io::Cursor::new(Vec::new());
    bitmap
        .write_to(&mut encoded, image::ImageFormat::Png)
        .expect("the fixture should encode as a PNG");
    encoded.into_inner()
}

/// A one-tile terrain-RGB archive, written by the `pmtiles` writer.
///
/// `Compression::None` so the bytes the reader hands back are the bytes that
/// went in: a compressed body would still round-trip, but through a decompress
/// the assertion could not distinguish from a decode.
fn height_archive_fixture() -> Vec<u8> {
    let (z, x, y) = TILE;
    let mut sink = std::io::Cursor::new(Vec::new());
    let mut writer = pmtiles::PmTilesWriter::new(pmtiles::TileType::Png)
        .tile_compression(pmtiles::Compression::None)
        .min_zoom(z)
        .max_zoom(z)
        .bounds(-180.0, -85.0, 180.0, 85.0)
        .create(&mut sink)
        .expect("the writer opens");
    writer
        .add_tile(
            pmtiles::TileCoord::new(z, x, y).expect("the fixture coordinate is a tile"),
            terrain_rgb_png().as_slice(),
        )
        .expect("the tile writes");
    writer.finalize().expect("the archive finalizes");
    sink.into_inner()
}

/// A per-test directory under the OS temp dir, removed on drop — the idiom
/// `basemap_archive::archives_tests` uses, for its reason.
struct TempRoot(PathBuf);

impl TempRoot {
    fn new(label: &str) -> Self {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "squallar-height-reader-{}-{label}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("the cache root is created");
        Self(path)
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Bytes held under `path`, recursively.
fn bytes_under(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.metadata() {
            Ok(meta) if meta.is_dir() => bytes_under(&entry.path()),
            Ok(meta) => meta.len(),
            Err(_) => 0,
        })
        .sum()
}

// ---------------------------------------------------------------------------
// The round trip
// ---------------------------------------------------------------------------

/// A height tile reaches undecoded bytes through the archive the override
/// names, over the layout a published archive is served in.
#[test]
fn a_height_tile_round_trips_to_undecoded_bytes_from_the_archive_the_override_names() {
    let body = height_archive_fixture();

    // The published shape: `.partNNN` only, and **nothing at the bare path**,
    // so a reader that skipped the probe would 404 rather than pass by
    // accident. `PART_BYTES` is the stride `HttpRangeSource::new` reads at, so
    // the fixture is one part exactly as a small published archive would be.
    let server = harness::RangeServer::parted(&body, PART_BYTES as usize);
    let url = server.url();

    let binary = std::env::current_exe().expect("a test process knows its own binary");
    let output = std::process::Command::new(&binary)
        .env(super::HEIGHT_ARCHIVE_URL_ENV, &url)
        .args(["--exact", "--ignored", "--nocapture", CHILD_TEST])
        .output()
        .expect("the test binary re-executes");

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "the child half failed against {url}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(
        stdout.contains(ROUND_TRIP_PROOF),
        "the child exited zero without reaching the round trip — a filter that \
         matched nothing, or a skip, exits zero too\n--- stdout ---\n{stdout}\n\
         --- stderr ---\n{stderr}"
    );

    // Where the bytes were asked for. A published height archive is parts, and
    // a bare `GET` of the logical name 404s: the probe is what keeps that from
    // being a fault.
    let part000 = format!("{}.part000", harness::ARCHIVE_PATH);
    assert!(
        server.requests_to(&part000) > 0,
        "the reader never probed {part000}; the parts layout was not selected"
    );
    assert_eq!(
        server.requests_to(harness::ARCHIVE_PATH),
        0,
        "the reader asked for the bare archive path, which a published archive \
         answers 404 for"
    );
}

/// The half that runs with [`super::HEIGHT_ARCHIVE_URL_ENV`] set, in a process
/// whose environment was written before it started.
///
/// `#[ignore]` because it is meaningless without that variable and the parent
/// is the only thing that sets it. Run directly it takes the skip path below
/// and prints no proof line, which is exactly what the parent refuses.
#[test]
#[ignore = "driven by its parent, which binds the port and sets the override"]
fn the_height_round_trip_child_half() {
    if std::env::var(super::HEIGHT_ARCHIVE_URL_ENV).is_err() {
        use std::io::Write as _;
        let _ = writeln!(
            std::io::stderr(),
            "\n## SKIPPED {CHILD_TEST}: {} is not set, so this asserted NOTHING.\n\
             ## It is the child half of \
             a_height_tile_round_trips_to_undecoded_bytes_from_the_archive_the_override_names; \
             run that instead.\n",
            super::HEIGHT_ARCHIVE_URL_ENV
        );
        return;
    }

    // The override, read through the production function — this is the whole
    // reason the halves are two processes.
    let url = super::height_archive_url();
    assert_ne!(
        url,
        super::HEIGHT_ARCHIVE_URL,
        "{} was set and {} still answered the compiled-in archive",
        super::HEIGHT_ARCHIVE_URL_ENV,
        "height_archive_url"
    );

    // A cache root with **both** generation directories already populated. The
    // GC runs once per root per process, inside the first read, and deletes
    // every directory that is not live; planting before that is what makes the
    // survival below a fact about the GC rather than a fact about the block
    // writes that follow it.
    let root = TempRoot::new("round-trip");
    let stale = root.0.join(STALE_GENERATION);
    plant(&stale);

    super::install_basemap_cache_dir(root.0.clone());
    let config = super::archive_block_cache(&url).expect("a cache directory was installed");
    let generation = config.generation.clone();
    let live_dir = root.0.join(&generation);
    plant(&live_dir);

    // The derived half of the live-set ratchet. `tiles::tests` pins the list
    // itself; this is the one process in the suite that installs a cache
    // directory, so it is the only place the *config* built from that list can
    // be looked at without changing what every other test writes through.
    for live in super::live_archive_urls() {
        let derived = crate::basemap_archive::block_cache::generation_for_url(&live);
        assert!(
            config.live_generations.contains(&derived),
            "{live} derives generation {derived}, which archive_block_cache \
             left out of the live set {:?} — that archive's cache is deleted at \
             the first open of every launch",
            config.live_generations
        );
    }
    assert!(
        config.live_generations.contains(&generation),
        "the height archive's own generation is missing from the live set, so \
         its cache is deleted at the first open of every launch"
    );

    // The chain, exactly as the plan spells it. Nothing here is an
    // `HttpsTiles`: no texture, no style, no decode.
    let source = HttpRangeSource::new(harness::loopback_client(), &url)
        .expect("the loopback archive URL parses");
    let cached = crate::basemap_archive::block_cache::BlockCachedSource::new(source, Some(config));
    let archives: BasemapArchives<_, HttpRangeSource> =
        harness::block_on(BasemapArchives::open(cached)).expect("the height archive opens");

    assert_eq!(
        archives.tile_type(),
        pmtiles::TileType::Png,
        "a terrain-RGB archive declares PNG bodies"
    );

    let (z, x, y) = TILE;
    let read = harness::block_on(archives.tile(z, x, y)).expect("the height tile reads");
    assert_eq!(
        read,
        TileBytes::Present(terrain_rgb_png()),
        "the height tile did not round-trip byte for byte"
    );

    // A tile the archive does not hold is a positive `Absent`, not a failure —
    // the distinction the whole reader is built on, and the control that says
    // the assertion above is about these bytes rather than about any bytes.
    let (az, ax, ay) = ABSENT_TILE;
    assert_eq!(
        harness::block_on(archives.tile(az, ax, ay)).expect("a missing tile is not an error"),
        TileBytes::Absent,
        "a tile outside the fixture must answer Absent"
    );

    // The GC ran: the generation nothing reads is gone, marker and all.
    assert!(
        !stale.exists(),
        "the stale generation survived, so this process's GC never ran and the \
         survival below would prove nothing"
    );

    // And it spared the height archive's, which is the claim
    // `live_archive_urls` exists to make true. The marker is the evidence: it
    // was planted before the first read and nothing but the GC could remove
    // it, so a directory holding it is a directory the GC walked past.
    assert!(
        live_dir.join(SURVIVOR_MARKER).exists(),
        "the height generation directory was deleted and rebuilt by the block \
         writes: its marker is gone, so the generation is not in the live set \
         the GC honours"
    );

    // And the read really did go through the cache, so the directory the GC
    // spared is one with bytes worth sparing.
    assert!(
        bytes_under(&live_dir) > u64::try_from(SURVIVOR_MARKER.len()).unwrap_or(0),
        "the height archive's generation directory holds nothing but the \
         marker, so the read never went through the cache"
    );

    println!("{ROUND_TRIP_PROOF}");
}
