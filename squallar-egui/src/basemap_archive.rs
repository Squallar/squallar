//! The PMTiles v3 basemap archive reader, behind `basemap-vector`.
//!
//! **Nothing calls this yet.** The draw seam is a later step; this module is
//! reached only from its own tests. It is here so that the transport questions
//! — range requests, retries, the wasm bound — are settled against a real
//! archive before a renderer depends on the answers.
//!
//! # A range source, not a transport
//!
//! The archive is a set of byte ranges over one immutable file. *Where those
//! bytes come from* is a [`RangeSource`], and HTTP and a local file are two
//! sources of the same abstraction rather than two code paths. That is what
//! later lets a downloaded sub-archive be a source the reader prefers instead
//! of a fork of the reader.
//!
//! [`ArchiveRangeSource`] names the bound in one place, blanket-implemented,
//! on the shape [`crate::tile_source::AsyncTileSource`] established. **One
//! difference from that precedent is worth stating rather than leaving to be
//! rediscovered:** `AsyncTileSource` relaxes `Send + Sync` on wasm32, and this
//! trait cannot. `pmtiles::AsyncBackend::read` returns
//! `impl Future<Output = ...> + Send` and `AsyncPmTilesReader`'s inherent impl
//! is bounded `B: AsyncBackend + Sync + Send` on *every* target
//! (`pmtiles-0.23.0/src/async_reader.rs:76`, `:424`), so a wasm-relaxed bound
//! would not satisfy the crate we are calling. The per-target split therefore
//! lands one level down, in [`transport`]: the JS work runs inside a
//! `spawn_local`'d task and the bytes come back over a channel, so the future
//! this crate hands `pmtiles` is `Send` on a target where `JsValue` is not.
//!
//! # Absence and failure are different things
//!
//! `pmtiles`' `get_tile` answers `Ok(None)` for a tile that is not in the
//! archive — and a caller that treats a transport failure the same way renders
//! an empty map either way, which is the "silent partial success" shape this
//! workspace has named as a recurring defect. [`TileBytes`] makes the
//! distinction a fact of the type: [`TileBytes::Absent`] is a *positive*
//! statement that the directories were read and hold no entry, and anything
//! that went wrong on the way is an [`ArchiveError`].
//!
//! # Two facts the transport is built around
//!
//! * **A `200` is retryable, not fatal.** `pmtiles::HttpBackend::read` errors
//!   `RangeRequestsUnsupported` on any status other than `206`
//!   (`pmtiles-0.23.0/src/backends/http.rs:67`). Measured against a real host,
//!   2 of 11 range requests came back `200` with the full body. Treated as
//!   fatal that reads like a corrupt archive rather than a transport fault, so
//!   [`HttpRangeSource`] retries up to [`RANGE_ATTEMPTS`] times and reports the
//!   status and the attempt count when it gives up.
//!
//!   The body of a non-`206` is **deliberately never read**: a `200` carries
//!   the whole archive, and buffering 151 MB — a planet archive, ~125 GB — to
//!   reach a 16 KiB directory is a worse outcome than the error.
//! * **The directory cache is mandatory.** With `NoCache` every tile costs two
//!   range requests on an archive deep enough to have leaf directories: one for
//!   the leaf, one for the tile. [`BasemapArchive`] holds a
//!   `pmtiles::HashMapCache` and `directory_cache_is_load_bearing` in the tests
//!   is the gate on it, counted rather than asserted in prose.

use std::fmt;
use std::future::Future;

use pmtiles::{
    AsyncBackend, AsyncPmTilesReader, BackendResponse, Compression, HashMapCache, Header, PmtError,
    PmtResult, TileCoord, TileType,
};
use reqwest::{Client, Url};

/// Range requests one [`HttpRangeSource::read_range`] will make before giving
/// up on a source that keeps answering `200`.
///
/// Bounded rather than persistent: a host that does not do ranges at all will
/// never start, and an unbounded retry against it is an infinite loop wearing a
/// recovery's clothes. Three is one original plus two retries, against a
/// measured rate of 2 in 11.
pub const RANGE_ATTEMPTS: u32 = 3;

/// HTTP status a range request is asking for.
const STATUS_PARTIAL_CONTENT: u16 = 206;

// ---------------------------------------------------------------------------
// The abstraction
// ---------------------------------------------------------------------------

/// Something that can hand back a byte range of one immutable archive.
///
/// `read_range` reads **up to** `length` bytes from `offset`, matching
/// `pmtiles::AsyncBackend::read`'s own contract — a source that has fewer bytes
/// left than were asked for returns what it has rather than erroring, and it is
/// `read_exact` above that decides whether that is short.
pub trait RangeSource {
    /// Read up to `length` bytes starting at `offset`.
    fn read_range(
        &self,
        offset: u64,
        length: usize,
    ) -> impl Future<Output = Result<Vec<u8>, RangeError>> + Send;
}

/// A [`RangeSource`] the reader can hold.
///
/// Blanket-implemented, so implementing [`RangeSource`] is the whole of the
/// work; this exists to state the bound once. See the module doc for why it
/// does **not** split per target the way
/// [`crate::tile_source::AsyncTileSource`] does.
pub trait ArchiveRangeSource: RangeSource + Send + Sync + 'static {}

impl<S: RangeSource + Send + Sync + 'static> ArchiveRangeSource for S {}

/// Instantiating this proves a source satisfies the bound [`BasemapArchive`]
/// needs, at compile time and on whichever target is being built.
///
/// The `const _` uses below are what make that a **wasm32** guarantee too. The
/// tests are native-only, so without these the web arm's only evidence that
/// `HttpRangeSource` is still `Send + Sync` there would be a host test on a
/// different target — and it is exactly on wasm32 that a `!Send` type is one
/// stray field away.
fn assert_source_bounds<S: ArchiveRangeSource>() {}

/// What went wrong reaching for a byte range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeError {
    /// The source could not be reached, or answered with a failure status.
    Transport(String),
    /// The source answered, repeatedly, with something other than a `206`.
    ///
    /// Carries what it did answer and how many attempts were spent, because
    /// "the archive is corrupt" and "this host does not do range requests" are
    /// the two readings of the same symptom and only the second is actionable.
    NotRanged {
        /// The status of the last attempt.
        status: u16,
        /// How many attempts were made.
        attempts: u32,
    },
    /// The task carrying the request went away before it answered. wasm32
    /// only: the JS work runs in a detached task and the page can drop it.
    Cancelled,
}

impl fmt::Display for RangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(message) => write!(f, "range request failed: {message}"),
            Self::NotRanged { status, attempts } => write!(
                f,
                "the source answered {status} rather than {STATUS_PARTIAL_CONTENT} on all \
                 {attempts} attempts; it does not appear to serve range requests"
            ),
            Self::Cancelled => write!(f, "the range request was dropped before it answered"),
        }
    }
}

impl std::error::Error for RangeError {}

// ---------------------------------------------------------------------------
// The adapter onto `pmtiles`
// ---------------------------------------------------------------------------

/// Presents a [`RangeSource`] as the backend `pmtiles` asks for.
///
/// The one place the two vocabularies meet, so no source has to know that
/// `pmtiles` exists and `pmtiles` never sees more than one backend type.
pub struct RangeBackend<S> {
    source: S,
}

impl<S> RangeBackend<S> {
    /// Wrap `source`.
    pub fn new(source: S) -> Self {
        Self { source }
    }
}

impl<S: ArchiveRangeSource> AsyncBackend for RangeBackend<S> {
    async fn read(&self, offset: usize, length: usize) -> PmtResult<BackendResponse> {
        // The source's own error is preserved as the `io::Error`'s cause rather
        // than flattened to a string, so a caller three layers up can still
        // read why the bytes did not arrive. `PmtError` has no variant for "the
        // backend failed" that is available without `http-async`, and that
        // feature is off on wasm32.
        let bytes = self
            .source
            .read_range(offset as u64, length)
            .await
            .map_err(|error| PmtError::Reading(std::io::Error::other(error)))?;

        Ok(BackendResponse::new(bytes.into()))
    }
}

// ---------------------------------------------------------------------------
// The HTTP source
// ---------------------------------------------------------------------------

/// Byte ranges over HTTP.
///
/// The client is handed in rather than built here, so archive requests go
/// through the same platform-verified TLS as every other squallar request —
/// see [`crate::tile_source`] for why that matters.
pub struct HttpRangeSource {
    client: Client,
    url: Url,
}

/// See [`assert_source_bounds`]. Checked on every target, wasm32 included.
const _: fn() = assert_source_bounds::<HttpRangeSource>;

/// Per-range request timeout.
///
/// A directory or tile read is a small range off object storage; the same
/// figure [`crate::tile_source`] uses for a tile, for the same reason — a
/// request that never answers otherwise holds its slot for as long as the
/// process lives.
pub const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// The HTTP client archive ranges should be read through.
///
/// Spelled here rather than left to the call site, because the call site is
/// step 7 and this is exactly the decision [`crate::tile_source`] exists to
/// stop being re-taken: every squallar request goes through
/// `squallar_radar::tls::client` (`rustls-platform-verifier` + *ring*), so the
/// OS decides trust at handshake time and nothing in the binary carries an
/// expiry date. A `reqwest::Client::new()` here would both bypass that and, on
/// this workspace's `rustls-no-provider` pin, panic on construction.
///
/// # Panics
///
/// If the client will not build, which means the TLS configuration is wrong
/// for the target rather than anything a caller can recover from.
pub fn archive_client() -> Client {
    squallar_radar::tls::client(squallar_radar::tls::USER_AGENT, REQUEST_TIMEOUT)
        .build()
        .expect("the basemap archive HTTP client should build")
}

impl HttpRangeSource {
    /// A source reading `url` through `client`.
    ///
    /// # Errors
    ///
    /// Returns the string that would not parse as a URL.
    pub fn new(client: Client, url: &str) -> Result<Self, RangeError> {
        let url = Url::parse(url)
            .map_err(|error| RangeError::Transport(format!("{url} is not a URL: {error}")))?;

        Ok(Self { client, url })
    }
}

impl RangeSource for HttpRangeSource {
    fn read_range(
        &self,
        offset: u64,
        length: usize,
    ) -> impl Future<Output = Result<Vec<u8>, RangeError>> + Send {
        // `bytes=N-M`, inclusive at both ends, which is the one form the CORS
        // safelist covers — so no preflight, which is what makes reading the
        // archive straight off object storage viable at all.
        let end = offset.saturating_add(length as u64).saturating_sub(1);
        let range = format!("bytes={offset}-{end}");
        let client = self.client.clone();
        let url = self.url.clone();

        async move {
            let mut last_status = 0;

            for attempt in 1..=RANGE_ATTEMPTS {
                match transport::fetch(client.clone(), url.clone(), range.clone()).await? {
                    RangeReply::Range { bytes } => return Ok(bytes),
                    RangeReply::NotRanged { status } => {
                        log::debug!(
                            "{url} answered {status} rather than {STATUS_PARTIAL_CONTENT} for \
                             {range} (attempt {attempt} of {RANGE_ATTEMPTS})"
                        );
                        last_status = status;
                    }
                }
            }

            Err(RangeError::NotRanged {
                status: last_status,
                attempts: RANGE_ATTEMPTS,
            })
        }
    }
}

/// What one HTTP attempt came back with.
enum RangeReply {
    /// A `206` and its bytes.
    Range {
        /// The bytes of the range.
        bytes: Vec<u8>,
    },
    /// A success that was not a `206`. The body was not read.
    NotRanged {
        /// The status that came back.
        status: u16,
    },
}

/// Perform one range request.
///
/// Target-independent, and deliberately so: the `cfg` in [`transport`] selects
/// only **where this future is driven**, never what it does. On wasm32 the
/// future it returns is not `Send` — `reqwest::Response` wraps a `JsValue` —
/// which is exactly why it is driven inside a `spawn_local`'d task there rather
/// than written twice.
async fn execute(client: Client, url: Url, range: String) -> Result<RangeReply, RangeError> {
    let response = client
        .get(url.clone())
        .header(reqwest::header::RANGE, range.as_str())
        .send()
        .await
        .map_err(|error| RangeError::Transport(format!("{url}: {error}")))?;

    let status = response.status();
    if !status.is_success() {
        return Err(RangeError::Transport(format!(
            "{url} answered {status} for {range}"
        )));
    }

    if status.as_u16() != STATUS_PARTIAL_CONTENT {
        // The body is dropped unread on purpose. A `200` to a range request
        // carries the WHOLE archive, so reading it to find out what we got
        // would download 151 MB of Oklahoma — or ~125 GB of planet — to reach
        // a 16 KiB directory.
        drop(response);
        return Ok(RangeReply::NotRanged {
            status: status.as_u16(),
        });
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|error| RangeError::Transport(format!("{url}: reading {range}: {error}")))?;

    Ok(RangeReply::Range {
        bytes: bytes.to_vec(),
    })
}

/// Where an HTTP range request is driven.
///
/// The whole per-target split, and it is a *selection* rather than a fork:
/// both arms call the one [`execute`] above. The shape follows
/// [`crate::tile_source`]'s own `runtime` module, which splits the IO runtime
/// the same way.
mod transport {
    #[cfg(not(target_arch = "wasm32"))]
    mod native {
        use reqwest::{Client, Url};

        use super::super::{RangeError, RangeReply, execute};

        /// Driven on the caller's runtime. `execute`'s future is already
        /// `Send` here, so there is nothing to bridge.
        pub(crate) fn fetch(
            client: Client,
            url: Url,
            range: String,
        ) -> impl Future<Output = Result<RangeReply, RangeError>> + Send {
            execute(client, url, range)
        }
    }

    #[cfg(target_arch = "wasm32")]
    mod wasm {
        use futures::channel::oneshot;
        use reqwest::{Client, Url};

        use super::super::{RangeError, RangeReply, execute};

        /// Driven inside a `spawn_local`'d task, with the result posted back
        /// over a channel.
        ///
        /// This is the whole reason the web target needs its own backend.
        /// `pmtiles::AsyncBackend::read` must return a `Send` future, and on
        /// wasm32 `reqwest::Response` is not `Send` — `pmtiles`' own
        /// `HttpBackend` fails its own trait here with two E0277s. The
        /// `!Send` half stays inside the task; what crosses back out is a
        /// `oneshot::Receiver<Result<RangeReply, RangeError>>`, which is
        /// `Send` because `RangeReply` holds `Vec<u8>` and nothing from JS.
        pub(crate) fn fetch(
            client: Client,
            url: Url,
            range: String,
        ) -> impl Future<Output = Result<RangeReply, RangeError>> + Send {
            let (result_tx, result_rx) = oneshot::channel();

            wasm_bindgen_futures::spawn_local(async move {
                // The receiver is gone if the reader was dropped mid-request,
                // which is a cancellation and not an error to report.
                let _ = result_tx.send(execute(client, url, range).await);
            });

            async move { result_rx.await.map_err(|_| RangeError::Cancelled)? }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(super) use native::fetch;
    #[cfg(target_arch = "wasm32")]
    pub(super) use wasm::fetch;
}

// ---------------------------------------------------------------------------
// The local-file source
// ---------------------------------------------------------------------------

/// Byte ranges over a local file.
///
/// Native only — wasm32 has no `std::fs`, and this is a `cfg` selecting a
/// *dependency*, which is what ARCHITECTURE.md permits one to do. The web
/// target's local archives arrive as a different [`RangeSource`], which is the
/// point of the abstraction.
///
/// The reads block. That is correct where this runs and wrong where it does
/// not: `tile_source`'s IO runtime owns a thread that exists to block, so
/// `std::fs` on it costs nothing and adds no dependency, while the frame thread
/// must never see one of these.
#[cfg(not(target_arch = "wasm32"))]
pub struct FileRangeSource {
    file: std::sync::Mutex<std::fs::File>,
    len: u64,
}

#[cfg(not(target_arch = "wasm32"))]
impl FileRangeSource {
    /// Open `path` for range reads.
    ///
    /// # Errors
    ///
    /// Returns [`RangeError::Transport`] if the file cannot be opened or its
    /// length cannot be read.
    pub fn open(path: &std::path::Path) -> Result<Self, RangeError> {
        let file = std::fs::File::open(path)
            .map_err(|error| RangeError::Transport(format!("{}: {error}", path.display())))?;
        let len = file
            .metadata()
            .map_err(|error| RangeError::Transport(format!("{}: {error}", path.display())))?
            .len();

        Ok(Self {
            file: std::sync::Mutex::new(file),
            len,
        })
    }

    /// The file's length in bytes.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Whether the file is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// See [`assert_source_bounds`].
#[cfg(not(target_arch = "wasm32"))]
const _: fn() = assert_source_bounds::<FileRangeSource>;

#[cfg(not(target_arch = "wasm32"))]
impl RangeSource for FileRangeSource {
    fn read_range(
        &self,
        offset: u64,
        length: usize,
    ) -> impl Future<Output = Result<Vec<u8>, RangeError>> + Send {
        // `read_range` reads *up to* `length`, so a range running past the end
        // is clamped rather than refused — the header read asks for 16 KiB and
        // an archive can be smaller than that.
        let available = self.len.saturating_sub(offset);
        let wanted = usize::try_from(available.min(length as u64)).unwrap_or(0);

        async move {
            use std::io::{Read as _, Seek as _, SeekFrom};

            let mut file = self
                .file
                .lock()
                .map_err(|_| RangeError::Transport("the archive file lock is poisoned".into()))?;
            file.seek(SeekFrom::Start(offset))
                .map_err(|error| RangeError::Transport(format!("seek to {offset}: {error}")))?;

            let mut bytes = vec![0u8; wanted];
            file.read_exact(&mut bytes)
                .map_err(|error| RangeError::Transport(format!("read at {offset}: {error}")))?;

            Ok(bytes)
        }
    }
}

// ---------------------------------------------------------------------------
// The reader
// ---------------------------------------------------------------------------

/// What the archive holds for one tile.
///
/// The whole point of the type: `Absent` is a **positive** answer, reached by
/// reading the directories and finding no entry, and it can never stand in for
/// a range that failed to arrive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TileBytes {
    /// The archive holds this tile. The bytes are decompressed.
    Present(Vec<u8>),
    /// The archive's directories were read and hold no entry for this tile.
    Absent,
}

impl TileBytes {
    /// The bytes, if the tile is present.
    pub fn bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Present(bytes) => Some(bytes),
            Self::Absent => None,
        }
    }

    /// Whether the archive holds this tile.
    pub fn is_present(&self) -> bool {
        matches!(self, Self::Present(_))
    }
}

/// What went wrong reading the archive.
#[derive(Debug)]
pub enum ArchiveError {
    /// The archive would not open: unreachable, truncated, or not PMTiles v3.
    Open(PmtError),
    /// A tile could not be read.
    Tile(PmtError),
    /// The coordinate is not a tile: a zoom past 31, or an `x`/`y` outside the
    /// grid at its zoom.
    Coordinate {
        /// Zoom.
        z: u8,
        /// Column.
        x: u32,
        /// Row.
        y: u32,
    },
}

impl fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open(error) => write!(f, "the basemap archive would not open: {error}"),
            Self::Tile(error) => write!(f, "the basemap archive would not read a tile: {error}"),
            Self::Coordinate { z, x, y } => write!(f, "{z}/{x}/{y} is not a tile coordinate"),
        }
    }
}

impl std::error::Error for ArchiveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Open(error) | Self::Tile(error) => Some(error),
            Self::Coordinate { .. } => None,
        }
    }
}

/// A PMTiles v3 archive, read a range at a time from whatever source it was
/// opened over.
///
/// The directory cache is not optional and is not a tuning dial — see the
/// module doc.
pub struct BasemapArchive<S: ArchiveRangeSource> {
    reader: AsyncPmTilesReader<RangeBackend<S>, HashMapCache>,
}

impl<S: ArchiveRangeSource> BasemapArchive<S> {
    /// Open the archive `source` addresses, reading and validating its header
    /// and root directory.
    ///
    /// # Errors
    ///
    /// [`ArchiveError::Open`] if the source will not answer, or if what it
    /// answers with is not a valid PMTiles v3 archive.
    pub async fn open(source: S) -> Result<Self, ArchiveError> {
        let reader = AsyncPmTilesReader::try_from_cached_source(
            RangeBackend::new(source),
            HashMapCache::default(),
        )
        .await
        .map_err(ArchiveError::Open)?;

        Ok(Self { reader })
    }

    /// The archive's header.
    pub fn header(&self) -> &Header {
        self.reader.get_header()
    }

    /// The deepest zoom the archive stores.
    ///
    /// Read from the archive rather than assumed: the render ceiling and any
    /// later download ceiling then come from one number that cannot drift.
    pub fn max_zoom(&self) -> u8 {
        self.header().max_zoom
    }

    /// The shallowest zoom the archive stores.
    pub fn min_zoom(&self) -> u8 {
        self.header().min_zoom
    }

    /// What the tiles are.
    pub fn tile_type(&self) -> TileType {
        self.header().tile_type
    }

    /// How the tiles are compressed on the wire. [`Self::tile`] has already
    /// undone this by the time it answers.
    pub fn tile_compression(&self) -> Compression {
        self.header().tile_compression
    }

    /// Fetch one tile, decompressed.
    ///
    /// # Errors
    ///
    /// [`ArchiveError::Coordinate`] if `z/x/y` is not a tile; [`ArchiveError::Tile`]
    /// if the directories or the tile body could not be read, or the tile could
    /// not be decompressed. **A tile the archive does not hold is
    /// [`TileBytes::Absent`], not an error** — and, equally, a failure is never
    /// reported as absence.
    pub async fn tile(&self, z: u8, x: u32, y: u32) -> Result<TileBytes, ArchiveError> {
        let coord = TileCoord::new(z, x, y).map_err(|_| ArchiveError::Coordinate { z, x, y })?;

        match self
            .reader
            .get_tile_decompressed(coord)
            .await
            .map_err(ArchiveError::Tile)?
        {
            Some(bytes) => Ok(TileBytes::Present(bytes.to_vec())),
            None => Ok(TileBytes::Absent),
        }
    }
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests;
