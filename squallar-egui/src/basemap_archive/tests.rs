//! Tests for [`super`].
//!
//! Two halves, and they answer different questions.
//!
//! The **transport** half runs against a loopback server and needs nothing on
//! disk. It is where retry-on-200 and the absence/failure distinction are
//! gated, so those never depend on a fixture being present.
//!
//! The **archive** half runs against the Monaco build committed at
//! [`DEFAULT_ARCHIVE`], so it runs everywhere rather than only on a workstation
//! that happens to hold a regional build. Small is not the same as toy: that
//! archive has `n_addressed_tiles` 246, `n_tile_entries` 157 and
//! `n_tile_contents` 108, three different numbers, which is what makes it
//! exercise both of PMTiles' dedup mechanisms rather than neither.
//!
//! What it cannot exercise is the leaf-directory path. All 246 tiles fit in a
//! 511-byte root directory, so the archive has no leaves, and a test about what
//! reading one costs has no second read to count. Pointing [`ARCHIVE_ENV`] at a
//! larger build runs [`directory_cache_is_load_bearing`] for real; the
//! coordinate-driven tests read their seed point out of whatever archive they
//! were handed, so they follow the override rather than breaking under it.
//!
//! **When a test cannot assert the thing it exists to assert it skips rather
//! than passing quietly, and the skip is shouted.** A green run that tested
//! nothing is the failure mode being designed out here, so [`skip_banner`]
//! writes straight to the process's stderr handle rather than through
//! `eprintln!`: libtest captures the macro and hides it behind `--nocapture`,
//! and a skip notice nobody sees is not a notice.

use std::io::Write as _;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use pmtiles::{Compression, TileType};

use super::{
    ArchiveError, BasemapArchive, FileRangeSource, HttpRangeSource, RANGE_ATTEMPTS, RangeError,
    RangeSource, TileBytes,
};

// ---------------------------------------------------------------------------
// The real archive, and the loud skip
// ---------------------------------------------------------------------------

/// The committed Monaco fixture, resolved through `CARGO_MANIFEST_DIR` so the
/// suite finds it whatever directory it was invoked from. `testdata/README.md`
/// records how it was built and why it never needs rebuilding.
const DEFAULT_ARCHIVE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/monaco.pmtiles");

/// Environment variable naming a different archive, so the same suite can be
/// pointed at a full regional build.
const ARCHIVE_ENV: &str = "SQUALLAR_PMTILES_ARCHIVE";

/// Byte length of the committed fixture.
const MONACO_BYTES: u64 = 419_355;

/// The three dedup counters of the fixture. All three differ, which is the
/// property that makes it a non-trivial input: `run_length` collapsing
/// consecutive ids is the first gap and content-hashing collapsing non-adjacent
/// ones is the second, so an input where the three agreed would exercise
/// neither dedup mechanism and the header pins would prove much less than they
/// look like they do.
const MONACO_ADDRESSED_TILES: u64 = 246;
/// See [`MONACO_ADDRESSED_TILES`].
const MONACO_TILE_ENTRIES: u64 = 157;
/// See [`MONACO_ADDRESSED_TILES`].
const MONACO_TILE_CONTENTS: u64 = 108;

/// The centre point the fixture's header declares, inside a bounding box of
/// lon 7.408583..7.595671 and lat 43.483817..43.752930.
const MONACO_LON: f64 = 7.502_127;
/// See [`MONACO_LON`].
const MONACO_LAT: f64 = 43.618_373_5;

/// The path to test against, if there is one.
fn archive_path() -> PathBuf {
    std::env::var_os(ARCHIVE_ENV).map_or_else(|| PathBuf::from(DEFAULT_ARCHIVE), PathBuf::from)
}

/// Shout that a test tested nothing.
///
/// Straight at the stderr handle: `eprintln!` goes through libtest's output
/// capture and would be swallowed on a passing test, which is precisely the
/// "green run that tested nothing" this exists to prevent.
///
/// `reason` and `remedy` are arguments rather than one fixed message because
/// there is more than one way for a test here to have nothing to run against —
/// no archive at all, or an archive that cannot exercise this particular test —
/// and the two call for different fixes. A reader who cannot tell them apart
/// will apply neither.
fn skip_banner(test: &str, reason: &str, remedy: &str) {
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(
        stderr,
        "\n\
         ###########################################################################\n\
         ## SKIPPED, NOT PASSED: {test}\n\
         ##   {reason}\n\
         ##   this test asserted NOTHING. {remedy}\n\
         ##   before reading this suite as covering the reader.\n\
         ###########################################################################"
    );
}

/// The "there is nothing on disk" skip, phrased once.
fn no_archive_banner(test: &str, path: &Path) {
    skip_banner(
        test,
        &format!("no PMTiles archive at {}", path.display()),
        &format!("Restore the committed fixture, or point {ARCHIVE_ENV} at an archive,"),
    );
}

/// The archive, or `None` with a shouted skip.
fn open_archive_file(test: &str) -> Option<FileRangeSource> {
    let path = archive_path();
    if !path.is_file() {
        no_archive_banner(test, &path);
        return None;
    }

    Some(FileRangeSource::open(&path).expect("an existing archive file should open"))
}

/// Byte length of the archive's leaf directories, read straight out of the
/// PMTiles v3 header: bytes 48..56, a little-endian `u64`.
///
/// By hand because `pmtiles::Header` does not expose it, and
/// [`directory_cache_is_load_bearing`] cannot tell whether it has been handed a
/// usable input without it. Zero means every tile is addressed from the root
/// directory, so there is no second read for the cache to save.
fn leaf_directory_len(path: &Path) -> u64 {
    let mut file = std::fs::File::open(path).expect("an existing archive file should open");
    let mut header = [0_u8; 56];
    std::io::Read::read_exact(&mut file, &mut header)
        .expect("a PMTiles archive is at least a 127-byte header");
    u64::from_le_bytes(
        header[48..56]
            .try_into()
            .expect("a fixed eight-byte slice is a [u8; 8]"),
    )
}

/// The centre point the archive itself declares.
///
/// From the header rather than hardcoded so the coordinate tests survive
/// [`ARCHIVE_ENV`]: a constant over Monaco is not in an Oklahoma build, and a
/// test that reddened on that would be reddening for a reason with nothing to
/// do with the reader under test.
fn archive_centre<S: super::ArchiveRangeSource>(archive: &BasemapArchive<S>) -> (f64, f64) {
    let header = archive.header();
    (header.center_longitude, header.center_latitude)
}

/// A client that can reach the cleartext loopback server.
///
/// [`super::archive_client`] cannot: it sets `https_only`, which is pinned by
/// `the_archive_client_refuses_cleartext` below. Same split, and for the same
/// reason, as `tile_source::tests::loopback_client`.
fn loopback_client() -> reqwest::Client {
    squallar_radar::tls::init();
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("the loopback client should build")
}

/// Run `future` on a current-thread runtime.
fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime should build")
        .block_on(future)
}

// ---------------------------------------------------------------------------
// Sources built for the tests
// ---------------------------------------------------------------------------

/// Wraps a source and counts the ranges asked of it.
struct CountingSource<S> {
    inner: S,
    reads: Arc<AtomicUsize>,
}

impl<S> CountingSource<S> {
    fn new(inner: S) -> (Self, Arc<AtomicUsize>) {
        let reads = Arc::new(AtomicUsize::new(0));
        (
            Self {
                inner,
                reads: Arc::clone(&reads),
            },
            reads,
        )
    }
}

impl<S: RangeSource + Send + Sync> RangeSource for CountingSource<S> {
    fn read_range(
        &self,
        offset: u64,
        length: usize,
    ) -> impl Future<Output = Result<Vec<u8>, RangeError>> + Send {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.inner.read_range(offset, length)
    }
}

/// Serves the archive's opening bytes and then fails every later range.
///
/// The point is that it opens *successfully* — the header and root directory
/// arrive — and only fails once a tile is asked for. A source that failed at
/// `open` would never reach the code under test.
struct FailsAfterOpenSource {
    inner: FileRangeSource,
    healthy: Arc<AtomicU32>,
}

impl RangeSource for FailsAfterOpenSource {
    fn read_range(
        &self,
        offset: u64,
        length: usize,
    ) -> impl Future<Output = Result<Vec<u8>, RangeError>> + Send {
        let allowed = self.healthy.load(Ordering::Relaxed) > 0;
        if allowed {
            self.healthy.fetch_sub(1, Ordering::Relaxed);
        }

        let inner = allowed.then(|| self.inner.read_range(offset, length));

        async move {
            match inner {
                Some(read) => read.await,
                None => Err(RangeError::Transport("the network went away".into())),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// A loopback server that can be told how to answer a range request
// ---------------------------------------------------------------------------

/// How the loopback server answers.
#[derive(Clone, Copy)]
enum Answer {
    /// `200` with a body, i.e. the whole resource — what a host that ignores
    /// `Range` does, and what 2 of 11 real requests did.
    WholeBody,
    /// `206` with the requested slice.
    Range,
    /// `500`.
    ServerError,
}

/// A loopback HTTP server answering range requests over a fixed body.
struct RangeServer {
    port: u16,
    requests: Arc<AtomicUsize>,
    #[expect(
        dead_code,
        reason = "held to keep the server thread alive for the test"
    )]
    thread: std::thread::JoinHandle<()>,
}

impl RangeServer {
    /// Answer `whole_body_first` requests with a `200`, then follow `then`.
    fn start(body: Vec<u8>, whole_body_first: usize, then: Answer) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port should bind");
        let port = listener
            .local_addr()
            .expect("a bound listener has an address")
            .port();
        let requests = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&requests);

        let thread = std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let seen = counter.fetch_add(1, Ordering::SeqCst);
                let answer = if seen < whole_body_first {
                    Answer::WholeBody
                } else {
                    then
                };

                if serve_one(stream, &body, answer).is_err() {
                    break;
                }
            }
        });

        Self {
            port,
            requests,
            thread,
        }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/archive.pmtiles", self.port)
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

/// Read one request, answer it, close.
fn serve_one(mut stream: TcpStream, body: &[u8], answer: Answer) -> std::io::Result<()> {
    use std::io::Read as _;

    let mut request = Vec::new();
    let mut buffer = [0u8; 1024];
    loop {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..read]);
        if request.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }

    let request = String::from_utf8_lossy(&request).to_string();
    let (start, end) = parse_range(&request, body.len());

    match answer {
        Answer::WholeBody => {
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes())?;
            stream.write_all(body)?;
        }
        Answer::Range => {
            let slice = &body[start..end];
            let head = format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {}-{}/{}\r\nContent-Length: \
                 {}\r\nConnection: close\r\n\r\n",
                start,
                end.saturating_sub(1),
                body.len(),
                slice.len()
            );
            stream.write_all(head.as_bytes())?;
            stream.write_all(slice)?;
        }
        Answer::ServerError => {
            stream.write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")?;
        }
    }

    stream.flush()
}

/// `bytes=N-M` out of a request head, clamped to `len`.
fn parse_range(request: &str, len: usize) -> (usize, usize) {
    let Some(spec) = request.lines().find_map(|line| {
        line.strip_prefix("Range: bytes=")
            .or_else(|| line.strip_prefix("range: bytes="))
    }) else {
        return (0, len);
    };

    let spec = spec.trim();
    let mut halves = spec.split('-');
    let start: usize = halves.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    let end: usize = halves.next().and_then(|v| v.parse().ok()).unwrap_or(len);

    (start.min(len), end.saturating_add(1).min(len))
}

// ---------------------------------------------------------------------------
// Transport: retry on 200
// ---------------------------------------------------------------------------

/// A host answering `200` to a range request has not corrupted the archive; it
/// has ignored the header. 2 of 11 real requests did exactly this, and treating
/// it as fatal is what makes a transport fault read as a bad file.
#[test]
fn a_two_hundred_is_retried_rather_than_failing_the_read() {
    let body: Vec<u8> = (0u8..=255).cycle().take(4096).collect();
    let server = RangeServer::start(body.clone(), 1, Answer::Range);
    let source = HttpRangeSource::new(loopback_client(), &server.url())
        .expect("the loopback URL should parse");

    let bytes = block_on(source.read_range(100, 64)).expect("the retry should have succeeded");

    assert_eq!(
        bytes,
        &body[100..164],
        "the retried attempt should return the range that was asked for"
    );
    assert_eq!(
        server.requests(),
        2,
        "one attempt should have been spent on the 200 and one on the 206"
    );
}

/// Bounded, because a host that does not do ranges at all will never start —
/// and an unbounded retry against it is an infinite loop wearing a recovery's
/// clothes. The error names the status and the count so the reader can tell the
/// two readings of the symptom apart.
#[test]
fn retries_are_bounded_and_the_error_says_what_happened() {
    let body: Vec<u8> = (0u8..=255).cycle().take(4096).collect();
    let server = RangeServer::start(body, usize::MAX, Answer::WholeBody);
    let source = HttpRangeSource::new(loopback_client(), &server.url())
        .expect("the loopback URL should parse");

    let error = block_on(source.read_range(0, 64)).expect_err("a host that never ranges must fail");

    assert_eq!(
        error,
        RangeError::NotRanged {
            status: 200,
            attempts: RANGE_ATTEMPTS,
        },
        "the failure should carry the status and the attempt count"
    );
    assert_eq!(
        server.requests(),
        RANGE_ATTEMPTS as usize,
        "the retry loop should be bounded at RANGE_ATTEMPTS"
    );
    assert!(
        error
            .to_string()
            .contains("does not appear to serve range requests"),
        "the message should point at the transport, not at the archive: {error}"
    );
}

/// A failure status is not a range problem and must not be retried into one.
#[test]
fn a_failure_status_is_reported_rather_than_retried() {
    let server = RangeServer::start(Vec::new(), 0, Answer::ServerError);
    let source = HttpRangeSource::new(loopback_client(), &server.url())
        .expect("the loopback URL should parse");

    let error = block_on(source.read_range(0, 64)).expect_err("a 500 should fail the read");

    assert!(
        matches!(error, RangeError::Transport(_)),
        "a 500 should be a transport error, not a range-support verdict: {error}"
    );
    assert_eq!(
        server.requests(),
        1,
        "a failure status should not be retried"
    );
}

// ---------------------------------------------------------------------------
// The archive
// ---------------------------------------------------------------------------

/// The header is read from the archive rather than assumed anywhere, so this
/// pins what the 5a build actually contains — including the three dedup
/// counters, which differ from one another and are what make this a real input
/// rather than a fixture that would pass against a much weaker reader.
#[test]
fn the_header_matches_the_built_archive() {
    let Some(source) = open_archive_file("the_header_matches_the_built_archive") else {
        return;
    };
    // Every pin below describes the committed fixture specifically, so an
    // overridden archive is not a failure here -- it is a different archive,
    // and asserting Monaco's counters against it would say nothing about the
    // reader.
    if source.len() != MONACO_BYTES {
        skip_banner(
            "the_header_matches_the_built_archive",
            "the archive under test is not the committed Monaco fixture",
            &format!("Unset {ARCHIVE_ENV} to pin the fixture these constants describe,"),
        );
        return;
    }

    let archive = block_on(BasemapArchive::open(source)).expect("the archive should open");
    let header = archive.header();

    assert_eq!(header.spec_version(), 3, "PMTiles v3");
    assert_eq!(archive.min_zoom(), 0);
    assert_eq!(archive.max_zoom(), 14);
    assert_eq!(archive.tile_type(), TileType::Mvt);
    assert_eq!(archive.tile_compression(), Compression::Gzip);
    assert!(header.clustered(), "the fixture is clustered");

    let counters = (
        header.n_addressed_tiles().map(std::num::NonZeroU64::get),
        header.n_tile_entries().map(std::num::NonZeroU64::get),
        header.n_tile_contents().map(std::num::NonZeroU64::get),
    );
    assert_eq!(
        counters,
        (
            Some(MONACO_ADDRESSED_TILES),
            Some(MONACO_TILE_ENTRIES),
            Some(MONACO_TILE_CONTENTS),
        ),
    );

    assert_eq!(
        (header.center_longitude, header.center_latitude),
        (MONACO_LON, MONACO_LAT),
        "the seed point the coordinate tests read out of the header"
    );

    // Pinned rather than described in a comment, because it is the fact
    // `directory_cache_is_load_bearing` skips on and prose is not evidence.
    assert_eq!(
        leaf_directory_len(&archive_path()),
        0,
        "the fixture's 246 tiles all fit in its 511-byte root directory, so it has no leaf \
         directories at all"
    );
}

/// The non-triviality floor under [`the_header_matches_the_built_archive`], and
/// a compile-time one because it is a statement about the constants rather than
/// about a run: `run_length` collapsing consecutive ids is the first gap and
/// content-hashing collapsing non-adjacent ones is the second, so an input where
/// the three counters were equal would exercise neither dedup mechanism and the
/// header pins above would prove much less than they look like they do.
const _: () = {
    assert!(MONACO_ADDRESSED_TILES > MONACO_TILE_ENTRIES);
    assert!(MONACO_TILE_ENTRIES > MONACO_TILE_CONTENTS);
};

/// A tile the archive holds comes back present, decompressed, and non-empty.
#[test]
fn a_tile_the_archive_holds_comes_back_decompressed() {
    let Some(source) = open_archive_file("a_tile_the_archive_holds_comes_back_decompressed") else {
        return;
    };

    let archive = block_on(BasemapArchive::open(source)).expect("the archive should open");
    let z = archive.max_zoom();
    let (lon, lat) = archive_centre(&archive);
    let x = squallar_geo::lon_to_tile_x(lon, z);
    let y = squallar_geo::lat_to_tile_y(lat, z);

    let tile = block_on(archive.tile(z, x, y)).expect("reading a covered tile should succeed");

    let bytes = tile.bytes().unwrap_or_else(|| {
        panic!("{z}/{x}/{y} is the archive's own declared centre and must be in it")
    });
    assert!(!bytes.is_empty(), "an MVT tile is not zero bytes");
    assert_ne!(
        &bytes[..2],
        &[0x1f, 0x8b],
        "the tiles are gzip on the wire; `tile` must hand back the decompressed body, not the \
         gzip member"
    );
}

/// Absence is a positive answer: the directories were read and hold nothing.
#[test]
fn a_tile_outside_the_coverage_is_absent_rather_than_an_error() {
    let Some(source) =
        open_archive_file("a_tile_outside_the_coverage_is_absent_rather_than_an_error")
    else {
        return;
    };

    let archive = block_on(BasemapArchive::open(source)).expect("the archive should open");
    let z = archive.max_zoom();
    // Mid-Atlantic: outside any regional build this suite is pointed at.
    let x = squallar_geo::lon_to_tile_x(-30.0, z);
    let y = squallar_geo::lat_to_tile_y(25.0, z);

    let tile = block_on(archive.tile(z, x, y)).expect("a missing tile is not an error");

    assert_eq!(
        tile,
        TileBytes::Absent,
        "{z}/{x}/{y} is open ocean and a regional archive should simply not hold it"
    );
}

/// **The distinction the type exists for.** The same call that answers `Absent`
/// for a tile the archive does not hold must answer `Err` when the bytes could
/// not be reached — for a tile that certainly *is* there. Collapsing the two
/// renders an empty map either way and reads green.
#[test]
fn a_failing_source_errors_and_is_never_reported_as_absence() {
    let Some(inner) = open_archive_file("a_failing_source_errors_and_is_never_reported_as_absence")
    else {
        return;
    };

    // Enough healthy reads for the header and root directory, then nothing.
    let healthy = Arc::new(AtomicU32::new(1));
    let source = FailsAfterOpenSource {
        inner,
        healthy: Arc::clone(&healthy),
    };

    let archive = block_on(BasemapArchive::open(source))
        .expect("the opening read is served, so the archive should open");

    let z = archive.max_zoom();
    let (lon, lat) = archive_centre(&archive);
    let x = squallar_geo::lon_to_tile_x(lon, z);
    let y = squallar_geo::lat_to_tile_y(lat, z);
    assert_eq!(
        healthy.load(Ordering::Relaxed),
        0,
        "the source should now be refusing every read"
    );

    let outcome = block_on(archive.tile(z, x, y));

    let error = match outcome {
        Ok(TileBytes::Absent) => panic!(
            "a transport failure was reported as absence -- the map would draw empty and the run \
             would read green"
        ),
        Ok(TileBytes::Present(_)) => panic!("the source refused every read; nothing could arrive"),
        Err(error) => error,
    };
    assert!(
        matches!(error, ArchiveError::Tile(_)),
        "a failure reaching the bytes is a tile read error: {error}"
    );
    assert!(
        error.to_string().contains("the network went away"),
        "the source's own reason should survive to the caller: {error}"
    );
}

/// Not an optimisation. Without the directory cache every tile pays a second
/// range request for the leaf directory it lives in, on every fetch — the cost
/// that makes a planet archive unusable. Counted, because the version of this
/// test that asserts "the cache exists" cannot fail.
#[test]
fn directory_cache_is_load_bearing() {
    let Some(inner) = open_archive_file("directory_cache_is_load_bearing") else {
        return;
    };

    // The committed fixture cannot run this one, and weakening the assertion so
    // that it could would be worse than not running it: a test that passes on
    // an input which cannot exercise it is a green light for nothing. So this
    // skips as loudly as a missing archive does, and says which of the two it
    // is.
    if leaf_directory_len(&archive_path()) == 0 {
        skip_banner(
            "directory_cache_is_load_bearing",
            "THIS ARCHIVE HAS NO LEAF DIRECTORIES: every tile is addressed from the root",
            &format!(
                "There is no second read to save, so point {ARCHIVE_ENV} at a build large \
                 enough to have leaves,"
            ),
        );
        return;
    }

    let (source, reads) = CountingSource::new(inner);
    let archive = block_on(BasemapArchive::open(source)).expect("the archive should open");

    let z = archive.max_zoom();
    let (lon, lat) = archive_centre(&archive);
    let x = squallar_geo::lon_to_tile_x(lon, z);
    let y = squallar_geo::lat_to_tile_y(lat, z);

    // First fetch: leaf directory plus tile body. The skip above establishes
    // the archive has leaves at all; this establishes that the seed tile
    // actually sits behind one, rather than in the part of the tree the root
    // directory still addresses directly.
    let before_first = reads.load(Ordering::SeqCst);
    let first = block_on(archive.tile(z, x, y)).expect("the first tile should read");
    assert!(first.is_present(), "the seed tile must be in the archive");
    let first_cost = reads.load(Ordering::SeqCst) - before_first;
    assert!(
        first_cost >= 2,
        "the first fetch should pay for a leaf directory as well as the tile; it cost \
         {first_cost} reads, so this seed tile is addressed from the root and the assertion \
         below would prove nothing"
    );

    // Second fetch, a neighbour in the same leaf: the tile body only.
    let before_second = reads.load(Ordering::SeqCst);
    let second = block_on(archive.tile(z, x + 1, y)).expect("the neighbour tile should read");
    assert!(second.is_present(), "the neighbour must be in the archive");
    let second_cost = reads.load(Ordering::SeqCst) - before_second;
    assert_eq!(
        second_cost, 1,
        "a neighbouring tile should cost one range request; {second_cost} means the leaf \
         directory was re-fetched and the cache is not doing its job"
    );
}

/// The source abstraction is what lets a later downloaded sub-archive be a
/// source the reader prefers rather than a second reader. This holds it to
/// that: the same [`BasemapArchive`] opens over HTTP and over a file, with no
/// per-source code above the [`RangeSource`] impl.
#[test]
fn the_same_reader_opens_over_http_and_over_a_file() {
    let path = archive_path();
    if !path.is_file() {
        no_archive_banner("the_same_reader_opens_over_http_and_over_a_file", &path);
        return;
    }

    let from_file = block_on(BasemapArchive::open(
        FileRangeSource::open(&path).expect("the archive should open"),
    ))
    .expect("the file-backed archive should open");

    // The loopback server serves the archive's opening bytes: enough for the
    // header and root directory, which is what `open` reads. Clamped, because
    // the committed fixture is smaller than this ceiling and a bare slice would
    // panic on it rather than test anything.
    let head = std::fs::read(&path)
        .map(|whole| whole[..whole.len().min(1 << 20)].to_vec())
        .expect("the archive should be readable");
    let server = RangeServer::start(head, 0, Answer::Range);
    let from_http = block_on(BasemapArchive::open(
        HttpRangeSource::new(loopback_client(), &server.url())
            .expect("the loopback URL should parse"),
    ))
    .expect("the http-backed archive should open");

    assert_eq!(from_http.max_zoom(), from_file.max_zoom());
    assert_eq!(from_http.min_zoom(), from_file.min_zoom());
    assert_eq!(from_http.tile_type(), from_file.tile_type());
    assert_eq!(from_http.tile_compression(), from_file.tile_compression());
}

/// A coordinate that is not a tile is its own answer, distinct from both
/// absence and a transport failure.
#[test]
fn a_coordinate_off_the_grid_is_neither_absent_nor_a_transport_failure() {
    let Some(source) =
        open_archive_file("a_coordinate_off_the_grid_is_neither_absent_nor_a_transport_failure")
    else {
        return;
    };

    let archive = block_on(BasemapArchive::open(source)).expect("the archive should open");
    // x = 4 does not exist at zoom 1, where the grid is 2x2.
    let error = block_on(archive.tile(1, 4, 0)).expect_err("an off-grid coordinate is not a tile");

    assert!(
        matches!(error, ArchiveError::Coordinate { z: 1, x: 4, y: 0 }),
        "{error}"
    );
}

// ---------------------------------------------------------------------------
// Shape
// ---------------------------------------------------------------------------

/// The bound `pmtiles` imposes, held where a future change would notice.
///
/// `AsyncPmTilesReader`'s inherent impl is bounded `B: AsyncBackend + Sync +
/// Send` on every target, so a source that is not both cannot be read from —
/// which is the whole reason the wasm arm bridges through `spawn_local` instead
/// of relaxing the bound the way `AsyncTileSource` does.
#[test]
fn the_sources_satisfy_the_bound_the_reader_needs() {
    fn assert_usable<S: super::ArchiveRangeSource>() {}

    assert_usable::<FileRangeSource>();
    assert_usable::<HttpRangeSource>();
}

/// Archive traffic cannot fall back to cleartext, for the same reason tile
/// traffic cannot: `https_only` is set once in `squallar_source::tls` so no
/// call site has to remember it, and this is what holds
/// [`super::archive_client`] to going through it rather than building its own
/// `reqwest::Client`.
#[test]
fn the_archive_client_refuses_cleartext() {
    // Built inside the runtime: `reqwest`'s client wants a reactor in scope.
    let error = block_on(async {
        super::archive_client()
            .get("http://127.0.0.1:1/basemap.pmtiles")
            .send()
            .await
    })
    .expect_err("a cleartext archive URL must not be fetched");

    assert!(
        error.is_builder(),
        "the request failed, but not at the https_only scheme check: {error}"
    );
}

/// The same client does *not* reject `https://`.
#[test]
fn the_archive_client_accepts_https() {
    let error = block_on(async {
        super::archive_client()
            .get("https://127.0.0.1:1/basemap.pmtiles")
            .send()
            .await
    })
    .expect_err("nothing listens on port 1, so the connection must fail");

    assert!(
        !error.is_builder(),
        "an https:// archive URL was rejected before any connection was attempted: {error}"
    );
    assert!(
        error.is_connect(),
        "expected a connection failure, got: {error}"
    );
}
