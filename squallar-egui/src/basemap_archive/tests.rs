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
    ArchiveError, BasemapArchive, FileRangeSource, HttpRangeSource, PartSpan, RANGE_ATTEMPTS,
    RangeError, RangeSource, TileBytes, part_spans,
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
pub(super) fn archive_path() -> PathBuf {
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
pub(super) fn no_archive_banner(test: &str, path: &Path) {
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
pub(crate) fn loopback_client() -> reqwest::Client {
    squallar_radar::tls::init();
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("the loopback client should build")
}

/// Run `future` on a current-thread runtime.
pub(crate) fn block_on<F: Future>(future: F) -> F::Output {
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
///
/// `pub(super)` so the composition's suite counts with the same instrument
/// this one does: a second counter written beside it could disagree about
/// what a read is, and the whole of both suites' evidence is read counts.
pub(super) struct CountingSource<S> {
    inner: S,
    reads: Arc<AtomicUsize>,
}

impl<S> CountingSource<S> {
    pub(super) fn new(inner: S) -> (Self, Arc<AtomicUsize>) {
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

/// Wraps a source and records every `(offset, length)` asked of it.
///
/// What lets a test *discover* a tile's byte range honestly: on an archive
/// with no leaf directories, one `tile()` call after open is exactly one
/// range read — the tile body itself.
struct RecordingSource<S> {
    inner: S,
    log: ReadLog,
}

/// The `(offset, length)` of every read a [`RecordingSource`] has seen.
type ReadLog = Arc<std::sync::Mutex<Vec<(u64, usize)>>>;

impl<S> RecordingSource<S> {
    fn new(inner: S) -> (Self, ReadLog) {
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        (
            Self {
                inner,
                log: Arc::clone(&log),
            },
            log,
        )
    }
}

impl<S: RangeSource + Send + Sync> RangeSource for RecordingSource<S> {
    fn read_range(
        &self,
        offset: u64,
        length: usize,
    ) -> impl Future<Output = Result<Vec<u8>, RangeError>> + Send {
        self.log
            .lock()
            .expect("the read log is not poisoned")
            .push((offset, length));
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

/// How the loopback server answers a known path.
#[derive(Clone, Copy)]
pub(crate) enum Answer {
    /// `200` with a body, i.e. the whole resource — what a host that ignores
    /// `Range` does, and what 2 of 11 real requests did.
    WholeBody,
    /// `206` with the requested slice.
    Range,
    /// `500`.
    ServerError,
}

/// One path's serving plan: `whole_body_first` requests answered `200`, then
/// `then`.
pub(crate) struct PathPlan {
    body: Vec<u8>,
    whole_body_first: usize,
    then: Answer,
}

impl PathPlan {
    /// A well-behaved ranged path.
    pub(crate) fn ranged(body: Vec<u8>) -> Self {
        Self {
            body,
            whole_body_first: 0,
            then: Answer::Range,
        }
    }
}

/// One request the server saw: the path, and the raw `Range` bounds it asked
/// for — start and *inclusive* end, unclamped, exactly as the client spelled
/// them. Raw because the boundary tests assert on what was *asked* (a span
/// ending at a part's last byte), not on what a clamping server chose to
/// serve.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SeenRequest {
    pub(super) path: String,
    pub(super) start: usize,
    pub(super) end: usize,
}

/// A loopback HTTP server holding a set of paths, each with its own plan.
///
/// Path-aware because the source under test now speaks to more than one URL:
/// the probe asks for `<path>.part000`, a parted archive is `.part000`,
/// `.part001`, …, and the monolith is the bare path. A path with no plan
/// answers a clean `404`, which is itself load-bearing: it is what tells the
/// probe an archive has no parts.
pub(crate) struct RangeServer {
    port: u16,
    seen: Arc<std::sync::Mutex<Vec<SeenRequest>>>,
    #[expect(
        dead_code,
        reason = "held to keep the server thread alive for the test"
    )]
    thread: std::thread::JoinHandle<()>,
}

/// The bare archive path every test serves under.
pub(crate) const ARCHIVE_PATH: &str = "/archive.pmtiles";

/// The path of part `index` under [`ARCHIVE_PATH`], per the publish contract.
fn part_path(index: usize) -> String {
    format!("{ARCHIVE_PATH}.part{index:03}")
}

/// `body` split at `part_bytes`, the way the publish side slices an archive:
/// every part exactly `part_bytes` long except the last.
fn split_into_parts(body: &[u8], part_bytes: usize) -> Vec<Vec<u8>> {
    assert!(part_bytes > 0);
    body.chunks(part_bytes).map(<[u8]>::to_vec).collect()
}

impl RangeServer {
    /// A server answering exactly `paths`, and `404` elsewhere.
    pub(crate) fn start(paths: std::collections::HashMap<String, PathPlan>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port should bind");
        let port = listener
            .local_addr()
            .expect("a bound listener has an address")
            .port();
        let seen: Arc<std::sync::Mutex<Vec<SeenRequest>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);

        let thread = std::thread::spawn(move || {
            let mut served: std::collections::HashMap<String, usize> =
                std::collections::HashMap::new();

            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                if serve_one(stream, &paths, &mut served, &log).is_err() {
                    break;
                }
            }
        });

        Self { port, seen, thread }
    }

    /// The pre-split shape: one file at the bare path, no parts anywhere, so
    /// the probe's `404` selects monolith mode.
    pub(crate) fn monolith(body: Vec<u8>, whole_body_first: usize, then: Answer) -> Self {
        Self::start(std::collections::HashMap::from([(
            ARCHIVE_PATH.to_owned(),
            PathPlan {
                body,
                whole_body_first,
                then,
            },
        )]))
    }

    /// The publish format: `body` sliced at `part_bytes` into `.partNNN`
    /// paths, and **nothing at the bare path** — a request there answers
    /// `404`, so a reader that fell back to the monolith would fail loudly
    /// rather than pass by accident.
    pub(crate) fn parted(body: &[u8], part_bytes: usize) -> Self {
        Self::start(
            split_into_parts(body, part_bytes)
                .into_iter()
                .enumerate()
                .map(|(index, part)| (part_path(index), PathPlan::ranged(part)))
                .collect(),
        )
    }

    pub(crate) fn url(&self) -> String {
        format!("http://127.0.0.1:{}{ARCHIVE_PATH}", self.port)
    }

    /// Every request seen so far, in arrival order.
    pub(super) fn seen(&self) -> Vec<SeenRequest> {
        self.seen
            .lock()
            .expect("the request log is not poisoned")
            .clone()
    }

    /// How many requests `path` has received.
    pub(crate) fn requests_to(&self, path: &str) -> usize {
        self.seen()
            .iter()
            .filter(|request| request.path == path)
            .count()
    }
}

/// Read one request, record it, answer it, close.
fn serve_one(
    mut stream: TcpStream,
    paths: &std::collections::HashMap<String, PathPlan>,
    served: &mut std::collections::HashMap<String, usize>,
    log: &std::sync::Mutex<Vec<SeenRequest>>,
) -> std::io::Result<()> {
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
    let path = request
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_owned();

    let Some(plan) = paths.get(&path) else {
        log.lock()
            .expect("the request log is not poisoned")
            .push(SeenRequest {
                path,
                start: 0,
                end: 0,
            });
        stream.write_all(
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )?;
        return stream.flush();
    };

    let (start, end) = parse_range(&request, plan.body.len());
    log.lock()
        .expect("the request log is not poisoned")
        .push(SeenRequest {
            path: path.clone(),
            start,
            end,
        });

    let times_before = served.entry(path).or_insert(0);
    let answer = if *times_before < plan.whole_body_first {
        Answer::WholeBody
    } else {
        plan.then
    };
    *times_before += 1;

    let body = &plan.body;
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
            // Clamped the way a real host clamps: a range running past the
            // end serves what exists. The log above keeps the raw request.
            let from = start.min(body.len());
            let to = end.saturating_add(1).min(body.len());
            let slice = &body[from..to];
            let head = format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {}-{}/{}\r\nContent-Length: \
                 {}\r\nConnection: close\r\n\r\n",
                from,
                to.saturating_sub(1),
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

/// `bytes=N-M` out of a request head: raw start and *inclusive* end, no
/// clamping — serving clamps, the log does not.
fn parse_range(request: &str, len: usize) -> (usize, usize) {
    let Some(spec) = request.lines().find_map(|line| {
        line.strip_prefix("Range: bytes=")
            .or_else(|| line.strip_prefix("range: bytes="))
    }) else {
        return (0, len.saturating_sub(1));
    };

    let spec = spec.trim();
    let mut halves = spec.split('-');
    let start: usize = halves.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    let end: usize = halves
        .next()
        .and_then(|v| v.parse().ok())
        .unwrap_or(len.saturating_sub(1));

    (start, end)
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
    let server = RangeServer::monolith(body.clone(), 1, Answer::Range);
    let source = HttpRangeSource::new(loopback_client(), &server.url())
        .expect("the loopback URL should parse");

    let bytes = block_on(source.read_range(100, 64)).expect("the retry should have succeeded");

    assert_eq!(
        bytes,
        &body[100..164],
        "the retried attempt should return the range that was asked for"
    );
    assert_eq!(
        server.requests_to(ARCHIVE_PATH),
        2,
        "one attempt should have been spent on the 200 and one on the 206"
    );
    assert_eq!(
        server.requests_to(&part_path(0)),
        1,
        "the first read probes for part000 exactly once; its 404 is what selected monolith mode"
    );
}

/// Bounded, because a host that does not do ranges at all will never start —
/// and an unbounded retry against it is an infinite loop wearing a recovery's
/// clothes. The error names the status and the count so the reader can tell the
/// two readings of the symptom apart.
#[test]
fn retries_are_bounded_and_the_error_says_what_happened() {
    let body: Vec<u8> = (0u8..=255).cycle().take(4096).collect();
    let server = RangeServer::monolith(body, usize::MAX, Answer::WholeBody);
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
        server.requests_to(ARCHIVE_PATH),
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
    let server = RangeServer::monolith(Vec::new(), 0, Answer::ServerError);
    let source = HttpRangeSource::new(loopback_client(), &server.url())
        .expect("the loopback URL should parse");

    let error = block_on(source.read_range(0, 64)).expect_err("a 500 should fail the read");

    assert!(
        matches!(error, RangeError::Transport(_)),
        "a 500 should be a transport error, not a range-support verdict: {error}"
    );
    assert_eq!(
        server.requests_to(ARCHIVE_PATH),
        1,
        "a failure status should not be retried"
    );
}

// ---------------------------------------------------------------------------
// Parts: the probe, the arithmetic, the stitch
// ---------------------------------------------------------------------------

/// A byte pattern with no short period, so a stitch that dropped, duplicated
/// or reordered a chunk cannot alias back to the expected bytes: 251 is prime
/// and shares no factor with any part stride these tests use.
fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| ((i * 131) % 251) as u8).collect()
}

/// A parts-mode source over `server` with the stride under test.
fn parted_source(server: &RangeServer, part_bytes: u64) -> HttpRangeSource {
    HttpRangeSource::with_small_parts(loopback_client(), &server.url(), part_bytes)
        .expect("the loopback URL should parse")
}

/// The part index a request path names, if it names one.
fn seen_part_index(path: &str) -> Option<u64> {
    path.rsplit(".part")
        .next()
        .and_then(|digits| digits.parse().ok())
}

/// The one function both transports share is pinned directly: part `k` holds
/// global bytes `[k*stride, (k+1)*stride)`, spans arrive in archive order,
/// and a span never crosses or even touches a part it takes no bytes from.
#[test]
fn part_spans_cut_exactly_at_the_publish_stride() {
    let span = |part, offset, length| PartSpan {
        part,
        offset,
        length,
    };

    // Inside one part: untouched arithmetic, one request.
    assert_eq!(part_spans(10, 100, 1000), vec![span(0, 10, 100)]);
    // Straddling one boundary: two requests whose lengths sum to the read's.
    assert_eq!(
        part_spans(950, 100, 1000),
        vec![span(0, 950, 50), span(1, 0, 50)]
    );
    // Spanning a whole middle part.
    assert_eq!(
        part_spans(500, 2000, 1000),
        vec![span(0, 500, 500), span(1, 0, 1000), span(2, 0, 500)]
    );
    // Ending exactly on a boundary: the next part is NEVER in the plan. On
    // the archive's last boundary that part does not exist, and requesting
    // zero bytes of it would turn a correct read into a 404.
    assert_eq!(part_spans(900, 100, 1000), vec![span(0, 900, 100)]);
    // Starting exactly on a boundary.
    assert_eq!(part_spans(1000, 10, 1000), vec![span(1, 0, 10)]);
    // A zero-length read maps to no requests at all.
    assert_eq!(part_spans(123, 0, 1000), vec![]);
}

/// A read across a boundary is two ranged requests stitched in order, and the
/// server's own log is the witness: the first request drains part000 to its
/// last byte, the next one opens part001 at zero.
#[test]
fn a_read_across_a_part_boundary_stitches_bytes_in_order() {
    let body = pattern(2500);
    let server = RangeServer::parted(&body, 1000);
    let source = parted_source(&server, 1000);

    let bytes = block_on(source.read_range(950, 100)).expect("a straddling read should succeed");
    assert_eq!(
        bytes,
        &body[950..1050],
        "the stitched read must be byte-identical to the same range of the unsplit body"
    );

    let seen = server.seen();
    let tail = seen
        .iter()
        .position(|request| {
            request.path == part_path(0) && request.start == 950 && request.end == 999
        })
        .expect("part000 should have been asked for its tail, up to its very last byte");
    assert_eq!(
        seen.get(tail + 1),
        Some(&SeenRequest {
            path: part_path(1),
            start: 0,
            end: 49,
        }),
        "part001 should have been asked for the remainder, immediately after and from byte zero"
    );

    // A read spanning three parts stitches the middle part whole.
    let bytes = block_on(source.read_range(500, 2000)).expect("a three-part read should succeed");
    assert_eq!(bytes, &body[500..2500]);
}

/// The boundary edges: a read ending exactly on a part boundary, and the
/// final short part — into it, and past the archive's end.
#[test]
fn part_boundary_edges_hold_at_the_last_part_and_at_an_exact_boundary_end() {
    // Parts of 1000, 1000, 500: the last part is short, as the real last
    // part almost always is.
    let body = pattern(2500);
    let server = RangeServer::parted(&body, 1000);
    let source = parted_source(&server, 1000);

    // Ends exactly on the part000/part001 boundary.
    let bytes = block_on(source.read_range(900, 100)).expect("a boundary-ending read succeeds");
    assert_eq!(bytes, &body[900..1000]);
    assert_eq!(
        server.requests_to(&part_path(1)),
        0,
        "a read ending on the boundary must not touch the next part at all"
    );

    // Entirely inside the final short part.
    let bytes = block_on(source.read_range(2100, 100)).expect("the short part serves reads");
    assert_eq!(bytes, &body[2100..2200]);

    // Straddling into the final short part.
    let bytes = block_on(source.read_range(1990, 30)).expect("a straddle into the last part");
    assert_eq!(bytes, &body[1990..2020]);

    // Running past the archive's end: up to `length`, exactly as
    // `FileRangeSource` clamps, because `read_range`'s contract is the same
    // for every source.
    let bytes = block_on(source.read_range(2400, 500)).expect("a read past the end clamps");
    assert_eq!(bytes, &body[2400..2500]);
}

/// A parted server whose `part000` is `part000_bytes` long instead of the
/// `stride` the publish contract promises, with the later parts holding the
/// bytes a correct publish would put in them.
///
/// The mid-sequence short part: a stride the publisher and [`PART_BYTES`]
/// disagree on, or an upload that stopped early. Its shape is what the
/// reader's `offset / stride` arithmetic has no way to see.
fn server_with_a_short_first_part(body: &[u8], stride: usize, part000_bytes: usize) -> RangeServer {
    let mut paths = std::collections::HashMap::new();
    for (index, part) in split_into_parts(body, stride).into_iter().enumerate() {
        let part = if index == 0 {
            part[..part000_bytes].to_vec()
        } else {
            part
        };
        paths.insert(part_path(index), PathPlan::ranged(part));
    }
    RangeServer::start(paths)
}

/// **A part that is short in the middle of the sequence is an error, never a
/// short `Ok`.**
///
/// The contract that only the final part is short is the publisher's word and
/// nothing in the bytes carries it, so the reader checks it rather than
/// assuming it. Before the check, `read_range(950, 100)` over a `part000`
/// holding 900 of its 1000 bytes answered `Ok` with **zero** bytes, and
/// `read_range(0, 2500)` answered `Ok` with 900 of 2500 — the whole of
/// `part001` and `part002` silently dropped though both serve. Every terminal
/// consumer does check its own lengths, so that landed as "the archive is
/// truncated" rather than as the publish fault it is — and
/// [`super::block_cache::BlockCachedSource`] writes the short answer to disk,
/// which makes one bad publish durable for the generation.
#[test]
fn a_short_mid_sequence_part_is_an_error_rather_than_a_short_read() {
    let body = pattern(2500);
    let server = server_with_a_short_first_part(&body, 1000, 900);
    let source = parted_source(&server, 1000);

    // Straddling the boundary out of the short part: the 50 bytes at
    // `part001[0..50]` are there and the old code answered with none of them.
    let error = block_on(source.read_range(950, 100))
        .expect_err("a part that ran short with a later part present is not the archive ending");
    assert!(
        matches!(
            error,
            RangeError::ShortPart {
                part: 0,
                got: 0,
                wanted: 50,
                stride: 1000,
            }
        ),
        "the error should name the part, what it answered and the stride it broke: {error:?}"
    );
    assert!(
        error.to_string().contains("part001"),
        "the message should name the later part that proves the archive did not end: {error}"
    );

    // A read wholly inside the short part's own hole, which is one span and
    // so never planned a later part: still not the archive ending.
    let error = block_on(source.read_range(500, 500))
        .expect_err("a single-span read out of a short mid-sequence part errors too");
    assert!(
        matches!(error, RangeError::ShortPart { part: 0, .. }),
        "{error:?}"
    );

    // And the whole-archive read that dropped 1600 bytes in silence.
    let error = block_on(source.read_range(0, 2500))
        .expect_err("the whole-archive read must not answer 900 of 2500 bytes as success");
    assert!(
        matches!(error, RangeError::ShortPart { part: 0, .. }),
        "{error:?}"
    );

    // A read that never touches the short part is unaffected: the check costs
    // correct reads nothing.
    let bytes = block_on(source.read_range(1000, 100)).expect("part001 is whole and reads");
    assert_eq!(bytes, &body[1000..1100]);
}

/// The other half of the same branch: the archive really ending inside its
/// final part is still a clamped `Ok`, and the verdict costs **one** request
/// for the source's whole life.
///
/// The over-correction guard. A check that turned every end-of-archive read
/// into an error would pass the test above and break every tail read there
/// is — [`super::block_cache`]'s last block among them.
#[test]
fn the_archive_ending_in_its_last_part_still_clamps_rather_than_erroring() {
    // Parts of 1000, 1000, 500: the last part is short, as a real one is.
    let body = pattern(2500);
    let server = RangeServer::parted(&body, 1000);
    let source = parted_source(&server, 1000);

    let bytes = block_on(source.read_range(2400, 500)).expect("a read past the end clamps");
    assert_eq!(bytes, &body[2400..2500]);
    assert_eq!(
        server.requests_to(&part_path(3)),
        1,
        "the end verdict is taken by asking whether the next part exists, exactly once"
    );

    // Every later short read reads the held verdict rather than the wire.
    for _ in 0..3 {
        let bytes = block_on(source.read_range(2400, 500)).expect("the clamp holds");
        assert_eq!(bytes, &body[2400..2500]);
    }
    assert_eq!(
        server.requests_to(&part_path(3)),
        1,
        "the verdict is held for the source's lifetime, not re-asked per short read"
    );

    // A short read straddling into the final part clamps the same way.
    let bytes = block_on(source.read_range(1990, 600)).expect("a straddle past the end clamps");
    assert_eq!(bytes, &body[1990..2500]);
}

/// A source that answers every read one byte short, behind the real reader.
///
/// The refutation's own gate: the claim that a short read cannot reach a
/// decoder as plausible data rests on `pmtiles`' `AsyncBackend::read_exact`
/// default, which [`super::RangeBackend`] deliberately does not override.
/// Override it — or swap the reader for one that calls `read` — and this
/// reddens instead of a truncated directory or tile body being decoded.
struct ShortByOneSource {
    inner: FileRangeSource,
}

impl RangeSource for ShortByOneSource {
    fn read_range(
        &self,
        offset: u64,
        length: usize,
    ) -> impl Future<Output = Result<Vec<u8>, RangeError>> + Send {
        let read = self.inner.read_range(offset, length);
        async move {
            let mut bytes = read.await?;
            bytes.pop();
            Ok(bytes)
        }
    }
}

/// A source whose every answer is truncated to `cap` bytes.
struct CappedSource {
    inner: FileRangeSource,
    cap: usize,
}

impl RangeSource for CappedSource {
    fn read_range(
        &self,
        offset: u64,
        length: usize,
    ) -> impl Future<Output = Result<Vec<u8>, RangeError>> + Send {
        let cap = self.cap;
        let read = self.inner.read_range(offset, length);
        async move {
            let mut bytes = read.await?;
            bytes.truncate(cap);
            Ok(bytes)
        }
    }
}

/// **An archive truncated inside its root directory fails the open; it never
/// aborts the task.**
///
/// `pmtiles` opens with a plain "up to `length`" read and slices the header
/// *and* the root directory out of the one answer without bounds-checking the
/// second, so a source answering between the header's 127 bytes and the end
/// of the root directory panics in `bytes` rather than erroring. Measured on
/// the committed fixture before the guard: 0 and 100 bytes failed cleanly,
/// 127 through 637 aborted the task, 638 opened. A truncated segment or a
/// `part000` that stopped early is such a source, and
/// `a_truncated_local_segment_is_skipped_and_the_tile_still_arrives`
/// truncates to 7 bytes, which lands below the window and misses it.
#[test]
fn an_archive_truncated_inside_its_root_directory_fails_the_open_rather_than_panicking() {
    const TEST: &str =
        "an_archive_truncated_inside_its_root_directory_fails_the_open_rather_than_panicking";
    if open_archive_file(TEST).is_none() {
        return;
    }

    // Every cap from the header's last byte to just inside the root
    // directory's end: the whole window, not a sample of it.
    for cap in [127usize, 128, 200, 400, 637] {
        let inner = open_archive_file(TEST).expect("the fixture was there a moment ago");
        let opened = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            block_on(BasemapArchive::open(CappedSource { inner, cap }))
        }));
        match opened {
            Err(_) => panic!(
                "an archive answering {cap} bytes aborted the task; a truncated archive must \
                 fail the open, not unwind through it"
            ),
            Ok(Ok(_)) => panic!(
                "an archive answering {cap} bytes opened, which is fewer than its own root \
                 directory needs"
            ),
            Ok(Err(error)) => assert!(
                matches!(error, ArchiveError::Open(_)),
                "the truncation surfaces as a failed open: {error}"
            ),
        }
    }

    // And the first cap that holds the whole prelude still opens, so the
    // guard is a bound rather than a blanket refusal.
    let inner = open_archive_file(TEST).expect("the fixture was there a moment ago");
    block_on(BasemapArchive::open(CappedSource { inner, cap: 638 }))
        .expect("an answer holding the header and the whole root directory opens");
}

/// **A short read is never decoded.** The reader asks for structures whose
/// lengths it knows, so one byte missing is an error at the seam rather than
/// a directory or a tile body handed to a decoder a byte light.
#[test]
fn a_source_answering_one_byte_short_fails_rather_than_decoding_the_truncation() {
    const TEST: &str =
        "a_source_answering_one_byte_short_fails_rather_than_decoding_the_truncation";
    let Some(inner) = open_archive_file(TEST) else {
        return;
    };

    // The opening read is `pmtiles`' one deliberate "up to" read — a byte
    // short of 16 KiB still holds the whole prelude, so the open succeeds,
    // and that is correct rather than a hole.
    block_on(BasemapArchive::open(ShortByOneSource { inner }))
        .expect("a byte short of the opening read still holds the header and root directory");

    // Every read after it is held to a length the archive itself declared, so
    // a tile body a byte short is an error rather than bytes handed on.
    let Some(inner) = open_archive_file(TEST) else {
        return;
    };
    let archive = block_on(BasemapArchive::open(ShortAfterOpenSource { inner }))
        .expect("the opening read is served whole, so the archive opens");
    let z = archive.max_zoom();
    let (lon, lat) = archive_centre(&archive);
    let (x, y) = (
        squallar_geo::lon_to_tile_x(lon, z),
        squallar_geo::lat_to_tile_y(lat, z),
    );

    // `warm_tile` and not `tile`, deliberately: `tile` decompresses, and a
    // gzip stream a byte short fails in the DECOMPRESSOR whether or not the
    // length was ever checked — so asserting on it would pass with
    // `read_exact` overridden away and gate nothing. `warm_tile` drops the
    // bytes undecompressed, which leaves the length check as the only thing
    // that can turn this read into an error.
    let error = block_on(archive.warm_tile(z, x, y))
        .expect_err("a tile body a byte short is a failure, never truncated bytes");
    assert!(
        matches!(error, ArchiveError::Tile(_)),
        "the short body surfaces as a tile failure: {error}"
    );

    // And the decompressing path is an error too, for whichever of the two
    // reasons comes first.
    assert!(
        block_on(archive.tile(z, x, y)).is_err(),
        "the decompressing read of a short body is a failure as well"
    );
}

/// [`ShortByOneSource`] that serves the read at offset zero whole, so the
/// archive opens and only the reads *after* the open are truncated.
///
/// Keyed on the offset rather than on a read count, because a count would
/// silently start truncating the wrong read on an archive whose open costs a
/// different number of them.
struct ShortAfterOpenSource {
    inner: FileRangeSource,
}

impl RangeSource for ShortAfterOpenSource {
    fn read_range(
        &self,
        offset: u64,
        length: usize,
    ) -> impl Future<Output = Result<Vec<u8>, RangeError>> + Send {
        let read = self.inner.read_range(offset, length);
        async move {
            let mut bytes = read.await?;
            if offset != 0 {
                bytes.pop();
            }
            Ok(bytes)
        }
    }
}

/// **The differential, over the whole fixture.** Every coordinate the
/// fixture's bounding box holds, at every zoom it declares, read from the
/// monolith and from a 7-part split of the same bytes — and the present
/// count is pinned to the header's own `n_addressed_tiles`, so this cannot
/// quietly become a sample.
#[test]
fn every_fixture_tile_is_byte_identical_from_parts_and_from_the_monolith() {
    const TEST: &str = "every_fixture_tile_is_byte_identical_from_parts_and_from_the_monolith";
    let path = archive_path();
    if !path.is_file() {
        no_archive_banner(TEST, &path);
        return;
    }
    let bytes = std::fs::read(&path).expect("the archive should be readable");
    if bytes.len() as u64 != MONACO_BYTES {
        skip_banner(
            TEST,
            "the archive under test is not the committed Monaco fixture",
            &format!(
                "The exhaustive walk and its addressed-tiles pin are sized for the fixture, so \
                 unset {ARCHIVE_ENV},"
            ),
        );
        return;
    }

    let monolith = block_on(BasemapArchive::open(
        FileRangeSource::open(&path).expect("the archive should open"),
    ))
    .expect("the monolith archive should open");

    // 65,536-byte parts cut the 419,355-byte fixture into 7, the last one
    // short — six boundaries scattered through the directory and tile data.
    const PART: u64 = 65_536;
    let server = RangeServer::parted(&bytes, PART as usize);
    let parted = block_on(BasemapArchive::open(parted_source(&server, PART)))
        .expect("the parted archive should open");

    let header = monolith.header();
    let (min_lon, max_lon) = (header.min_longitude, header.max_longitude);
    let (min_lat, max_lat) = (header.min_latitude, header.max_latitude);
    let mut present = 0_u64;
    let mut compared = 0_u64;

    for z in monolith.min_zoom()..=monolith.max_zoom() {
        let x_range =
            squallar_geo::lon_to_tile_x(min_lon, z)..=squallar_geo::lon_to_tile_x(max_lon, z);
        // Tile rows grow southward, so the *maximum* latitude is the first row.
        let y_range =
            squallar_geo::lat_to_tile_y(max_lat, z)..=squallar_geo::lat_to_tile_y(min_lat, z);

        for x in x_range {
            for y in y_range.clone() {
                let from_monolith =
                    block_on(monolith.tile(z, x, y)).expect("the monolith read should succeed");
                let from_parts =
                    block_on(parted.tile(z, x, y)).expect("the parts read should succeed");

                assert_eq!(
                    from_parts, from_monolith,
                    "{z}/{x}/{y} must be byte-identical from the parts and from the monolith"
                );
                compared += 1;
                if from_monolith.is_present() {
                    present += 1;
                }
            }
        }
    }

    assert_eq!(
        present, MONACO_ADDRESSED_TILES,
        "the bounding-box walk found {present} of the header's {MONACO_ADDRESSED_TILES} \
         addressed tiles over {compared} coordinates; a shortfall means this differential ran \
         over a subset while reading as \"every tile\""
    );

    // And the walk exercised the stitch, not just the arithmetic: some read
    // drained a part to its last byte and continued into the next from zero.
    let seen = server.seen();
    let stitched = seen.windows(2).any(|pair| {
        match (
            seen_part_index(&pair[0].path),
            seen_part_index(&pair[1].path),
        ) {
            (Some(first), Some(second)) => {
                second == first + 1 && pair[0].end as u64 == PART - 1 && pair[1].start == 0
            }
            _ => false,
        }
    });
    assert!(
        stitched,
        "no read straddled any of the six part boundaries, so the differential never exercised \
         the stitch; choose a part size that lands a boundary inside a tile"
    );
}

/// The brief's deliberately hostile construction: a part stride chosen so the
/// boundary **bisects one known tile's byte range**, discovered honestly by
/// recording the one range read that tile costs on a leafless archive. The
/// tile must come back byte-identical from its two stitched halves.
#[test]
fn a_part_boundary_bisecting_a_tile_reproduces_it_byte_for_byte() {
    const TEST: &str = "a_part_boundary_bisecting_a_tile_reproduces_it_byte_for_byte";
    let path = archive_path();
    if !path.is_file() {
        no_archive_banner(TEST, &path);
        return;
    }

    // Discover the tile's byte range: after open, one `tile()` call's LAST
    // read is the tile body itself (its only read, on an archive with no
    // leaf directories).
    let (recording, log) = RecordingSource::new(
        FileRangeSource::open(&path).expect("an existing archive file should open"),
    );
    let reference =
        block_on(BasemapArchive::open(recording)).expect("the recording archive should open");
    let z = reference.max_zoom();
    let (lon, lat) = archive_centre(&reference);
    let x = squallar_geo::lon_to_tile_x(lon, z);
    let y = squallar_geo::lat_to_tile_y(lat, z);

    log.lock().expect("the read log is not poisoned").clear();
    let expected = block_on(reference.tile(z, x, y)).expect("the centre tile should read");
    assert!(
        expected.is_present(),
        "the archive's centre tile must be in it"
    );
    let (tile_offset, tile_len) = *log
        .lock()
        .expect("the read log is not poisoned")
        .last()
        .expect("reading a tile reads at least the tile body");
    assert!(tile_len >= 2, "a {tile_len}-byte tile cannot be bisected");

    // A stride that puts the part000/part001 boundary in the middle of that
    // tile's bytes.
    let stride = tile_offset + tile_len as u64 / 2;
    let bytes = std::fs::read(&path).expect("the archive should be readable");
    let server = RangeServer::parted(&bytes, stride as usize);
    let parted = block_on(BasemapArchive::open(parted_source(&server, stride)))
        .expect("the bisected archive should open");

    let actual = block_on(parted.tile(z, x, y)).expect("the bisected tile should read");
    assert_eq!(
        actual, expected,
        "{z}/{x}/{y} is bisected by the part boundary and must decode byte-identically from \
         the stitched halves"
    );

    // The two halves really were fetched as halves, adjacent and in order.
    let first_half = SeenRequest {
        path: part_path(0),
        start: usize::try_from(tile_offset).expect("the fixture is far smaller than usize"),
        end: usize::try_from(stride - 1).expect("the fixture is far smaller than usize"),
    };
    let second_half = SeenRequest {
        path: part_path(1),
        start: 0,
        end: usize::try_from(tile_offset + tile_len as u64 - 1 - stride)
            .expect("the fixture is far smaller than usize"),
    };
    let seen = server.seen();
    let halves = seen
        .windows(2)
        .any(|pair| pair[0] == first_half && pair[1] == second_half);
    assert!(
        halves,
        "the tile's two halves should appear as adjacent requests {first_half:?} then \
         {second_half:?}; the log saw {seen:?}"
    );
}

// ---------------------------------------------------------------------------
// Parts: the probe
// ---------------------------------------------------------------------------

/// `part000` present selects part mode — and part mode never touches the bare
/// URL, because new generations publish **no monolith** there.
#[test]
fn a_present_part000_selects_part_mode_and_never_touches_the_bare_url() {
    const TEST: &str = "a_present_part000_selects_part_mode_and_never_touches_the_bare_url";
    let path = archive_path();
    if !path.is_file() {
        no_archive_banner(TEST, &path);
        return;
    }
    let bytes = std::fs::read(&path).expect("the archive should be readable");

    let server = RangeServer::parted(&bytes, 65_536);
    let archive = block_on(BasemapArchive::open(parted_source(&server, 65_536)))
        .expect("the parts alone must be enough to open the archive");

    let z = archive.max_zoom();
    let (lon, lat) = archive_centre(&archive);
    let tile = block_on(archive.tile(
        z,
        squallar_geo::lon_to_tile_x(lon, z),
        squallar_geo::lat_to_tile_y(lat, z),
    ))
    .expect("the centre tile should read from the parts");
    assert!(tile.is_present(), "the archive's centre tile must be in it");

    assert_eq!(
        server.requests_to(ARCHIVE_PATH),
        0,
        "part mode must never request the bare URL: a parts-only generation serves nothing there"
    );
    let probes = server
        .seen()
        .iter()
        .filter(|request| request.path == part_path(0) && (request.start, request.end) == (0, 15))
        .count();
    assert_eq!(
        probes, 1,
        "one probe, at open, held for the source's lifetime"
    );
}

/// A *clean* 404 for `part000` selects monolith mode — the compatibility path
/// for generations published before the split, for exactly one probe's cost.
#[test]
fn an_absent_part000_selects_monolith_mode_after_one_probe() {
    const TEST: &str = "an_absent_part000_selects_monolith_mode_after_one_probe";
    let path = archive_path();
    if !path.is_file() {
        no_archive_banner(TEST, &path);
        return;
    }
    let bytes = std::fs::read(&path).expect("the archive should be readable");

    let server = RangeServer::monolith(bytes, 0, Answer::Range);
    let source = HttpRangeSource::new(loopback_client(), &server.url())
        .expect("the loopback URL should parse");
    let archive = block_on(BasemapArchive::open(source)).expect("the monolith archive should open");

    let z = archive.max_zoom();
    let (lon, lat) = archive_centre(&archive);
    let tile = block_on(archive.tile(
        z,
        squallar_geo::lon_to_tile_x(lon, z),
        squallar_geo::lat_to_tile_y(lat, z),
    ))
    .expect("the centre tile should read from the monolith");
    assert!(tile.is_present(), "the archive's centre tile must be in it");

    assert_eq!(
        server.requests_to(&part_path(0)),
        1,
        "exactly one probe; its clean 404 selects monolith mode for the source's lifetime"
    );
    assert!(
        server.requests_to(ARCHIVE_PATH) >= 2,
        "the header and the tile should both have been read from the bare URL"
    );
}

/// **A probe failure is a fault, not an absence.** The bare URL here holds a
/// perfectly good archive, so a reader that shrugged the 500 off as "no
/// parts" would open successfully — this test failing on `expect_err` is that
/// silent fallback happening.
#[test]
fn a_probe_5xx_fails_the_open_rather_than_silently_selecting_monolith() {
    const TEST: &str = "a_probe_5xx_fails_the_open_rather_than_silently_selecting_monolith";
    let path = archive_path();
    if !path.is_file() {
        no_archive_banner(TEST, &path);
        return;
    }
    let bytes = std::fs::read(&path).expect("the archive should be readable");

    let server = RangeServer::start(std::collections::HashMap::from([
        (
            ARCHIVE_PATH.to_owned(),
            PathPlan {
                body: bytes,
                whole_body_first: 0,
                then: Answer::Range,
            },
        ),
        (
            part_path(0),
            PathPlan {
                body: Vec::new(),
                whole_body_first: 0,
                then: Answer::ServerError,
            },
        ),
    ]));

    let source = HttpRangeSource::new(loopback_client(), &server.url())
        .expect("the loopback URL should parse");
    let error = match block_on(BasemapArchive::open(source)) {
        Ok(_) => panic!(
            "the open succeeded, which means the 5xx probe was shrugged off as \"no parts\" and \
             monolith mode was silently selected"
        ),
        Err(error) => error,
    };

    assert!(
        matches!(error, ArchiveError::Open(_)),
        "the fault surfaces through the same path a failed open uses: {error}"
    );
    assert!(
        error.to_string().contains("probe"),
        "the error should say the probe is what failed: {error}"
    );
    assert_eq!(
        server.requests_to(&part_path(0)),
        RANGE_ATTEMPTS as usize,
        "the probe retries per the range discipline before surfacing the fault"
    );
    assert_eq!(
        server.requests_to(ARCHIVE_PATH),
        0,
        "the bare URL held a working archive, and reaching for it is exactly the silent \
         fallback this test forbids"
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
    let server = RangeServer::monolith(head, 0, Answer::Range);
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
