//! The PMTiles v3 basemap archive reader.
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
//! * **A published archive may be split into parts.** The publish side slices
//!   anything the edge cannot cache whole into [`PART_BYTES`]-byte files at
//!   `<url>.partNNN`, and [`HttpRangeSource`] probes for `part000` at open to
//!   decide which shape it is reading — see its docs for the contract. The
//!   split lives entirely inside that source: [`part_spans`] cuts a global
//!   range into per-part reads and nothing above the [`RangeSource`] seam
//!   knows parts exist. The stride that arithmetic assumes is **checked, not
//!   trusted**: a part stopping short of it with a later part still there is
//!   [`RangeError::ShortPart`], never `Ok` with fewer bytes than were asked
//!   for.
//! * **The directory cache is mandatory.** With `NoCache` every tile costs two
//!   range requests on an archive deep enough to have leaf directories: one for
//!   the leaf, one for the tile. [`BasemapArchive`] holds a
//!   `pmtiles::HashMapCache` and `directory_cache_is_load_bearing` in the tests
//!   is the gate on it, counted rather than asserted in prose.

use std::fmt;
use std::future::Future;
use std::sync::Arc;

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

/// Bytes in every published part except the last.
///
/// **A publish-side contract, not a tuning dial.** The publisher slices the
/// archive at exactly this stride, so the reader's `offset / PART_BYTES`
/// arithmetic in [`part_spans`] is only correct while the two agree. It is
/// under Cloudflare's 512 MB cacheable-object ceiling (measured: a 3.34 GB
/// object BYPASSes the cache), which is the entire point of the parts.
///
/// Nothing in the bytes carries the promise, and the publisher is not in this
/// repository, so [`HttpRangeSource`] **checks** it rather than assuming it:
/// a part answering short is the archive ending only if no later part exists,
/// and [`RangeError::ShortPart`] is what a disagreement reads as. See
/// [`HttpRangeSource::check_short_part_is_the_last`].
pub const PART_BYTES: u64 = 500_000_000;

/// Whether `status` is a host cleanly saying "no such object".
///
/// The probe's monolith verdict hangs on this being *clean* absence: a `404`
/// (or a `410`, absence with a tombstone) from a healthy host. A `5xx` or a
/// timeout is a host failing to answer, and reading that as "no parts" would
/// silently select monolith mode during an origin outage — the wrong archive
/// shape held for the source's whole lifetime.
fn is_clean_absence(status: u16) -> bool {
    status == 404 || status == 410
}

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

    /// A stable name for the archive behind these bytes, or `None` when the
    /// source cannot promise the bytes will never move under a reader.
    ///
    /// The name is what lets two readers of one archive share decoded
    /// directories ([`crate::pmt_index`]), so answering `Some` is a promise
    /// that the same name always addresses the same bytes for as long as the
    /// process lives. A name that outlived its bytes would hand the second
    /// reader the first one's directories for a different file, which is why
    /// the default is `None` and each implementation opts in by name: a
    /// wrapper that forwards only `read_range` inherits anonymity rather than
    /// its inner source's promise.
    fn archive_identity(&self) -> Option<String> {
        None
    }
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
    /// A part that is not the archive's last ran short of the publish stride.
    ///
    /// The publish side's own contract ([`PART_BYTES`]) is that every part but
    /// the final one is exactly the stride long, and the parts arithmetic in
    /// [`part_spans`] is only correct while that holds. Nothing in the bytes
    /// carries the promise, so [`HttpRangeSource`] checks it at the one place
    /// it would otherwise be assumed: a short answer is the archive ending
    /// only if no later part exists. When one does, this says so instead of
    /// returning fewer bytes than were asked for and letting a decoder three
    /// layers up call it a corrupt archive.
    ShortPart {
        /// The part that ran short.
        part: u64,
        /// Bytes it answered with.
        got: usize,
        /// Bytes the span asked it for.
        wanted: usize,
        /// The stride the reader and the publisher are supposed to agree on.
        stride: u64,
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
            Self::ShortPart {
                part,
                got,
                wanted,
                stride,
            } => write!(
                f,
                "part{part:03} answered {got} bytes of the {wanted} asked for, but part{:03} \
                 exists, so the archive did not end there: the published parts do not match the \
                 {stride}-byte stride this reader cuts ranges at",
                part + 1
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
    /// `offset` is `u64` and is passed straight through.
    ///
    /// It used to be `usize`, and that was the whole of a defect that made the
    /// self-hosted basemap unreachable in a browser for as long as it shipped.
    /// `usize` is 32 bits on wasm32; the published archive is 83.8 GB with its
    /// leaf directories at `leaf_offset = 83_785_884_629`; every leaf read was
    /// truncated to `83_785_884_629 mod 2**32 = 2_181_506_005` and fetched
    /// bytes from a quarter of the way into the archive, which are not a gzip
    /// stream. Since the root directory is 2917 leaf pointers and zero direct
    /// tiles, that was every tile. See `vendor/pmtiles/VENDORED.md` for the
    /// whole account, and `four_gib_offset_tests` beside this file for the pin
    /// — `the_seam_cannot_narrow` is the half a 64-bit builder cannot pass
    /// vacuously.
    ///
    /// The narrowing was never this crate's: [`RangeSource::read_range`] has
    /// taken a `u64` offset since it was written, and the `as u64` that used to
    /// stand here was widening a value that had already lost its top bits.
    async fn read(&self, offset: u64, length: usize) -> PmtResult<BackendResponse> {
        // The source's own error is preserved as the `io::Error`'s cause rather
        // than flattened to a string, so a caller three layers up can still
        // read why the bytes did not arrive. `PmtError` has no variant for "the
        // backend failed" that is available without `http-async`, and that
        // feature is off on wasm32.
        let bytes = self
            .source
            .read_range(offset, length)
            .await
            .map_err(|error| PmtError::Reading(std::io::Error::other(error)))?;

        // `pmtiles` opens with a plain "up to `length`" read at offset zero
        // and then slices BOTH the header and the root directory out of the
        // one answer with `Bytes::split_off`/`split_to`
        // (`pmtiles-0.23.0/src/async_reader.rs:88-102`) — which **panic**
        // rather than erroring when the answer stops inside either. It only
        // guards the header's own 127 bytes. Measured against the committed
        // fixture: an answer of 0 or 100 bytes fails the open cleanly, and
        // one of 127..=637 aborts the task.
        //
        // A truncated segment, or a `part000` that stops early, is exactly
        // such a source. The bound is checked here because this is the one
        // place holding both the read and what it answered, off the same
        // header parser [`crate::pmt_index`] uses so the two cannot disagree
        // about where the root directory is.
        if offset == 0
            && let Ok(header) = crate::pmt_index::parse_header(&bytes)
            && (bytes.len() as u64) < header.root_offset.saturating_add(header.root_length)
        {
            return Err(PmtError::Reading(std::io::Error::other(format!(
                "the archive answered {} bytes of its opening read, which stops inside the root \
                 directory it declares at {}..{}",
                bytes.len(),
                header.root_offset,
                header.root_offset.saturating_add(header.root_length)
            ))));
        }

        Ok(BackendResponse::new(bytes.into()))
    }
}

// ---------------------------------------------------------------------------
// The HTTP source
// ---------------------------------------------------------------------------

/// Byte ranges over HTTP, over an archive published either as one file or as
/// byte-slice parts.
///
/// The client is handed in rather than built here, so archive requests go
/// through the same platform-verified TLS as every other squallar request —
/// see [`crate::tile_source`] for why that matters.
///
/// # Parts
///
/// A published archive can exceed what an edge cache will hold as one object —
/// the planet build is ~84 GB against Cloudflare's 512 MB cacheable-object
/// ceiling, so every range request against the monolith crosses to origin
/// storage and bills there. The publish side therefore slices the archive into
/// [`PART_BYTES`]-byte files named `<archive-url>.part000`, `.part001`, …
/// (three digits, zero-padded), each small enough for the edge to absorb.
/// Parts are the publish format; the bare un-suffixed URL is the
/// compatibility path for generations published before the split.
///
/// Which of the two this source is reading is a **probe, not configuration**:
/// the first read asks for the opening bytes of `<url>.part000`, and its
/// answer — present, or cleanly absent — is held for the source's lifetime.
/// See [`HttpRangeSource::layout`].
pub struct HttpRangeSource {
    client: Client,
    url: Url,
    /// Every part except the last is exactly this long. [`PART_BYTES`] outside
    /// the tests; a parameter so a 419 KB fixture can exercise the boundary
    /// arithmetic without a 500 MB fixture.
    part_bytes: u64,
    /// The probe's verdict, taken once and held for the source's lifetime.
    ///
    /// In an `Arc` because [`RangeSource::read_range`] returns a `'static`
    /// future — everything it touches is cloned in — and the verdict must be
    /// shared across those clones rather than re-probed per read.
    layout: Arc<std::sync::OnceLock<ArchiveLayout>>,
    /// Which part the archive ends in, learned the first time a read runs
    /// short and then held like [`Self::layout`] is.
    ///
    /// A short answer from part `k` is the archive ending *or* a part that
    /// broke the publish contract, and the two are told apart by asking
    /// whether `part(k+1)` exists. Held so that costs one request per source
    /// rather than one per short read: the archive is immutable for the
    /// source's lifetime (see [`RangeSource::archive_identity`]), so where it
    /// ends cannot move under a reader.
    last_part: Arc<std::sync::OnceLock<u64>>,
}

/// How the archive is published at [`HttpRangeSource::url`].
///
/// Decided by the probe in [`HttpRangeSource::layout_for`], never by
/// configuration, so a generation can change shape without any client
/// setting changing with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArchiveLayout {
    /// One file at the bare URL. The compatibility and local-mirror shape;
    /// new generations are published as parts.
    Monolith,
    /// [`PART_BYTES`]-byte slices at `<url>.partNNN`.
    Parts,
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
    /// A source reading `url` through `client`, with parts of [`PART_BYTES`].
    ///
    /// # Errors
    ///
    /// Returns the string that would not parse as a URL.
    pub fn new(client: Client, url: &str) -> Result<Self, RangeError> {
        Self::with_part_bytes(client, url, PART_BYTES)
    }

    /// A source reading `url` as **one file**, with the layout probe skipped.
    ///
    /// For an archive that cannot be published as parts because nothing
    /// published it: a downloaded sub-archive is written whole by this
    /// client, at [`crate::basemap_download::SEGMENT_BYTES`] — three orders
    /// of magnitude under the [`PART_BYTES`] ceiling the parts exist for. The
    /// probe against such a URL can only ever cost one request per segment to
    /// be told what is already known, and on web that request goes to the
    /// service worker holding the segment.
    ///
    /// This selects a *value* the probe would otherwise decide; it is not a
    /// second read path. Everything after the layout verdict is
    /// [`Self::new`]'s code unchanged.
    ///
    /// # Errors
    ///
    /// Returns the string that would not parse as a URL.
    pub fn monolith(client: Client, url: &str) -> Result<Self, RangeError> {
        let source = Self::with_part_bytes(client, url, PART_BYTES)?;
        let _ = source.layout.set(ArchiveLayout::Monolith);
        Ok(source)
    }

    /// [`Self::new`] with the part stride overridden.
    ///
    /// Test-gated rather than `pub`: the stride is the publish side's
    /// contract, and a call site that could pick its own would be a client
    /// quietly disagreeing with the publisher about where every part
    /// boundary is. The tests need it so the 419 KB fixture can have
    /// boundaries at all. Gated exactly as its only caller (`mod tests`,
    /// native-only) is, so the wasm32 test target does not warn it unused.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn with_small_parts(
        client: Client,
        url: &str,
        part_bytes: u64,
    ) -> Result<Self, RangeError> {
        Self::with_part_bytes(client, url, part_bytes)
    }

    fn with_part_bytes(client: Client, url: &str, part_bytes: u64) -> Result<Self, RangeError> {
        assert!(part_bytes > 0, "a zero part stride divides by zero");
        let url = Url::parse(url)
            .map_err(|error| RangeError::Transport(format!("{url} is not a URL: {error}")))?;

        Ok(Self {
            client,
            url,
            part_bytes,
            layout: Arc::new(std::sync::OnceLock::new()),
            last_part: Arc::new(std::sync::OnceLock::new()),
        })
    }

    /// The URL of part `index`: the archive URL with `.partNNN` appended to
    /// its path — `NNN` three digits, zero-padded, growing past three only if
    /// an archive ever exceeds 500 GB.
    ///
    /// Appended to the *path* rather than to the serialized URL, so a query
    /// string, if one ever appears, stays behind the part suffix rather than
    /// in the middle of it.
    fn part_url(base: &Url, index: u64) -> Url {
        let mut url = base.clone();
        url.set_path(&format!("{}.part{index:03}", base.path()));
        url
    }
}

impl RangeSource for HttpRangeSource {
    /// The archive URL.
    ///
    /// A published archive's generation is *in its URL*
    /// (`omt-20260828.pmtiles`, `7c94bc6966ab-20260829/...`), so a new
    /// generation is a new name and the promise [`RangeSource::archive_identity`]
    /// makes holds without anything here having to check a header or an ETag.
    /// The `.partNNN` suffix is derived from this URL rather than carried
    /// beside it, so the parts layout does not give one archive two names.
    fn archive_identity(&self) -> Option<String> {
        Some(self.url.to_string())
    }

    fn read_range(
        &self,
        offset: u64,
        length: usize,
    ) -> impl Future<Output = Result<Vec<u8>, RangeError>> + Send {
        let client = self.client.clone();
        let url = self.url.clone();
        let part_bytes = self.part_bytes;
        let layout = Arc::clone(&self.layout);
        let last_part = Arc::clone(&self.last_part);

        async move {
            match Self::layout_for(&layout, &client, &url).await? {
                ArchiveLayout::Monolith => read_ranged(&client, &url, offset, length).await,
                ArchiveLayout::Parts => {
                    let mut bytes = Vec::with_capacity(length);

                    for span in part_spans(offset, length, part_bytes) {
                        let part = Self::part_url(&url, span.part);
                        let chunk = read_ranged(&client, &part, span.offset, span.length).await?;
                        // An over-long chunk (a server answering more than the
                        // range asked) is clamped so the next span still lands
                        // at its right global offset.
                        let got = chunk.len().min(span.length);
                        bytes.extend_from_slice(&chunk[..got]);
                        if got < span.length {
                            // The part ran out inside the span. By the publish
                            // contract only the archive's FINAL part is short,
                            // which would make this the archive ending and a
                            // short answer right — `read_range` reads *up to*
                            // `length`. But that contract is the publisher's
                            // word, and nothing in the bytes carries it: a
                            // stride the publisher and `PART_BYTES` disagree
                            // on, or an upload that stopped early, is a
                            // mid-sequence short part, and reading it as the
                            // end returns `Ok` with fewer bytes than were
                            // asked for. That short answer is then cached to
                            // disk by [`block_cache::BlockCachedSource`], so
                            // one bad second at the origin would be served
                            // short for the rest of the generation. So the
                            // claim is CHECKED rather than assumed, once per
                            // source.
                            Self::check_short_part_is_the_last(
                                &client, &url, &last_part, part_bytes, span, got,
                            )
                            .await?;
                            break;
                        }
                    }

                    Ok(bytes)
                }
            }
        }
    }
}

/// One part-local read of a global range, produced by [`part_spans`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PartSpan {
    /// Which part, counting from `part000`.
    part: u64,
    /// Offset within that part.
    offset: u64,
    /// Bytes to read from that offset. Never zero, and never crosses the
    /// part's end.
    length: usize,
}

/// Map a global `(offset, length)` onto per-part reads, in archive order.
///
/// **The whole of the parts arithmetic, in one place.** Both transports reach
/// it through the one [`RangeSource::read_range`] above — the `cfg` split in
/// [`transport`] selects where a request is driven, never how a range is cut.
/// Part `k` holds global bytes `[k * part_bytes, (k + 1) * part_bytes)`, so a
/// read spanning a boundary becomes two (rarely more) spans whose
/// concatenation, in order, is exactly the global range.
///
/// A `length` of zero maps to no spans, and a span never has length zero: a
/// read ending exactly on a part boundary ends with the earlier part's last
/// byte and must not touch the next part — requesting zero bytes of `partN+1`
/// would 404 on the archive's last boundary and turn a correct read into a
/// fault.
fn part_spans(offset: u64, length: usize, part_bytes: u64) -> Vec<PartSpan> {
    let mut spans = Vec::new();
    let mut at = offset;
    let mut remaining = length as u64;

    while remaining > 0 {
        let part = at / part_bytes;
        let local = at % part_bytes;
        let take = remaining.min(part_bytes - local);
        spans.push(PartSpan {
            part,
            offset: local,
            length: take as usize,
        });
        at += take;
        remaining -= take;
    }

    spans
}

impl HttpRangeSource {
    /// The archive's published shape, probed once and then held.
    ///
    /// The probe asks for the first bytes of `<url>.part000`. Any success —
    /// a `206`, or a `200` from a host ignoring the `Range` header — proves
    /// the part exists and selects [`ArchiveLayout::Parts`]; a *clean*
    /// absence ([`is_clean_absence`]) selects [`ArchiveLayout::Monolith`].
    /// Anything else — a timeout, a `5xx`, a `403` — is a fault: it is
    /// retried up to [`RANGE_ATTEMPTS`] times and then **returned as the
    /// error it is**, never read as "no parts". A source whose first read is
    /// this probe failing therefore fails [`BasemapArchive::open`], which is
    /// the path that reaches the app's archive fault latch; the alternative
    /// is silently holding monolith mode for the source's lifetime because
    /// the origin had a bad second at open.
    ///
    /// The verdict is logged once, at the `OnceLock` set. The first read an
    /// archive open performs is what drives this, so in practice the probe
    /// runs before any read concurrency exists; if two reads ever race it,
    /// the second `set` loses and the extra probe cost one request.
    async fn layout_for(
        layout: &std::sync::OnceLock<ArchiveLayout>,
        client: &Client,
        url: &Url,
    ) -> Result<ArchiveLayout, RangeError> {
        if let Some(&decided) = layout.get() {
            return Ok(decided);
        }

        let verdict = Self::probe(client, url).await?;
        if layout.set(verdict).is_ok() {
            match verdict {
                ArchiveLayout::Parts => log::info!("basemap archive: part mode at {url}"),
                ArchiveLayout::Monolith => log::info!("basemap archive: monolith mode at {url}"),
            }
        }
        Ok(verdict)
    }

    /// One probe of `<url>.part000`, with the retry bounded like every other
    /// range request. See [`Self::layout_for`] for what each answer means.
    async fn probe(client: &Client, url: &Url) -> Result<ArchiveLayout, RangeError> {
        if Self::part_exists(client, url, 0).await? {
            Ok(ArchiveLayout::Parts)
        } else {
            Ok(ArchiveLayout::Monolith)
        }
    }

    /// Whether `<url>.partNNN` is there, retried like every other range
    /// request.
    ///
    /// Any success — a `206`, or a `200` from a host ignoring the `Range`
    /// header — proves the part exists; a *clean* absence
    /// ([`is_clean_absence`]) proves it does not. Anything else — a timeout, a
    /// `5xx`, a `403` — is a host failing to answer and is **returned as the
    /// error it is**, because both callers turn "absent" into a durable
    /// verdict (the archive's shape, or where the archive ends) and reading an
    /// origin's bad second as absence would hold the wrong one for the
    /// source's lifetime.
    async fn part_exists(client: &Client, url: &Url, index: u64) -> Result<bool, RangeError> {
        let part = Self::part_url(url, index);
        // First bytes only. On a `200` the body — up to 500 MB of part — is
        // dropped unread, exactly as `execute` does for every range request.
        let range = "bytes=0-15".to_owned();
        let mut last_error = None;

        for attempt in 1..=RANGE_ATTEMPTS {
            match transport::fetch(client.clone(), part.clone(), range.clone()).await {
                Ok(RangeReply::Range { .. } | RangeReply::NotRanged { .. }) => return Ok(true),
                Ok(RangeReply::Failed { status }) if is_clean_absence(status) => return Ok(false),
                Ok(RangeReply::Failed { status }) => {
                    log::debug!(
                        "{part} answered {status} to the part probe (attempt {attempt} of \
                         {RANGE_ATTEMPTS})"
                    );
                    last_error = Some(RangeError::Transport(format!(
                        "{part} answered {status} to the part probe; neither part nor a clean \
                         absence, after {attempt} attempts"
                    )));
                }
                Err(error @ RangeError::Cancelled) => return Err(error),
                Err(error) => {
                    log::debug!(
                        "{part} probe failed (attempt {attempt} of {RANGE_ATTEMPTS}): {error}"
                    );
                    last_error = Some(error);
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            RangeError::Transport(format!("{part}: the part probe made no attempt"))
        }))
    }

    /// Establish that `span`'s part running short is the archive ending, not
    /// a part that broke the publish contract.
    ///
    /// The whole of the check the parts loop's short branch used to assume.
    /// A part answering fewer bytes than its span asked for has two readings —
    /// the archive ends inside it, or it is a mid-sequence part that is
    /// shorter than [`PART_BYTES`] — and only the first makes a short `Ok`
    /// the right answer. They are told apart by whether the *next* part
    /// exists, which is one 16-byte request, taken **once per source**: the
    /// archive is immutable for the source's lifetime, so the verdict is held
    /// in `last_part` and every later short read reads it for free.
    ///
    /// # Errors
    ///
    /// [`RangeError::ShortPart`] when a later part is there, so the archive
    /// provably did not end. A probe that will not answer surfaces as its own
    /// error rather than as either verdict.
    async fn check_short_part_is_the_last(
        client: &Client,
        url: &Url,
        last_part: &std::sync::OnceLock<u64>,
        part_bytes: u64,
        span: PartSpan,
        got: usize,
    ) -> Result<(), RangeError> {
        let short = RangeError::ShortPart {
            part: span.part,
            got,
            wanted: span.length,
            stride: part_bytes,
        };

        if let Some(&last) = last_part.get() {
            return if span.part == last {
                Ok(())
            } else {
                Err(short)
            };
        }

        if Self::part_exists(client, url, span.part + 1).await? {
            return Err(short);
        }

        if last_part.set(span.part).is_ok() {
            log::info!("basemap archive: {url} ends in part{:03}", span.part);
        }
        Ok(())
    }
}

/// One ranged read of `url`, retried per [`RANGE_ATTEMPTS`] when the host
/// answers success-but-not-`206`.
///
/// The retry loop [`HttpRangeSource`] has always had, hoisted out of
/// `read_range` so the monolith read and every per-part read of a stitched
/// span go through the identical discipline.
async fn read_ranged(
    client: &Client,
    url: &Url,
    offset: u64,
    length: usize,
) -> Result<Vec<u8>, RangeError> {
    // `bytes=N-M`, inclusive at both ends, which is the one form the CORS
    // safelist covers — so no preflight, which is what makes reading the
    // archive straight off object storage viable at all.
    let end = offset.saturating_add(length as u64).saturating_sub(1);
    let range = format!("bytes={offset}-{end}");
    let mut last_status = 0;

    for attempt in 1..=RANGE_ATTEMPTS {
        match transport::fetch(client.clone(), url.clone(), range.clone()).await? {
            RangeReply::Range { bytes } => return Ok(bytes),
            RangeReply::NotRanged { status } => {
                log::debug!(
                    "{url} answered {status} rather than {STATUS_PARTIAL_CONTENT} for {range} \
                     (attempt {attempt} of {RANGE_ATTEMPTS})"
                );
                last_status = status;
            }
            // A failure status is not a range problem and is not retried —
            // pinned by `a_failure_status_is_reported_rather_than_retried`.
            RangeReply::Failed { status } => {
                return Err(RangeError::Transport(format!(
                    "{url} answered {status} for {range}"
                )));
            }
        }
    }

    Err(RangeError::NotRanged {
        status: last_status,
        attempts: RANGE_ATTEMPTS,
    })
}

/// What one HTTP attempt came back with.
///
/// A failure *status* is a reply, not an `Err`: the caller decides what it
/// means. To [`read_ranged`] any failure is a transport error; to
/// [`HttpRangeSource::probe`] a `404` is the answer "this archive has no
/// parts", and flattening it into an error string is what made that
/// distinction impossible to draw.
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
    /// A failure status. The body was not read.
    Failed {
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
        // A reply, not an error: the probe reads a `404` here as "no parts",
        // which an error string cannot carry. The body is dropped unread.
        drop(response);
        return Ok(RangeReply::Failed {
            status: status.as_u16(),
        });
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

    /// The bytes, if the tile is present, **moved out**.
    ///
    /// The rendering side hands them to a blocking task, which has to own them;
    /// `bytes().to_vec()` would copy a body that is already on the heap and
    /// about to be dropped.
    pub fn into_bytes(self) -> Option<Vec<u8>> {
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
    /// Read one tile's stored bytes and drop them, answering whether the
    /// archive held the tile.
    ///
    /// The seed's verb ([`block_cache::seed_shallow`]): it pulls the tile's
    /// range through the source — which is the point, the source is the
    /// caching wrapper — without paying [`Self::tile`]'s decompression for
    /// bytes nobody will look at.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn warm_tile(&self, z: u8, x: u32, y: u32) -> Result<bool, ArchiveError> {
        let coord = TileCoord::new(z, x, y).map_err(|_| ArchiveError::Coordinate { z, x, y })?;

        Ok(self
            .reader
            .get_tile(coord)
            .await
            .map_err(ArchiveError::Tile)?
            .is_some())
    }

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

// ---------------------------------------------------------------------------
// The composition
// ---------------------------------------------------------------------------

/// What one archive's own header says it could hold: a zoom band and a bbox.
///
/// Split out of [`OfflineSegment`] so the predicate is testable without an
/// archive behind it — it is the whole of what keeps a per-tile walk over N
/// local archives off the wire, and a rejection rule that can only be
/// exercised through a file is a rejection rule nothing pins directly.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Coverage {
    min_zoom: u8,
    max_zoom: u8,
    /// The area's bbox, as the writer declared it in the segment's header.
    west: f64,
    south: f64,
    east: f64,
    north: f64,
}

impl Coverage {
    /// Whether an archive with this header could hold `z/x/y`.
    ///
    /// **Conservative in one direction only**: `false` means it provably does
    /// not hold the tile, `true` means the directories have to be asked. The
    /// bbox is turned into a tile range with `squallar_geo::lon_to_tile_x`
    /// and `lat_to_tile_y` — the same two functions
    /// `basemap_download::area_tiles` enumerated the segment's contents with,
    /// so the enumeration and the rejection cannot round to different edges.
    fn holds(&self, z: u8, x: u32, y: u32) -> bool {
        if z < self.min_zoom || z > self.max_zoom {
            return false;
        }
        let west = squallar_geo::lon_to_tile_x(self.west, z);
        let east = squallar_geo::lon_to_tile_x(self.east, z);
        let north = squallar_geo::lat_to_tile_y(self.north, z);
        let south = squallar_geo::lat_to_tile_y(self.south, z);
        (west..=east).contains(&x) && (north..=south).contains(&y)
    }
}

/// One opened local sub-archive, with what its own header says it holds.
///
/// The [`Coverage`] is read at open and never re-read; answering from it
/// touches the source not at all. See [`BasemapArchives::tile`] for why that
/// matters more than it looks.
struct OfflineSegment<O: ArchiveRangeSource> {
    /// What to call it in a log. The store's own name for the artifact.
    label: String,
    archive: BasemapArchive<O>,
    coverage: Coverage,
}

/// The live archive with any locally downloaded sub-archives in front of it.
///
/// # It is a second source of ranges, not a second reader
///
/// Every archive here is a [`BasemapArchive`] over a [`RangeSource`] — the
/// same reader, the same directory cache, the same [`TileBytes`] vocabulary.
/// What this type adds is an *order*, and the order is the whole feature:
/// [`Self::tile`] asks the local segments first and the live archive only
/// after all of them have positively answered `Absent`. **A local hit that
/// deferred to the network would spend exactly the bandwidth this feature
/// exists to save.**
///
/// Partial coverage is therefore automatic and per-tile. Nothing anywhere
/// asks "is the viewport covered": half a viewport served locally and half
/// over the network is not a state to detect, report or warn about, it is
/// what the walk does on every tile independently.
///
/// # Per tile, N segments do not cost N reads
///
/// Two things keep the walk cheap, and neither is a cache that might miss:
///
/// * `Coverage::holds` rejects a segment from its header's own bbox and zoom
///   band, in memory, before any directory is consulted. The
///   download engine cuts an area into zoom-banded segments, so at a given
///   zoom most of an area's segments are rejected on the zoom alone.
/// * A segment that passes is asked through [`BasemapArchive::tile`], whose
///   `HashMapCache` holds the root directory from `open` onward. On a
///   segment small enough to have no leaf directories — 16 MB is, by a wide
///   margin — an `Absent` answer is decided in memory and costs **zero**
///   range reads. A segment with leaves pays one read per leaf, once.
///
/// # A local archive is never allowed to fail the tile
///
/// A segment that will not open is skipped at [`BasemapArchives::attach_offline`] with
/// a log, and one that errors mid-life is skipped at [`BasemapArchives::tile`] with a
/// log. Both degrade to "fetch it from the network", which is what walking
/// on to the live archive already does — so a corrupt local file costs
/// bandwidth, never a blank map, and never a notice the user cannot act on.
pub struct BasemapArchives<L: ArchiveRangeSource, O: ArchiveRangeSource> {
    /// In an `Arc` because the block-cache seed holds the live archive while
    /// this composition serves from it; the seed is the live archive's alone.
    live: Arc<BasemapArchive<L>>,
    offline: Vec<OfflineSegment<O>>,
    /// [`Self::max_zoom`], recomputed on every attach so the render ceiling
    /// is one number rather than a walk per frame.
    max_zoom: u8,
    min_zoom: u8,
}

impl<L: ArchiveRangeSource, O: ArchiveRangeSource> BasemapArchives<L, O> {
    /// Open the live archive. Local segments are attached afterwards, one at
    /// a time, so that a store that answers slowly or not at all cannot delay
    /// the header the frame side is waiting on.
    ///
    /// # Errors
    ///
    /// [`ArchiveError::Open`], exactly as [`BasemapArchive::open`] — the live
    /// archive failing is still a fault, because a composition with no live
    /// archive can only ever serve what happens to be downloaded.
    pub async fn open(live: L) -> Result<Self, ArchiveError> {
        let live = BasemapArchive::open(live).await?;
        let max_zoom = live.max_zoom();
        let min_zoom = live.min_zoom();
        Ok(Self {
            live: Arc::new(live),
            offline: Vec::new(),
            max_zoom,
            min_zoom,
        })
    }

    /// Put one local sub-archive in front of the live one, answering whether
    /// it was taken.
    ///
    /// **Never fails, never panics, never reaches the glass.** A segment that
    /// will not open, or whose bodies are not the kind the live archive's are,
    /// is logged and skipped — the tiles it would have served come from the
    /// network instead, which is the composition working rather than failing.
    /// The tile-type check is what stops a hand-placed raster archive being
    /// fed to the vector decoder the live header selected.
    pub async fn attach_offline(&mut self, label: String, source: O) -> bool {
        let archive = match BasemapArchive::open(source).await {
            Ok(archive) => archive,
            Err(error) => {
                log::warn!(
                    "the downloaded basemap segment {label} will not open, so its tiles come \
                     from the network instead: {error}"
                );
                return false;
            }
        };

        if archive.tile_type() != self.live.tile_type() {
            log::warn!(
                "the downloaded basemap segment {label} holds {:?} tiles where the archive holds \
                 {:?}, so it is not used",
                archive.tile_type(),
                self.live.tile_type()
            );
            return false;
        }

        let header = archive.header();
        let coverage = Coverage {
            min_zoom: header.min_zoom,
            max_zoom: header.max_zoom,
            west: header.min_longitude,
            south: header.min_latitude,
            east: header.max_longitude,
            north: header.max_latitude,
        };
        log::info!("downloaded basemap segment {label} in front of the archive: {coverage:?}");
        self.max_zoom = self.max_zoom.max(coverage.max_zoom);
        self.min_zoom = self.min_zoom.min(coverage.min_zoom);
        self.offline.push(OfflineSegment {
            label,
            archive,
            coverage,
        });
        true
    }

    /// The live archive, for the one caller that is about the live archive
    /// alone: the block cache's shallow-zoom seed.
    pub fn live(&self) -> &Arc<BasemapArchive<L>> {
        &self.live
    }

    /// How many local segments are in front of the live archive.
    pub fn offline_count(&self) -> usize {
        self.offline.len()
    }

    /// The live archive's header — what the bodies are and how they are
    /// compressed. A segment is only attached if its own header agrees, so
    /// there is one answer rather than a per-source one.
    pub fn header(&self) -> &Header {
        self.live.header()
    }

    /// The deepest zoom anything in the composition stores.
    ///
    /// **The ceiling is the live archive's unless a local segment is deeper**,
    /// which a segment cut from that archive can never be. So over-zoom past
    /// a downloaded area's own maximum is not a case: the published ceiling
    /// does not move, `Tiles::at` clamps against the same number it always
    /// did, and the stretched ancestor comes from
    /// `HttpsTiles::cached_or_interpolated` exactly as it does past the live
    /// archive's own maximum. There is no second over-zoom rule and no string
    /// to show for one.
    pub fn max_zoom(&self) -> u8 {
        self.max_zoom
    }

    /// The shallowest zoom anything in the composition stores.
    pub fn min_zoom(&self) -> u8 {
        self.min_zoom
    }

    /// What the tiles are. The live archive's word — see [`Self::header`].
    pub fn tile_type(&self) -> TileType {
        self.live.tile_type()
    }

    /// How the tiles are compressed on the wire. [`Self::tile`] has already
    /// undone this by the time it answers.
    pub fn tile_compression(&self) -> Compression {
        self.live.tile_compression()
    }

    /// Fetch one tile, decompressed, from the first source that holds it.
    ///
    /// Local segments first, in attach order, then the live archive. See the
    /// type doc for why that order is the feature and why it costs no reads
    /// to walk.
    ///
    /// # Errors
    ///
    /// [`ArchiveError::Coordinate`] if `z/x/y` is not a tile, and whatever the
    /// **live** archive answers with. A local segment's failure is never one
    /// of these: it is logged and walked past.
    pub async fn tile(&self, z: u8, x: u32, y: u32) -> Result<TileBytes, ArchiveError> {
        // Once, here, rather than once per source: an off-grid coordinate is
        // the caller's error and every archive would answer it identically.
        TileCoord::new(z, x, y).map_err(|_| ArchiveError::Coordinate { z, x, y })?;

        for segment in &self.offline {
            if !segment.coverage.holds(z, x, y) {
                continue;
            }
            match segment.archive.tile(z, x, y).await {
                Ok(TileBytes::Present(bytes)) => return Ok(TileBytes::Present(bytes)),
                Ok(TileBytes::Absent) => {}
                Err(error) => log::warn!(
                    "the downloaded basemap segment {} would not answer {z}/{x}/{y}, so it is \
                     read from the network instead: {error}",
                    segment.label
                ),
            }
        }

        self.live.tile(z, x, y).await
    }
}

/// The persistent block cache over [`RangeSource`], and the background seed
/// that warms it. A submodule of the archive reader, beside the seam
/// gate rather than carrying one of its own.
pub mod block_cache;

/// `pub(crate)` for its loopback harness, which two suites outside this
/// module read: `block_cache::tests` and `tiles::height_tests`. A second
/// loopback server written beside it could disagree with this one about what
/// a range request is, and every one of those suites' assertions is a range.
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod tests;

/// The composition's own suite. Beside [`tests`] rather than inside it
/// because what it measures is different: [`tests`] gates one reader over one
/// source, this gates the *order* two readers are asked in, and every
/// assertion in it is a read count.
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod archives_tests;

/// The pin on offsets above 4 GiB — the defect that made the self-hosted
/// basemap draw nothing at all in a browser. Its own file, and not folded into
/// [`tests`], because half of it is a **compile-time** proof about the width of
/// [`RangeBackend`]'s seam rather than a runtime assertion, and that half is
/// the only part of it that a 64-bit builder cannot pass vacuously. Read its
/// module doc before believing a green run.
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod four_gib_offset_tests;
