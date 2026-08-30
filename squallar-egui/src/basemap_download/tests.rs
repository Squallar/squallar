//! Tests for [`super`].
//!
//! The loopback pattern `tile_source/tests.rs` established, serving real
//! ranges: a real HTTP/1.1 server on 127.0.0.1 answers `206` with an exact
//! `Content-Range` for the archive side, and a second one implements the
//! service worker's offline-store contract for the web store side — so the
//! engine is exercised over the same wire shapes it meets in production, on
//! the committed Monaco fixture.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::Context;

use super::{
    AreaSpec, AreaStatus, BasemapDownload, DownloadOutcome, DownloadedArea, FsSegmentStore,
    HttpSegmentStore, OFFLINE_BASE_PATH, SEGMENT_BYTES, SegmentStore, TileSpan, area_status,
    area_tiles, coalesce, plan_area, valid_area_id,
};
use crate::basemap_archive::{FileRangeSource, HttpRangeSource, RangeError, RangeSource};
use crate::pmt_index::{PmtIndex, zxy_to_tile_id};
use squallar_units::DataSize;

// ---------------------------------------------------------------------------
// Fixture and helpers
// ---------------------------------------------------------------------------

/// The committed Monaco fixture — 246 addressed tiles, 157 entries, 108
/// distinct blobs, so both dedup mechanisms are live.
const MONACO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/monaco.pmtiles");

/// A generous bbox around Monaco. The engine enumerates from the bbox and
/// counts what the archive does not hold as absent, so this needs to cover
/// the fixture, not trace it.
fn monaco_area(area_id: &str, max_zoom: u8) -> AreaSpec {
    AreaSpec {
        area_id: area_id.to_owned(),
        west: 7.35,
        south: 43.70,
        east: 7.50,
        north: 43.78,
        max_zoom,
    }
}

/// The fixture's bytes, or `None` with the same shouted skip the sibling
/// suites use — straight at stderr, because libtest swallows `eprintln!` on a
/// passing test.
fn monaco_bytes(test: &str) -> Option<Vec<u8>> {
    match std::fs::read(MONACO) {
        Ok(bytes) => Some(bytes),
        Err(_) => {
            let mut stderr = std::io::stderr().lock();
            let _ = writeln!(
                stderr,
                "\n\
                 ###########################################################################\n\
                 ## SKIPPED, NOT PASSED: {test}\n\
                 ##   no PMTiles archive at {MONACO}\n\
                 ##   this test asserted NOTHING. Restore the committed fixture\n\
                 ##   before reading this suite as covering the download engine.\n\
                 ###########################################################################"
            );
            None
        }
    }
}

/// Run `future` on a current-thread runtime.
fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime should build")
        .block_on(future)
}

/// A per-test directory under the OS temp dir, removed on drop — the
/// `squallar::kv` tests' shape.
struct TempDir(PathBuf);

impl TempDir {
    fn new(test: &str) -> Self {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "squallar-basemap-download-{}-{test}",
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

const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30);

/// Poll the engine until its outcome lands.
fn wait_outcome(engine: &BasemapDownload, what: &str) -> DownloadOutcome {
    let start = Instant::now();
    loop {
        if let Some(outcome) = engine.outcome() {
            return outcome;
        }
        assert!(
            start.elapsed() < DOWNLOAD_TIMEOUT,
            "{what}: the download did not finish in {DOWNLOAD_TIMEOUT:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The `.pmtiles` files in `dir`, by name.
fn published_files(dir: &std::path::Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .collect();
    names.sort();
    names
}

// ---------------------------------------------------------------------------
// Source wrappers
// ---------------------------------------------------------------------------

/// Counts every range read reaching the wrapped source.
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
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.read_range(offset, length)
    }
}

/// Serves the first `budget` reads, then fails every one after — the network
/// going away mid-flight.
struct FailingSource {
    inner: FileRangeSource,
    budget: AtomicI64,
}

impl FailingSource {
    fn new(inner: FileRangeSource, budget: i64) -> Self {
        Self {
            inner,
            budget: AtomicI64::new(budget),
        }
    }
}

impl RangeSource for FailingSource {
    fn read_range(
        &self,
        offset: u64,
        length: usize,
    ) -> impl Future<Output = Result<Vec<u8>, RangeError>> + Send {
        let alive = self.budget.fetch_sub(1, Ordering::SeqCst) > 0;
        let read = alive.then(|| self.inner.read_range(offset, length));
        async move {
            match read {
                Some(read) => read.await,
                None => Err(RangeError::Transport("the network went away".to_owned())),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The loopback servers
// ---------------------------------------------------------------------------

/// One parsed request: method, path, `Range` bounds if any, body.
struct Request {
    method: String,
    path: String,
    range: Option<(u64, u64)>,
    body: Vec<u8>,
}

/// Read one full HTTP/1.1 request off `stream` — headers, then exactly
/// `Content-Length` body bytes.
fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let split = loop {
        if let Some(at) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
    };

    let head = String::from_utf8_lossy(&buffer[..split]).into_owned();
    let mut lines = head.lines();
    let mut request_line = lines.next()?.split_whitespace();
    let method = request_line.next()?.to_owned();
    let path = request_line.next()?.to_owned();

    let mut range = None;
    let mut content_length = 0usize;
    // reqwest writes standard header names lowercase; curl and friends write
    // them titlecase — the sibling suite's parser accepts both, so this one
    // does too.
    for line in lines {
        if let Some(bounds) = line
            .strip_prefix("Range: bytes=")
            .or_else(|| line.strip_prefix("range: bytes="))
            && let Some((from, to)) = bounds.split_once('-')
            && let (Ok(from), Ok(to)) = (from.parse(), to.parse())
        {
            range = Some((from, to));
        }
        if let Some(length) = line
            .strip_prefix("Content-Length: ")
            .or_else(|| line.strip_prefix("content-length: "))
            && let Ok(length) = length.parse()
        {
            content_length = length;
        }
    }

    let mut body = buffer[split..].to_vec();
    while body.len() < content_length {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(content_length);

    Some(Request {
        method,
        path,
        range,
        body,
    })
}

fn respond(stream: &mut TcpStream, status: u16, reason: &str, extra: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

/// A loopback HTTP server: one thread accepting, one per connection, a shared
/// request log, stopped on drop.
struct Loopback {
    base_url: String,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    accept: Option<std::thread::JoinHandle<()>>,
}

impl Loopback {
    fn start<F>(handler: F) -> Self
    where
        F: Fn(&Request, &mut TcpStream) + Send + Sync + 'static,
    {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind a loopback port");
        let addr = listener.local_addr().expect("read back the bound address");
        listener
            .set_nonblocking(true)
            .expect("put the listener in non-blocking mode");

        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let handler = Arc::new(handler);

        let accept = std::thread::spawn({
            let requests = Arc::clone(&requests);
            let stop = Arc::clone(&stop);
            move || {
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let requests = Arc::clone(&requests);
                            let handler = Arc::clone(&handler);
                            std::thread::spawn(move || {
                                let _ = stream.set_nonblocking(false);
                                if let Some(request) = read_request(&mut stream) {
                                    requests
                                        .lock()
                                        .expect("the request log is not poisoned")
                                        .push(format!("{} {}", request.method, request.path));
                                    handler(&request, &mut stream);
                                }
                            });
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(2));
                        }
                        Err(_) => break,
                    }
                }
            }
        });

        Self {
            base_url: format!("http://{addr}"),
            requests,
            stop,
            accept: Some(accept),
        }
    }

    fn requests(&self) -> Vec<String> {
        self.requests
            .lock()
            .expect("the request log is not poisoned")
            .clone()
    }
}

impl Drop for Loopback {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
    }
}

/// The archive side: `206` + exact `Content-Range` for a ranged GET of
/// `/archive.pmtiles`, `404` for everything else — which also answers the
/// part-layout probe with the clean absence that selects monolith mode.
fn archive_server(body: Vec<u8>) -> Loopback {
    let body = Arc::new(body);
    Loopback::start(move |request, stream| {
        if request.path != "/archive.pmtiles" {
            respond(stream, 404, "Not Found", "", b"no such object");
            return;
        }
        match request.range {
            Some((from, to)) => {
                let len = body.len() as u64;
                let from_at = from.min(len) as usize;
                let to_at = (to + 1).min(len) as usize;
                let slice = &body[from_at..to_at];
                let extra = format!(
                    "Content-Range: bytes {from}-{}/{len}\r\n",
                    from + slice.len() as u64 - 1
                );
                respond(stream, 206, "Partial Content", &extra, slice);
            }
            None => respond(stream, 200, "OK", "", &body),
        }
    })
}

/// The store side: the service worker contract, held in memory. `PUT` stores,
/// `GET __list__` answers `{url, bytes}` rows, `DELETE` removes — the same
/// three verbs `squallar-web/sw.js` will route.
struct StoreServer {
    server: Loopback,
    stored: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
}

impl StoreServer {
    fn start() -> Self {
        let stored: Arc<Mutex<BTreeMap<String, Vec<u8>>>> = Arc::new(Mutex::new(BTreeMap::new()));
        let server = Loopback::start({
            let stored = Arc::clone(&stored);
            move |request, stream| {
                let prefix = format!("/{OFFLINE_BASE_PATH}/");
                let Some(tail) = request.path.strip_prefix(&prefix) else {
                    respond(stream, 404, "Not Found", "", b"outside the offline base");
                    return;
                };
                let mut stored = stored.lock().expect("the store map is not poisoned");
                match request.method.as_str() {
                    "PUT" => {
                        stored.insert(tail.to_owned(), request.body.clone());
                        respond(stream, 200, "OK", "", b"");
                    }
                    "GET" if tail == "__list__" => {
                        let rows: Vec<String> = stored
                            .iter()
                            .map(|(key, bytes)| {
                                format!("{{\"url\":\"{prefix}{key}\",\"bytes\":{}}}", bytes.len())
                            })
                            .collect();
                        let body = format!("[{}]", rows.join(","));
                        respond(stream, 200, "OK", "", body.as_bytes());
                    }
                    "GET" => match stored.get(tail) {
                        Some(bytes) => respond(stream, 200, "OK", "", bytes),
                        None => respond(stream, 404, "Not Found", "", b""),
                    },
                    "DELETE" => {
                        if tail.ends_with('/') {
                            stored.retain(|key, _| !key.starts_with(tail));
                        } else {
                            stored.remove(tail);
                        }
                        respond(stream, 200, "OK", "", b"");
                    }
                    _ => respond(stream, 405, "Method Not Allowed", "", b""),
                }
            }
        });
        Self { server, stored }
    }

    fn stored(&self) -> BTreeMap<String, Vec<u8>> {
        self.stored
            .lock()
            .expect("the store map is not poisoned")
            .clone()
    }
}

/// A client that can reach the cleartext loopback servers.
fn loopback_client() -> reqwest::Client {
    squallar_radar::tls::init();
    reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("the loopback client should build")
}

/// Open the fixture as a [`PmtIndex`] over the file directly.
fn open_index() -> PmtIndex<FileRangeSource> {
    block_on(PmtIndex::open(
        FileRangeSource::open(std::path::Path::new(MONACO)).expect("the fixture opens"),
    ))
    .expect("the fixture is a v3 archive")
}

/// The distinct spans of `segment`, sorted by offset — what a fresh build of
/// it must fetch.
fn segment_spans(segment: &super::PlannedSegment) -> Vec<TileSpan> {
    let mut spans: Vec<TileSpan> = segment
        .tiles
        .iter()
        .map(|tile| tile.span)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    spans.sort_unstable_by_key(|span| span.offset);
    spans
}

/// Reads a [`PmtIndex::open`] costs on the Monaco fixture: the header and the
/// root directory (it has no leaves), plus one more for the metadata copy.
const OPEN_READS: usize = 3;

/// A small cap that forces a multi-segment plan out of the 419 KB fixture.
const SMALL_CAP: u64 = 64_000;

// ---------------------------------------------------------------------------
// Pure pieces
// ---------------------------------------------------------------------------

#[test]
fn coalesce_joins_adjacent_and_near_spans_and_splits_far_ones() {
    let span = |offset, length| TileSpan { offset, length };
    // Adjacent, near (gap <= COALESCE_GAP_BYTES), then far — measured from the
    // merged run's end at 160, because the gap is to the growing range, not to
    // the previous span.
    let far = 160 + super::COALESCE_GAP_BYTES + 1;
    let ranges = coalesce(&[span(0, 100), span(100, 50), span(150, 10), span(far, 5)]);
    assert_eq!(
        ranges.len(),
        2,
        "three near spans coalesce, the far one splits"
    );
    assert_eq!(ranges[0].start, 0);
    assert_eq!(ranges[0].length, 160);
    assert_eq!(ranges[0].spans.len(), 3);
    assert_eq!(ranges[1].start, far);
    assert_eq!(ranges[1].length, 5);
}

#[test]
fn coalesce_reads_through_a_small_gap_once() {
    let span = |offset, length| TileSpan { offset, length };
    let ranges = coalesce(&[span(0, 10), span(10 + super::COALESCE_GAP_BYTES, 10)]);
    assert_eq!(ranges.len(), 1, "a gap at the threshold is read through");
    assert_eq!(ranges[0].length, super::COALESCE_GAP_BYTES + 20);
}

#[test]
fn area_ids_that_could_traverse_a_path_are_refused() {
    for bad in ["", "../up", "a/b", "a\\b", ".hidden", "sp ace"] {
        assert!(valid_area_id(bad).is_err(), "{bad:?} should be refused");
    }
    for good in ["oklahoma", "area-2026_08.v1", "A9"] {
        assert!(valid_area_id(good).is_ok(), "{good:?} should be accepted");
    }
}

#[test]
fn the_enumeration_starts_at_the_single_z0_tile() {
    let tiles = area_tiles(&monaco_area("enum", 2));
    assert_eq!(tiles[0], (0, 0, 0));
    // One z0, one z1, one z2 tile for a bbox this small.
    assert_eq!(tiles.len(), 3);
}

// ---------------------------------------------------------------------------
// The plan
// ---------------------------------------------------------------------------

#[test]
fn the_default_cap_makes_the_fixture_one_segment_and_a_small_cap_makes_many() {
    let Some(_) =
        monaco_bytes("the_default_cap_makes_the_fixture_one_segment_and_a_small_cap_makes_many")
    else {
        return;
    };
    let index = open_index();
    let area = monaco_area("cap", index.header().max_zoom);

    let whole = block_on(plan_area(&index, &area, SEGMENT_BYTES)).expect("the plan builds");
    assert_eq!(whole.segments.len(), 1, "419 KB is far under the 16 MB cap");

    let small = block_on(plan_area(&index, &area, SMALL_CAP)).expect("the plan builds");
    assert!(
        small.segments.len() > 1,
        "a 64 KB cap must split {} bytes",
        small.fetch_bytes
    );
    for segment in &small.segments {
        assert!(
            segment.tile_bytes <= SMALL_CAP || segment.tile_count() == 1,
            "segment {} holds {} distinct bytes over the cap",
            segment.seg,
            segment.tile_bytes
        );
    }

    // Same tiles, same figure: the plan's exact cost is the index's, however
    // the tiles are cut.
    let quoted = block_on(index.download_bytes(area_tiles(&area))).expect("the quote computes");
    assert_eq!(whole.fetch_bytes, quoted.bytes);
    assert_eq!(small.fetch_bytes, quoted.bytes);
    assert_eq!(whole.present_tiles, quoted.present);
}

// ---------------------------------------------------------------------------
// The engine, over real ranges
// ---------------------------------------------------------------------------

#[test]
fn a_completed_download_transfers_exactly_the_quoted_bytes() {
    let Some(bytes) = monaco_bytes("a_completed_download_transfers_exactly_the_quoted_bytes")
    else {
        return;
    };
    let server = archive_server(bytes);
    let dir = TempDir::new("exact-bytes");
    let index = open_index();
    let area = monaco_area("exact", index.header().max_zoom);
    let quoted = block_on(index.download_bytes(area_tiles(&area))).expect("the quote computes");

    let source = HttpRangeSource::new(
        loopback_client(),
        &format!("{}/archive.pmtiles", server.base_url),
    )
    .expect("the URL parses");
    let engine = BasemapDownload::with_segment_bytes(
        source,
        FsSegmentStore::new(dir.0.clone()),
        area,
        Context::default(),
        SMALL_CAP,
    );

    match wait_outcome(&engine, "exact bytes") {
        DownloadOutcome::Complete { bytes, segments } => {
            assert_eq!(
                bytes, quoted.bytes,
                "the download must transfer exactly what pmt_index quoted"
            );
            assert!(segments > 1, "the small cap should have split the area");
            assert_eq!(published_files(&dir.0).len(), segments as usize);
        }
        other => panic!("expected Complete, got {other:?}"),
    }

    let progress = engine.progress();
    assert_eq!(
        progress.bytes_done, progress.bytes_total,
        "the run finished"
    );
    assert_eq!(progress.segments_done, progress.segments_total);
    assert!(
        server.requests().iter().any(|r| r.contains(".part000")),
        "the layout probe should have run against the loopback host"
    );
}

#[test]
fn each_published_segment_is_a_standalone_archive_holding_its_planned_tiles() {
    let Some(_) =
        monaco_bytes("each_published_segment_is_a_standalone_archive_holding_its_planned_tiles")
    else {
        return;
    };
    let dir = TempDir::new("standalone");
    let index = open_index();
    let area = monaco_area("standalone", index.header().max_zoom);
    let plan = block_on(plan_area(&index, &area, SMALL_CAP)).expect("the plan builds");

    let engine = BasemapDownload::with_segment_bytes(
        FileRangeSource::open(std::path::Path::new(MONACO)).expect("the fixture opens"),
        FsSegmentStore::new(dir.0.clone()),
        area.clone(),
        Context::default(),
        SMALL_CAP,
    );
    assert!(matches!(
        wait_outcome(&engine, "standalone"),
        DownloadOutcome::Complete { .. }
    ));

    for segment in &plan.segments {
        let path = dir
            .0
            .join(format!("{}.{}.pmtiles", area.area_id, segment.seg));
        let reopened = block_on(PmtIndex::open(
            FileRangeSource::open(&path).expect("the published segment opens"),
        ))
        .expect("the published segment is a v3 archive");

        let addressed: HashSet<u64> = block_on(reopened.tile_entries())
            .expect("its directories walk")
            .iter()
            .flat_map(|entry| entry.tile_id..entry.tile_id + entry.run_length)
            .collect();
        let planned: HashSet<u64> = segment
            .tile_coords()
            .map(|(z, x, y)| zxy_to_tile_id(z, x, y).expect("planned tiles are on the grid"))
            .collect();
        assert_eq!(
            addressed, planned,
            "segment {} must address exactly the tiles it was planned to hold",
            segment.seg
        );
    }
}

#[test]
fn a_mid_flight_death_reports_partial_and_publishes_nothing_unfinished() {
    let Some(_) =
        monaco_bytes("a_mid_flight_death_reports_partial_and_publishes_nothing_unfinished")
    else {
        return;
    };
    let dir = TempDir::new("partial");
    let index = open_index();
    let area = monaco_area("partial", index.header().max_zoom);
    let plan = block_on(plan_area(&index, &area, SMALL_CAP)).expect("the plan builds");
    let of = plan.segments.len() as u32;

    // Enough reads to open, plan and finish the first segment; the network
    // dies somewhere in the second.
    let first_segment_reads = coalesce(&segment_spans(&plan.segments[0])).len();
    let budget = (OPEN_READS + first_segment_reads + 1) as i64;
    let engine = BasemapDownload::with_segment_bytes(
        FailingSource::new(
            FileRangeSource::open(std::path::Path::new(MONACO)).expect("the fixture opens"),
            budget,
        ),
        FsSegmentStore::new(dir.0.clone()),
        area.clone(),
        Context::default(),
        SMALL_CAP,
    );

    match wait_outcome(&engine, "partial") {
        DownloadOutcome::Partial {
            done,
            of: reported_of,
            first_error,
            ..
        } => {
            assert_eq!(reported_of, of);
            assert!(done >= 1, "the first segment had the budget to finish");
            assert!(done < of, "the death must not read as done");
            assert!(
                first_error.contains("network went away"),
                "the first error is carried: {first_error}"
            );
            // The store's truth agrees with the report, and nothing
            // unfinished was published: every file present parses whole.
            let files = published_files(&dir.0);
            assert_eq!(files.len(), done as usize);
            for name in &files {
                assert!(!name.ends_with(".part"), "no .part is ever published");
                let reopened = block_on(PmtIndex::open(
                    FileRangeSource::open(&dir.0.join(name)).expect("published files open"),
                ));
                assert!(reopened.is_ok(), "{name} must be a whole archive");
            }
        }
        other => panic!("expected Partial, got {other:?}"),
    }
}

#[test]
fn a_truncated_part_is_never_listed_and_its_own_header_convicts_it() {
    let Some(_) = monaco_bytes("a_truncated_part_is_never_listed_and_its_own_header_convicts_it")
    else {
        return;
    };
    let dir = TempDir::new("truncated-part");
    let area = monaco_area("trunc", open_index().header().max_zoom);

    // The artifact a death mid-write leaves: a prefix of a real segment,
    // still under its .part name because the rename never ran.
    let engine = BasemapDownload::start(
        FileRangeSource::open(std::path::Path::new(MONACO)).expect("the fixture opens"),
        FsSegmentStore::new(dir.0.clone()),
        area.clone(),
        Context::default(),
    );
    assert!(matches!(
        wait_outcome(&engine, "truncated-part setup"),
        DownloadOutcome::Complete { .. }
    ));
    let whole = std::fs::read(dir.0.join(format!("{}.0.pmtiles", area.area_id)))
        .expect("the finished segment reads");
    let part = dir.0.join(format!("{}.9.part", area.area_id));
    std::fs::write(&part, &whole[..whole.len() / 3]).expect("the truncated part writes");

    // (a) It is not listed: completeness is recomputed from .pmtiles files
    // only, so the stray .part cannot make segment 9 read as present.
    let store = FsSegmentStore::new(dir.0.clone());
    let listed = block_on(store.existing_segments(&area.area_id)).expect("the listing reads");
    assert!(
        !listed.contains(&9),
        "a .part must never be listed complete"
    );

    // (b) The artifact convicts itself. A finding worth pinning: the stream
    // writer seeks back and writes header + root at the FRONT, so a prefix of
    // a finalized segment still opens — "a truncated file fails its own
    // header" is only literally true below the 16 KB reserved region. What a
    // longer prefix cannot do is lie about its sections: its own header
    // declares tile bytes past the file's end.
    let truncated = whole.len() as u64 / 3;
    let reopened = block_on(PmtIndex::open(
        FileRangeSource::open(&part).expect("the .part opens as a file"),
    ))
    .expect("a prefix past the reserved front region still opens");
    let header = reopened.header();
    assert!(
        header.tile_data_offset + header.tile_data_length > truncated,
        "the truncated artifact's own header must declare bytes it does not have"
    );

    // (c) A death before the front region is complete does fail the header
    // check outright.
    std::fs::write(&part, &whole[..100]).expect("the shorter part writes");
    assert!(
        block_on(PmtIndex::open(
            FileRangeSource::open(&part).expect("the .part opens as a file")
        ))
        .is_err(),
        "a sub-header truncation must not parse at all"
    );
}

#[test]
fn resume_is_a_set_difference_fetching_only_the_missing_segment() {
    let Some(_) = monaco_bytes("resume_is_a_set_difference_fetching_only_the_missing_segment")
    else {
        return;
    };
    let dir = TempDir::new("resume");
    let index = open_index();
    let area = monaco_area("resume", index.header().max_zoom);
    let plan = block_on(plan_area(&index, &area, SMALL_CAP)).expect("the plan builds");
    assert!(plan.segments.len() > 2, "the resume needs segments to keep");

    // A full download, then one segment lost.
    let engine = BasemapDownload::with_segment_bytes(
        FileRangeSource::open(std::path::Path::new(MONACO)).expect("the fixture opens"),
        FsSegmentStore::new(dir.0.clone()),
        area.clone(),
        Context::default(),
        SMALL_CAP,
    );
    assert!(matches!(
        wait_outcome(&engine, "resume setup"),
        DownloadOutcome::Complete { .. }
    ));
    drop(engine);
    let lost = &plan.segments[1];
    std::fs::remove_file(dir.0.join(format!("{}.{}.pmtiles", area.area_id, lost.seg)))
        .expect("the segment file removes");

    // The resume: only the lost segment's ranges may be fetched.
    let (source, reads) = CountingSource::new(
        FileRangeSource::open(std::path::Path::new(MONACO)).expect("the fixture opens"),
    );
    let resume = BasemapDownload::with_segment_bytes(
        source,
        FsSegmentStore::new(dir.0.clone()),
        area.clone(),
        Context::default(),
        SMALL_CAP,
    );
    match wait_outcome(&resume, "resume") {
        DownloadOutcome::Complete { bytes, segments } => {
            assert_eq!(segments, plan.segments.len() as u32);
            assert_eq!(
                bytes, lost.tile_bytes,
                "only the missing segment's distinct bytes are re-fetched"
            );
        }
        other => panic!("expected Complete, got {other:?}"),
    }
    let expected_reads = OPEN_READS + coalesce(&segment_spans(lost)).len();
    assert_eq!(
        reads.load(Ordering::SeqCst),
        expected_reads,
        "the resume must make exactly the missing segment's reads"
    );
    assert_eq!(published_files(&dir.0).len(), plan.segments.len());
}

#[test]
fn a_second_engine_seeing_a_finished_area_fetches_no_tile_bytes() {
    let Some(_) = monaco_bytes("a_second_engine_seeing_a_finished_area_fetches_no_tile_bytes")
    else {
        return;
    };
    let dir = TempDir::new("no-refetch");
    let area = monaco_area("norefetch", open_index().header().max_zoom);

    let engine = BasemapDownload::start(
        FileRangeSource::open(std::path::Path::new(MONACO)).expect("the fixture opens"),
        FsSegmentStore::new(dir.0.clone()),
        area.clone(),
        Context::default(),
    );
    assert!(matches!(
        wait_outcome(&engine, "no-refetch setup"),
        DownloadOutcome::Complete { .. }
    ));
    drop(engine);

    let (source, reads) = CountingSource::new(
        FileRangeSource::open(std::path::Path::new(MONACO)).expect("the fixture opens"),
    );
    let second = BasemapDownload::start(
        source,
        FsSegmentStore::new(dir.0.clone()),
        area,
        Context::default(),
    );
    match wait_outcome(&second, "no-refetch") {
        DownloadOutcome::Complete { bytes, segments } => {
            assert_eq!(bytes, 0, "a finished area costs nothing to re-check");
            assert_eq!(segments, 1);
        }
        other => panic!("expected Complete, got {other:?}"),
    }
    assert_eq!(
        reads.load(Ordering::SeqCst),
        OPEN_READS,
        "the second engine reads the header, root and metadata, and no tile ranges"
    );
}

#[test]
fn one_segment_peak_held_is_measured_and_reported() {
    let Some(_) = monaco_bytes("one_segment_peak_held_is_measured_and_reported") else {
        return;
    };
    let dir = TempDir::new("peak");
    let index = open_index();
    let area = monaco_area("peak", index.header().max_zoom);
    let quoted = block_on(index.download_bytes(area_tiles(&area))).expect("the quote computes");

    let engine = BasemapDownload::start(
        FileRangeSource::open(std::path::Path::new(MONACO)).expect("the fixture opens"),
        FsSegmentStore::new(dir.0.clone()),
        area,
        Context::default(),
    );
    assert!(matches!(
        wait_outcome(&engine, "peak"),
        DownloadOutcome::Complete { segments: 1, .. }
    ));

    let peak = engine.progress().peak_held;
    // The engine holds at least the segment's distinct tile bytes while the
    // artifact is built beside them; a zero would mean the counter is dead.
    assert!(
        peak.bytes() >= quoted.bytes,
        "the peak ({}) cannot be below the tile bytes it held ({})",
        peak.bytes(),
        quoted.bytes
    );
    // The measurement this step owes, printed with its denominator. NATIVE
    // figure — the wasm measurement is owed separately and is never inferred
    // from this one.
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(
        stderr,
        "MEASURED native one-segment peak engine-held bytes (fetched spans + artifact buffer, \
         Monaco {} tile bytes): {} bytes ({})",
        quoted.bytes,
        peak.bytes(),
        peak.label()
    );
}

// ---------------------------------------------------------------------------
// The web store, over the service worker contract
// ---------------------------------------------------------------------------

#[test]
fn the_http_store_speaks_the_service_worker_contract() {
    let Some(_) = monaco_bytes("the_http_store_speaks_the_service_worker_contract") else {
        return;
    };
    let store_server = StoreServer::start();
    let area = monaco_area("web", open_index().header().max_zoom);
    let origin: reqwest::Url = store_server
        .server
        .base_url
        .parse()
        .expect("the loopback origin parses");

    let engine = BasemapDownload::start(
        FileRangeSource::open(std::path::Path::new(MONACO)).expect("the fixture opens"),
        HttpSegmentStore::new(loopback_client(), origin.clone()),
        area.clone(),
        Context::default(),
    );
    assert!(matches!(
        wait_outcome(&engine, "web store"),
        DownloadOutcome::Complete { segments: 1, .. }
    ));
    drop(engine);

    // The PUT landed where the contract says, and what it carried is a whole
    // archive.
    let stored = store_server.stored();
    let key = format!("{}/0.pmtiles", area.area_id);
    let bytes = stored
        .get(&key)
        .expect("the segment was PUT under its area");
    let reopened = block_on(PmtIndex::open(super::SegmentBytes(Arc::new(bytes.clone()))));
    assert!(reopened.is_ok(), "the stored body is a standalone archive");
    assert!(
        store_server
            .server
            .requests()
            .iter()
            .any(|r| r == &format!("GET /{OFFLINE_BASE_PATH}/__list__")),
        "existing segments are recomputed from the listing"
    );

    // A second engine sees the listing and re-fetches nothing.
    let second = BasemapDownload::start(
        FileRangeSource::open(std::path::Path::new(MONACO)).expect("the fixture opens"),
        HttpSegmentStore::new(loopback_client(), origin.clone()),
        area.clone(),
        Context::default(),
    );
    match wait_outcome(&second, "web store second run") {
        DownloadOutcome::Complete { bytes, .. } => assert_eq!(bytes, 0),
        other => panic!("expected Complete, got {other:?}"),
    }
    let puts_after = store_server
        .server
        .requests()
        .iter()
        .filter(|r| r.starts_with("PUT "))
        .count();
    assert_eq!(puts_after, 1, "the finished segment is not re-stored");

    // DELETE removes the area's segments.
    let store = HttpSegmentStore::new(loopback_client(), origin);
    block_on(store.remove_area(&area.area_id)).expect("the delete succeeds");
    assert!(store_server.stored().is_empty());
}

// ---------------------------------------------------------------------------
// The record, and completeness recomputed rather than stored
// ---------------------------------------------------------------------------

/// A record for a seven-segment area, holding no claim about what is present.
fn seven_segment_record(area_id: &str) -> DownloadedArea {
    DownloadedArea {
        spec: monaco_area(area_id, 12),
        segments_expected: 7,
        bytes: DataSize::from_bytes(112_000_000),
        generation: "basemap_2Fomt-20260828.pmtiles".to_owned(),
    }
}

/// **The defect this feature is most exposed to, as a test.** An area whose
/// bytes are half gone must read as half gone — with both true counts — no
/// matter that the record persisted says seven.
#[test]
fn a_record_whose_segments_are_missing_reads_as_incomplete_with_true_counts() {
    let dir = TempDir::new("reconcile-missing");
    let store = FsSegmentStore::new(dir.0.clone());
    let area = seven_segment_record("reconcile-missing");

    for seg in [0u32, 3, 6] {
        block_on(store.publish(&area.spec.area_id, seg, vec![0u8; 16]))
            .expect("the store accepts a segment");
    }

    let status = block_on(area_status(&store, &area)).expect("the store lists its segments");
    assert_eq!(
        status,
        AreaStatus {
            present: 3,
            expected: 7,
        },
        "the counts must be the store's, not the record's",
    );
    assert!(
        !status.is_complete(),
        "a persisted record read as complete while four of its seven segments \
         are not on the device",
    );

    // The record itself is untouched by any of that: it says what was asked
    // for, which is what makes the answer above recomputable at every launch
    // rather than a flag someone has to remember to clear.
    assert_eq!(area.segments_expected, 7);

    // And the same record over a full store is complete — without which the
    // assertion above could pass on a reconciliation that never says yes.
    for seg in [1u32, 2, 4, 5] {
        block_on(store.publish(&area.spec.area_id, seg, vec![0u8; 16]))
            .expect("the store accepts a segment");
    }
    let filled = block_on(area_status(&store, &area)).expect("the store lists its segments");
    assert_eq!(
        filled,
        AreaStatus {
            present: 7,
            expected: 7,
        }
    );
    assert!(filled.is_complete());
}

/// Segments past the record's own cut are not counted, and an area expecting
/// nothing is never complete.
#[test]
fn reconciliation_counts_against_the_records_own_cut_and_never_divides_by_nothing() {
    let area = seven_segment_record("cut");
    let over = BTreeSet::from([0u32, 1, 2, 7, 8, 9, 10, 11]);
    assert_eq!(
        area.reconcile(&over),
        AreaStatus {
            present: 3,
            expected: 7,
        },
        "leftovers from a longer earlier cut made a three-segment area look done",
    );

    let empty = DownloadedArea {
        segments_expected: 0,
        ..seven_segment_record("empty")
    };
    assert!(
        !empty.reconcile(&BTreeSet::new()).is_complete(),
        "a zero denominator made the emptiest possible record the most \
         confident one",
    );
}

/// Only a finished download yields a record — layer 1 against silent partial
/// success, at the type.
#[test]
fn only_a_complete_outcome_yields_a_record() {
    let spec = monaco_area("outcome", 12);
    let generation = || "basemap_2Fomt-20260828.pmtiles".to_owned();

    let partial = DownloadOutcome::Partial {
        done: 3,
        of: 7,
        bytes: 48_000_000,
        first_error: "the connection went away".to_owned(),
    };
    assert!(
        DownloadedArea::from_outcome(spec.clone(), generation(), &partial).is_none(),
        "a half-downloaded area produced a persistable record",
    );
    assert!(
        DownloadedArea::from_outcome(
            spec.clone(),
            generation(),
            &DownloadOutcome::Failed {
                error: "no archive".to_owned(),
            },
        )
        .is_none(),
    );

    let complete = DownloadOutcome::Complete {
        bytes: 112_000_000,
        segments: 7,
    };
    let record = DownloadedArea::from_outcome(spec.clone(), generation(), &complete)
        .expect("a Complete outcome is exactly what a record is made of");
    assert_eq!(record.spec, spec);
    assert_eq!(record.segments_expected, 7);
    assert_eq!(record.bytes, DataSize::from_bytes(112_000_000));
    assert_eq!(record.generation, generation());
}

// ---------------------------------------------------------------------------
// Planning a leaf-bearing archive
// ---------------------------------------------------------------------------
//
// Monaco addresses all 246 of its tiles from its root, so nothing above this
// line reads a leaf directory at all — and the planning stall the published
// planet archive shows is made entirely of leaf reads. Measured against that
// archive over HTTPS on 2026-08-29: a 568 km box to z14 (110,026 tiles)
// touches 20 distinct leaves; the size probe read all 20, and the engine then
// re-read all 20, one round trip at a time, before the first tile byte moved.
// The archive below is the smallest thing with that shape.

/// Leaf directories in [`leafy_archive`]. More than
/// [`crate::pmt_index::MAX_INFLIGHT_LEAF_READS`] on purpose: a fixture at or
/// under the bound could not tell a bounded burst from an unbounded one.
const LEAFY_LEAVES: usize = 21;

/// Tiles per leaf in [`leafy_archive`] — 341 ids over [`LEAFY_LEAVES`].
const LEAFY_PER_LEAF: usize = 17;

/// The deepest zoom [`leafy_archive`] holds. `z0..=4` is 341 tiles, which is
/// every tile id from 0 to 340 inclusive, so the whole-world area below
/// enumerates exactly the archive's contents and the walk must reach every
/// leaf.
const LEAFY_MAX_ZOOM: u8 = 4;

/// LEB128-encode `value` onto `out`.
fn push_varint(mut value: u64, out: &mut Vec<u8>) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Serialize `(tile_id, run_length, length, offset)` rows in the v3
/// four-column layout, offsets absolute (`offset + 1`).
fn encode_directory(rows: &[(u64, u64, u64, u64)]) -> Vec<u8> {
    let mut out = Vec::new();
    push_varint(rows.len() as u64, &mut out);
    let mut previous = 0u64;
    for &(tile_id, ..) in rows {
        push_varint(tile_id - previous, &mut out);
        previous = tile_id;
    }
    for &(_, run_length, ..) in rows {
        push_varint(run_length, &mut out);
    }
    for &(_, _, length, _) in rows {
        push_varint(length, &mut out);
    }
    for &(.., offset) in rows {
        push_varint(offset + 1, &mut out);
    }
    out
}

/// Where [`leafy_archive`]'s leaf section sits — what tells a read counter
/// which reads were directory round trips.
#[derive(Clone, Copy)]
struct LeafSection {
    offset: u64,
    length: u64,
}

/// A whole v3 archive whose 341 tiles are addressed through [`LEAFY_LEAVES`]
/// leaf directories: header, root, metadata, leaves, tile data, with plain
/// (uncompressed) directories.
fn leafy_archive() -> (Vec<u8>, LeafSection) {
    let tiles = 341u64;
    let blob = 4u64;

    let leaves: Vec<Vec<u8>> = (0..tiles)
        .collect::<Vec<_>>()
        .chunks(LEAFY_PER_LEAF)
        .map(|ids| {
            let rows: Vec<(u64, u64, u64, u64)> =
                ids.iter().map(|&id| (id, 1, blob, id * blob)).collect();
            encode_directory(&rows)
        })
        .collect();
    assert_eq!(leaves.len(), LEAFY_LEAVES, "the fixture's leaf count");

    let mut leaf_bytes = Vec::new();
    let mut root_rows = Vec::new();
    for (at, leaf) in leaves.iter().enumerate() {
        root_rows.push((
            (at * LEAFY_PER_LEAF) as u64,
            0,
            leaf.len() as u64,
            leaf_bytes.len() as u64,
        ));
        leaf_bytes.extend_from_slice(leaf);
    }
    let root = encode_directory(&root_rows);
    let metadata = b"{}".to_vec();

    let root_offset = crate::pmt_index::HEADER_BYTES as u64;
    let metadata_offset = root_offset + root.len() as u64;
    let leaf_offset = metadata_offset + metadata.len() as u64;
    let tile_data_offset = leaf_offset + leaf_bytes.len() as u64;

    let mut header = vec![0u8; crate::pmt_index::HEADER_BYTES];
    header[0..7].copy_from_slice(b"PMTiles");
    header[7] = 3;
    header[8..16].copy_from_slice(&root_offset.to_le_bytes());
    header[16..24].copy_from_slice(&(root.len() as u64).to_le_bytes());
    header[24..32].copy_from_slice(&metadata_offset.to_le_bytes());
    header[32..40].copy_from_slice(&(metadata.len() as u64).to_le_bytes());
    header[40..48].copy_from_slice(&leaf_offset.to_le_bytes());
    header[48..56].copy_from_slice(&(leaf_bytes.len() as u64).to_le_bytes());
    header[56..64].copy_from_slice(&tile_data_offset.to_le_bytes());
    header[64..72].copy_from_slice(&(tiles * blob).to_le_bytes());
    header[72..80].copy_from_slice(&tiles.to_le_bytes());
    header[80..88].copy_from_slice(&tiles.to_le_bytes());
    header[88..96].copy_from_slice(&tiles.to_le_bytes());
    header[96] = 1; // clustered
    header[97] = 1; // internal_compression: none
    header[98] = 1; // tile_compression: none
    header[99] = 1; // tile_type: mvt
    header[100] = 0;
    header[101] = LEAFY_MAX_ZOOM;

    let mut archive = header;
    archive.extend_from_slice(&root);
    archive.extend_from_slice(&metadata);
    archive.extend_from_slice(&leaf_bytes);
    for id in 0..tiles {
        archive.extend_from_slice(&[(id & 0xff) as u8; 4]);
    }
    (
        archive,
        LeafSection {
            offset: leaf_offset,
            length: leaf_bytes.len() as u64,
        },
    )
}

/// The whole world at [`LEAFY_MAX_ZOOM`] — every tile [`leafy_archive`] holds
/// and not one it does not.
fn leafy_area(area_id: &str) -> AreaSpec {
    AreaSpec {
        area_id: area_id.to_owned(),
        west: -179.9,
        south: -84.9,
        east: 179.9,
        north: 84.9,
        max_zoom: LEAFY_MAX_ZOOM,
    }
}

/// One cooperative yield inside a read that answers from memory.
///
/// **The instrument would read serial without it, however concurrent the code
/// under it is**: a future that finishes on its first poll never lets the
/// executor start the one behind it, so a peak-in-flight counter over an
/// in-memory source would measure the source rather than the planner.
async fn yield_once() {
    let mut yielded = false;
    std::future::poll_fn(move |cx| {
        if yielded {
            std::task::Poll::Ready(())
        } else {
            yielded = true;
            cx.waker().wake_by_ref();
            std::task::Poll::Pending
        }
    })
    .await;
}

/// What one [`LeafReadCounter`] saw.
#[derive(Default)]
struct LeafReads {
    leaf_reads: AtomicUsize,
    inflight: AtomicUsize,
    peak_inflight: AtomicUsize,
}

/// An in-memory [`RangeSource`] that counts leaf-directory reads and how many
/// were in flight at once, and answers
/// [`RangeSource::archive_identity`] with whatever it was handed — `None`
/// standing in for the anonymous in-memory source `verify_segment` opens a
/// finished segment over.
struct LeafReadCounter {
    bytes: Arc<Vec<u8>>,
    section: LeafSection,
    identity: Option<String>,
    seen: Arc<LeafReads>,
}

impl RangeSource for LeafReadCounter {
    fn archive_identity(&self) -> Option<String> {
        self.identity.clone()
    }

    fn read_range(
        &self,
        offset: u64,
        length: usize,
    ) -> impl Future<Output = Result<Vec<u8>, RangeError>> + Send {
        let bytes = Arc::clone(&self.bytes);
        let seen = Arc::clone(&self.seen);
        let is_leaf =
            offset >= self.section.offset && offset < self.section.offset + self.section.length;
        async move {
            if is_leaf {
                seen.leaf_reads.fetch_add(1, Ordering::SeqCst);
                let now = seen.inflight.fetch_add(1, Ordering::SeqCst) + 1;
                seen.peak_inflight.fetch_max(now, Ordering::SeqCst);
                yield_once().await;
                seen.inflight.fetch_sub(1, Ordering::SeqCst);
            }
            let from = (offset as usize).min(bytes.len());
            let to = from.saturating_add(length).min(bytes.len());
            Ok(bytes[from..to].to_vec())
        }
    }
}

/// Plan `area` over a fresh reader of `archive` named `identity`, and report
/// the plan beside what that reader's leaf reads looked like.
fn plan_over(
    archive: &Arc<Vec<u8>>,
    section: LeafSection,
    identity: Option<&str>,
    area: &AreaSpec,
) -> (super::DownloadPlan, Arc<LeafReads>) {
    let seen = Arc::new(LeafReads::default());
    let source = LeafReadCounter {
        bytes: Arc::clone(archive),
        section,
        identity: identity.map(str::to_owned),
        seen: Arc::clone(&seen),
    };
    let plan = block_on(async {
        let index = PmtIndex::open(source).await.expect("the archive opens");
        plan_area(&index, area, SEGMENT_BYTES)
            .await
            .expect("the plan builds")
    });
    (plan, seen)
}

/// The plan as a value two runs can be compared on: the totals, the cut, and
/// every segment's tiles in order.
type PlanShape = (u64, u64, u64, Vec<(u32, u64, Vec<(u8, u32, u32)>)>);

/// See [`PlanShape`].
fn plan_shape(plan: &super::DownloadPlan) -> PlanShape {
    (
        plan.fetch_bytes,
        plan.present_tiles,
        plan.absent_tiles,
        plan.segments
            .iter()
            .map(|segment| {
                (
                    segment.seg,
                    segment.tile_bytes,
                    segment.tile_coords().collect(),
                )
            })
            .collect(),
    )
}

/// A unique archive name per call, so one test's shared directories can never
/// be another's — the suite shares a process with the registry.
fn unique_identity(what: &str) -> String {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    format!(
        "test://{what}/{}/{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    )
}

#[test]
fn the_planner_never_re_reads_a_leaf_an_earlier_reader_of_the_archive_already_read() {
    let (archive, section) = leafy_archive();
    let archive = Arc::new(archive);
    let area = leafy_area("leafy-shared");
    let named = unique_identity("shared");

    // The size probe's reader: it pays for every leaf.
    let (probe, probe_seen) = plan_over(&archive, section, Some(&named), &area);
    assert_eq!(
        probe_seen.leaf_reads.load(Ordering::SeqCst),
        LEAFY_LEAVES,
        "the first reader of the archive must read every leaf the area touches"
    );

    // The engine's reader, opened separately over the same archive.
    let (engine, engine_seen) = plan_over(&archive, section, Some(&named), &area);
    assert_eq!(
        engine_seen.leaf_reads.load(Ordering::SeqCst),
        0,
        "the engine re-read leaves the probe had already decoded"
    );

    // The control, without which the assertion above would pass just as well
    // on a build that had simply stopped reading leaves at all.
    let (other, other_seen) = plan_over(&archive, section, Some(&unique_identity("other")), &area);
    assert_eq!(
        other_seen.leaf_reads.load(Ordering::SeqCst),
        LEAFY_LEAVES,
        "a reader of a differently-named archive must share nothing"
    );

    // Sharing changes when the bytes arrive, never what they say: the plan is
    // still the pure function of the area and the archive resume rests on.
    assert_eq!(plan_shape(&probe), plan_shape(&engine));
    assert_eq!(plan_shape(&probe), plan_shape(&other));
    assert_eq!(probe.fetch_bytes, 341 * 4);
    assert_eq!(probe.present_tiles, 341);
    assert_eq!(probe.absent_tiles, 0);
}

#[test]
fn an_unnamed_archives_directories_are_never_shared_with_another_reader() {
    // The property that keeps `verify_segment` honest: it opens a just-built
    // segment over an in-memory source, and a segment handed another
    // segment's directories would verify the wrong bytes.
    let (archive, section) = leafy_archive();
    let archive = Arc::new(archive);
    let area = leafy_area("leafy-anonymous");

    for round in 0..2 {
        let (_, seen) = plan_over(&archive, section, None, &area);
        assert_eq!(
            seen.leaf_reads.load(Ordering::SeqCst),
            LEAFY_LEAVES,
            "reader {round} of an anonymous archive was handed someone else's directories"
        );
    }
}

#[test]
fn the_planners_leaf_reads_overlap_rather_than_queueing_one_at_a_time() {
    let (archive, section) = leafy_archive();
    let archive = Arc::new(archive);
    let area = leafy_area("leafy-concurrent");
    let (_, seen) = plan_over(
        &archive,
        section,
        Some(&unique_identity("concurrent")),
        &area,
    );

    assert_eq!(
        seen.peak_inflight.load(Ordering::SeqCst),
        crate::pmt_index::MAX_INFLIGHT_LEAF_READS,
        "the planner's leaf reads must fill the in-flight bound rather than queue \
         behind each other — and must not exceed it"
    );
    assert_eq!(
        seen.leaf_reads.load(Ordering::SeqCst),
        LEAFY_LEAVES,
        "overlapping the reads must not read a leaf twice"
    );
}
