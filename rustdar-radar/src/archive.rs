//! Anonymous S3 access to the NEXRAD Level II archive bucket.
//!
//! This is rustdar's own replacement for `nexrad_data::aws::archive`, which was
//! the last thing in the workspace pulling `aws-lc-sys` into the graph:
//! `nexrad-data`'s `aws` feature turns on `reqwest/rustls`, which resolves to
//! `__rustls-aws-lc-rs`, which drags in a second C crypto stack alongside the
//! *ring* one [`crate::tls`] already installs. Turning `aws` off deletes
//! `Identifier`, `list_files` and `download_file` along with it, so they are
//! reimplemented here against the same bucket, over the same client every other
//! rustdar request uses.
//!
//! `nexrad-data` is still a dependency and still does all the *decoding* --
//! [`nexrad_data::volume::File`] and its `scan()` are untouched. Only the HTTP
//! layer moved.
//!
//! # Why this needs no credentials
//!
//! `unidata-nexrad-level2` is a public, anonymously readable bucket: a bare
//! `GET https://unidata-nexrad-level2.s3.amazonaws.com/?list-type=2&prefix=...`
//! with no `Authorization` header returns `200` and a `ListBucketResult`. That
//! is what keeps this module small -- there is no SigV4 canonical request, no
//! credential chain and no clock skew handling, because an anonymous request is
//! never signed. `nexrad-data` did not sign either.
//!
//! # Bucket layout
//!
//! Objects are keyed `YYYY/MM/DD/SITE/SITEYYYYMMDD_HHMMSS_V06`, so a listing for
//! one site-day is a prefix query on `YYYY/MM/DD/SITE`. Alongside each volume
//! file the bucket also carries a `..._V06_MDM` metadata sidecar; upstream
//! returned those in the listing too and [`list_files`] deliberately still does
//! (see the note on [`key_to_identifier`]).

use std::sync::OnceLock;

use chrono::NaiveDate;
use reqwest::StatusCode;
use xml::reader::{EventReader, XmlEvent};

/// The public NOAA/Unidata bucket holding Level II archive volumes.
///
/// Kept as a `const` because it is used in `const`-ish positions and
/// [`crate::sources::DataSources`] holds its origins as `Cow`, which cannot be
/// dereferenced in a constant. `archive_bucket_matches_the_declared_origin`
/// pins the two together so this cannot drift.
pub const ARCHIVE_BUCKET: &str = "unidata-nexrad-level2";

/// How long a single archive request may take, end to end.
///
/// Upstream `nexrad-data` built its S3 client with **no** timeout, so a stalled
/// connection hung the fetch task -- and with it the pane's "fetching" state --
/// forever, with no error ever reaching the UI. This is the one error path this
/// module adds that upstream did not have. It is deliberately generous: archive
/// volumes run to ~9 MB, so this has to tolerate a slow link rather than merely
/// a slow server, and it exists to bound a hang, not to enforce latency.
const ARCHIVE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Upper bound on `ListObjectsV2` pages followed for one listing.
///
/// Purely a liveness guard. A real site-day is 200-350 keys, i.e. a single
/// page, and even a pathological one cannot approach 100 000; but the paging
/// loop is driven by a token the server chooses, and a server that kept
/// returning `IsTruncated` would otherwise spin forever inside a UI fetch.
const MAX_LIST_PAGES: usize = 100;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Failures reaching or interpreting the archive bucket.
///
/// This replaces the `nexrad_data::result::Error::AWS` variant, which is
/// `#[cfg(feature = "aws")]` upstream and therefore disappears along with the
/// feature. Every consumer in this workspace renders scan errors with `{:?}`,
/// so the variants are shaped for a legible debug print rather than for
/// matching.
#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    /// The HTTP request could not be completed.
    #[error("S3 request failed: {0}")]
    Request(#[from] reqwest::Error),

    /// The bucket answered `404` for the requested object.
    ///
    /// Kept distinct from [`ArchiveError::Status`] because upstream did the
    /// same, and because "this volume is not in the archive" is an ordinary
    /// outcome while "the bucket returned 503" is not.
    #[error("S3 object not found: {0}")]
    NotFound(String),

    /// The bucket answered with some other non-`200` status.
    #[error("S3 returned {status} for {url}{}", body.as_deref().map(|b| format!(": {b}")).unwrap_or_default())]
    Status {
        /// The status the bucket returned.
        status: StatusCode,
        /// The URL that was requested.
        url: String,
        /// The response body, when one could be read.
        body: Option<String>,
    },

    /// A `ListObjectsV2` response could not be understood.
    #[error("malformed S3 listing: {0}")]
    MalformedListing(String),

    /// An [`Identifier`] carries a name no object key can be derived from.
    #[error("cannot derive an archive key from identifier {0:?}")]
    UnkeyableIdentifier(String),
}

/// Convenience alias for this module's fallible operations.
pub type Result<T> = std::result::Result<T, ArchiveError>;

// ---------------------------------------------------------------------------
// Identifier
// ---------------------------------------------------------------------------

/// Identifying metadata for a NEXRAD archive volume file.
///
/// A faithful port of `nexrad_data::aws::archive::Identifier`: a newtype over
/// the bare object *name* (not the full key), with the site and collection time
/// recovered by fixed-offset slicing of that name. The derive list is
/// upstream's plus `Debug`, which upstream omitted: the frontend logs an
/// identifier when a loop frame fails to download, and every consumer in this
/// workspace renders errors with `{:?}`.
///
/// Names look like `KTLX20240520_000004_V06`: four characters of site, eight of
/// `%Y%m%d`, a separator, then six of `%H%M%S`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct Identifier(String);

impl Identifier {
    /// Constructs a new identifier from the provided name.
    pub fn new(name: String) -> Self {
        Identifier(name)
    }

    /// The file name.
    pub fn name(&self) -> &str {
        &self.0
    }

    /// The radar site this file was produced at, e.g. `KDMX`.
    ///
    /// `get(0..4)` rather than indexing: names come from bucket keys, so a
    /// short or non-ASCII name must yield `None` rather than panic.
    pub fn site(&self) -> Option<&str> {
        self.0.get(0..4)
    }

    /// This file's data collection time.
    pub fn date_time(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        let date_string = self.0.get(4..12)?;
        let date = NaiveDate::parse_from_str(date_string, "%Y%m%d").ok()?;
        let time_string = self.0.get(13..19)?;
        let time = chrono::NaiveTime::parse_from_str(time_string, "%H%M%S").ok()?;
        Some(chrono::DateTime::from_naive_utc_and_offset(
            chrono::NaiveDateTime::new(date, time),
            chrono::Utc,
        ))
    }
}

// ---------------------------------------------------------------------------
// URLs
// ---------------------------------------------------------------------------

/// Build the `ListObjectsV2` URL for one page of a prefix query.
///
/// Every parameter goes through `query_pairs_mut`, which percent-encodes.
/// Upstream interpolated the prefix straight into a format string, which is
/// safe for the `YYYY/MM/DD/SITE` prefixes this module builds but is not safe
/// for the continuation token: tokens are opaque base64-ish blobs that routinely
/// contain `/` and can contain `+` and `=`, and an unencoded `+` would reach S3
/// as a space and be rejected.
pub(crate) fn list_url(
    bucket: &str,
    prefix: &str,
    max_keys: Option<u32>,
    continuation_token: Option<&str>,
) -> Result<String> {
    let mut url = reqwest::Url::parse(&format!("https://{bucket}.s3.amazonaws.com/"))
        .map_err(|e| ArchiveError::MalformedListing(format!("bad bucket {bucket:?}: {e}")))?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("list-type", "2");
        query.append_pair("prefix", prefix);
        if let Some(max_keys) = max_keys {
            query.append_pair("max-keys", &max_keys.to_string());
        }
        if let Some(token) = continuation_token {
            query.append_pair("continuation-token", token);
        }
    }
    Ok(url.into())
}

/// Build the object URL for a full bucket key.
///
/// The key is interpolated rather than encoded, matching upstream. Archive keys
/// are drawn from `[A-Za-z0-9_/]` -- date, site and volume name -- so there is
/// nothing to encode, and encoding would have to leave the `/` separators alone
/// anyway.
fn object_url(bucket: &str, key: &str) -> String {
    crate::sources::DataSources::s3_object_url(bucket, key)
}

/// The prefix under which one site's volumes for one day are stored.
fn day_prefix(site: &str, date: &NaiveDate) -> String {
    format!("{}/{}", date.format("%Y/%m/%d"), site)
}

/// Recover a volume name from a full bucket key.
///
/// Upstream is `key.split('/').skip(4).collect::<String>()`, and this reproduces
/// it exactly, including two behaviours that look like accidents but are load
/// bearing downstream:
///
/// * The four skipped segments are `YYYY`, `MM`, `DD` and `SITE`. A key with
///   fewer than five segments yields an empty name, and `crate::scan` already
///   skips identifiers whose name has no `_`-separated time field, so an
///   unexpected key is dropped rather than fatal.
/// * `collect::<String>()` concatenates any remaining segments *without*
///   re-inserting the `/`. No archive key has a sixth segment, so this never
///   fires; it is preserved so that if one ever appears, this module and the
///   upstream it replaced agree.
fn key_to_identifier(key: &str) -> Identifier {
    Identifier::new(key.split('/').skip(4).collect::<String>())
}

// ---------------------------------------------------------------------------
// ListObjectsV2 parsing
// ---------------------------------------------------------------------------

/// One page of a `ListObjectsV2` response.
#[derive(Debug, Default, PartialEq, Eq)]
struct ListPage {
    /// Keys in the order S3 returned them, which is UTF-8 binary order.
    keys: Vec<String>,
    /// Whether S3 says more keys match the prefix than fit in this page.
    truncated: bool,
    /// The cursor for the next page, present exactly when `truncated`.
    next_token: Option<String>,
}

/// Which element's character data is currently being accumulated.
#[derive(PartialEq, Eq)]
enum Field {
    Key,
    IsTruncated,
    NextToken,
}

/// Parse one `ListBucketResult` document.
///
/// Only `Contents/Key`, `IsTruncated` and `NextContinuationToken` are read; the
/// sizes, ETags and timestamps upstream also parsed were never used by
/// `list_files`, which reduced every object to its key.
///
/// `Key` is only captured inside `Contents` so that the document's own
/// `<Prefix>`/`<Name>` and any `CommonPrefixes` cannot be mistaken for objects.
/// Character data is accumulated rather than assigned, because an XML parser is
/// free to split text across several events at entity boundaries -- and
/// continuation tokens are long enough to hit that.
fn parse_list_page(body: &str) -> Result<ListPage> {
    let mut page = ListPage::default();
    let mut field: Option<Field> = None;
    let mut in_contents = false;
    let mut buffer = String::new();

    for event in EventReader::new(body.as_bytes()) {
        let event = event.map_err(|e| ArchiveError::MalformedListing(e.to_string()))?;
        match event {
            XmlEvent::StartElement { name, .. } => {
                buffer.clear();
                field = match name.local_name.as_str() {
                    "Contents" => {
                        in_contents = true;
                        None
                    }
                    "Key" if in_contents => Some(Field::Key),
                    "IsTruncated" => Some(Field::IsTruncated),
                    "NextContinuationToken" => Some(Field::NextToken),
                    _ => None,
                };
            }
            XmlEvent::Characters(chars) => {
                if field.is_some() {
                    buffer.push_str(&chars);
                }
            }
            XmlEvent::EndElement { name } => {
                match name.local_name.as_str() {
                    "Contents" => in_contents = false,
                    "Key" if field == Some(Field::Key) => {
                        page.keys.push(std::mem::take(&mut buffer));
                    }
                    "IsTruncated" if field == Some(Field::IsTruncated) => {
                        page.truncated = buffer.trim() == "true";
                    }
                    "NextContinuationToken" if field == Some(Field::NextToken) => {
                        page.next_token = Some(std::mem::take(&mut buffer));
                    }
                    _ => {}
                }
                field = None;
                buffer.clear();
            }
            _ => {}
        }
    }

    Ok(page)
}

/// Follow `NextContinuationToken` until the listing is complete.
///
/// `fetch_page` receives the fully-built URL and returns the response body, so
/// a test can observe exactly what would have gone on the wire -- in particular
/// whether the token actually reached the query string, which is a separate
/// mistake from failing to thread it through this loop.
///
/// **This is where the behaviour differs from `nexrad-data` on purpose.**
/// Upstream issued a single request and returned
/// `AWSError::TruncatedListObjectsResponse` whenever `IsTruncated` came back
/// true, so a listing that did not fit one page was a hard error rather than a
/// short result. Paging instead is strictly a superset: every listing upstream
/// could serve, this serves identically (one request, same keys, same order),
/// and the listings upstream refused now succeed. In practice the archive's
/// prefix scheme keeps a site-day to 200-350 keys, so neither path fires --
/// measured against the live bucket, the busiest days sampled
/// (2011-04-27/KBMX, 2013-05-20/KTLX) were 311 and 323 keys against a 1000-key
/// page.
///
/// Paging keys off `IsTruncated`, not off the presence of a token: S3 sends
/// both together, but keying off the token alone would turn a final page that
/// echoed one into an infinite loop.
pub(crate) async fn collect_keys<F, Fut>(
    bucket: &str,
    prefix: &str,
    max_keys: Option<u32>,
    mut fetch_page: F,
) -> Result<Vec<String>>
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Result<String>>,
{
    let mut keys = Vec::new();
    let mut token: Option<String> = None;

    for _ in 0..MAX_LIST_PAGES {
        let url = list_url(bucket, prefix, max_keys, token.as_deref())?;
        let page = parse_list_page(&fetch_page(url).await?)?;
        keys.extend(page.keys);

        if !page.truncated {
            return Ok(keys);
        }
        let Some(next) = page.next_token else {
            return Err(ArchiveError::MalformedListing(format!(
                "listing for prefix {prefix:?} is truncated but carries no \
                 NextContinuationToken; {} keys would be silently lost",
                keys.len()
            )));
        };
        token = Some(next);
    }

    Err(ArchiveError::MalformedListing(format!(
        "listing for prefix {prefix:?} did not terminate within {MAX_LIST_PAGES} pages"
    )))
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

/// The shared, pooled client for every archive request.
///
/// Built through [`crate::tls::client`], which is what installs the *ring*
/// crypto provider — with `rustls-no-provider` there is no compiled-in default,
/// and `ClientBuilder::build` panics without one. Routing through that
/// constructor rather than `reqwest::Client::builder()` is the whole reason
/// this module can exist without re-introducing a bundled root store.
///
/// One client for the process, as upstream had: `list_scans_for_range` issues a
/// listing per day and a loop replay downloads a volume per frame, so losing
/// connection reuse here would be a real regression.
pub(crate) fn shared_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        crate::tls::client(crate::tls::USER_AGENT, ARCHIVE_TIMEOUT)
            .build()
            // Matches upstream, which also panicked here. A client that cannot
            // be constructed is a build-configuration fault, not a runtime
            // condition any caller could recover from.
            .unwrap_or_else(|e| panic!("failed to build the archive HTTP client: {e}"))
    })
}

/// How a response status should be interpreted.
///
/// Split out from the request so the mapping is testable without a socket.
#[derive(Debug, PartialEq, Eq)]
enum StatusClass {
    /// `200`: the body is the object.
    Ok,
    /// `404`: the object is absent.
    NotFound,
    /// Anything else, including other 2xx.
    Failed,
}

/// Classify an S3 response status.
///
/// Only `200` is success, matching upstream: a `206` or a `304` means something
/// asked for a partial or conditional fetch that this module never requests, so
/// treating it as a complete body would hand a truncated volume to the decoder.
fn classify(status: StatusCode) -> StatusClass {
    match status {
        StatusCode::OK => StatusClass::Ok,
        StatusCode::NOT_FOUND => StatusClass::NotFound,
        _ => StatusClass::Failed,
    }
}

/// GET a URL and return the body as text, or an error describing the status.
///
/// Upstream's `list_objects` never looked at the status at all: it fed whatever
/// came back to the XML parser, so a `403` or a `503` produced an *empty
/// listing* rather than an error. `crate::scan::list_files_with_fallback` reads
/// an empty listing as "no data for this date", falls back to the previous day,
/// gets the same empty result and reports "no scans available" -- an outage
/// rendered as an absence. Checking the status here is the second deliberate
/// departure from upstream.
pub(crate) async fn get_text(client: &reqwest::Client, url: String) -> Result<String> {
    let response = client.get(&url).send().await?;
    match classify(response.status()) {
        StatusClass::Ok => Ok(response.text().await?),
        StatusClass::NotFound => Err(ArchiveError::NotFound(url)),
        StatusClass::Failed => {
            let status = response.status();
            let body = response.text().await.ok();
            Err(ArchiveError::Status { status, url, body })
        }
    }
}

/// GET a URL and return the body as bytes.
///
/// The binary counterpart of [`get_text`], with the same status handling: a
/// `404` is [`ArchiveError::NotFound`] and anything else non-`200` is
/// [`ArchiveError::Status`], so a bucket outage cannot be mistaken for an
/// absent object.
pub(crate) async fn get_bytes(client: &reqwest::Client, url: String) -> Result<Vec<u8>> {
    let response = client.get(&url).send().await?;
    match classify(response.status()) {
        StatusClass::Ok => Ok(response.bytes().await?.to_vec()),
        StatusClass::NotFound => Err(ArchiveError::NotFound(url)),
        StatusClass::Failed => {
            let status = response.status();
            let body = response.text().await.ok();
            Err(ArchiveError::Status { status, url, body })
        }
    }
}

// NOTE: there is deliberately no byte-range helper here. The one consumer of
// HTTP `Range` in this workspace is `rustdar_overlays::hrrr::fetch`, and it
// needs semantics this module's `classify` cannot express: a `200` there means
// the server ignored the header and is about to stream a ~130 MB file, which is
// a hard error rather than something to slice locally. Keeping that logic next
// to the `.idx` arithmetic that produces the range is what makes the two
// testable together.

// ---------------------------------------------------------------------------
// Public surface
// ---------------------------------------------------------------------------

/// List data files for the specified site and date.
///
/// Returns an index of the volumes the archive holds for that site-day, in the
/// bucket's own key order, which is what `crate::scan` relies on when it breaks
/// ties between a volume and its `_MDM` sidecar by taking the first match.
pub async fn list_files(site: &str, date: &NaiveDate) -> Result<Vec<Identifier>> {
    // Resolved before the first `.await` so that merely polling this future
    // once installs the crypto provider; `crate::tls` has a probe that depends
    // on exactly that.
    let client = shared_client();
    let prefix = day_prefix(site, date);

    log::debug!("Listing archive objects for prefix {prefix:?}");
    let keys = collect_keys(ARCHIVE_BUCKET, &prefix, None, |url| get_text(client, url)).await?;
    log::debug!("Listing for {prefix:?} returned {} keys", keys.len());

    Ok(keys.iter().map(|key| key_to_identifier(key)).collect())
}

/// Download the volume file a given identifier names.
///
/// Returns the encoded contents wrapped in [`nexrad_data::volume::File`], which
/// still owns decompression and decoding.
pub async fn download_file(identifier: Identifier) -> Result<nexrad_data::volume::File> {
    let client = shared_client();

    let date = identifier
        .date_time()
        .ok_or_else(|| ArchiveError::UnkeyableIdentifier(identifier.name().to_string()))?;
    let site = identifier
        .site()
        .ok_or_else(|| ArchiveError::UnkeyableIdentifier(identifier.name().to_string()))?;

    let key = format!(
        "{}/{}/{}",
        date.format("%Y/%m/%d"),
        site,
        identifier.name()
    );
    let url = object_url(ARCHIVE_BUCKET, &key);

    log::debug!("Downloading archive object {key:?}");
    let response = client.get(&url).send().await?;
    match classify(response.status()) {
        StatusClass::Ok => {
            let data = response.bytes().await?.to_vec();
            log::debug!("Object {key:?} is {} bytes", data.len());
            Ok(nexrad_data::volume::File::new(data))
        }
        StatusClass::NotFound => Err(ArchiveError::NotFound(key)),
        StatusClass::Failed => {
            let status = response.status();
            let body = response.text().await.ok();
            Err(ArchiveError::Status { status, url, body })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).expect("valid date")
    }

    // -- Identifier ---------------------------------------------------------

    /// Site and time come out of the fixed offsets the bucket's naming uses.
    #[test]
    fn identifier_splits_site_and_collection_time() {
        let id = Identifier::new("KTLX20240520_000004_V06".to_string());
        assert_eq!(id.name(), "KTLX20240520_000004_V06");
        assert_eq!(id.site(), Some("KTLX"));
        let dt = id.date_time().expect("parses");
        assert_eq!(dt.format("%Y-%m-%d %H:%M:%S").to_string(), "2024-05-20 00:00:04");
    }

    /// A name too short to slice must yield `None`, not panic.
    ///
    /// `key_to_identifier` produces an empty name for any key that does not have
    /// the expected five segments, so this is reachable from a real listing.
    #[test]
    fn identifier_rejects_names_it_cannot_slice() {
        for name in ["", "KTL", "KTLX", "KTLX2024"] {
            let id = Identifier::new(name.to_string());
            assert!(
                id.date_time().is_none(),
                "{name:?} should have no parseable date/time"
            );
        }
    }

    // -- key -> name --------------------------------------------------------

    /// The four leading key segments are dropped and nothing else is.
    ///
    /// Off-by-one in either direction is caught: `skip(3)` leaves the site
    /// glued to the front, `skip(5)` empties the name.
    #[test]
    fn key_to_identifier_drops_exactly_the_date_and_site_segments() {
        let id = key_to_identifier("2024/05/20/KTLX/KTLX20240520_000004_V06");
        assert_eq!(id.name(), "KTLX20240520_000004_V06");
    }

    /// `_MDM` sidecars are returned, not filtered.
    ///
    /// `crate::scan` depends on this: it parses a time out of every name and
    /// breaks ties by taking the first, and the bucket orders `..._V06` before
    /// `..._V06_MDM`. Filtering them here would be a behaviour change even
    /// though it looks like a cleanup.
    #[test]
    fn key_to_identifier_keeps_mdm_sidecars() {
        let id = key_to_identifier("2024/05/20/KTLX/KTLX20240520_000004_V06_MDM");
        assert_eq!(id.name(), "KTLX20240520_000004_V06_MDM");
    }

    // -- URLs ---------------------------------------------------------------

    /// The listing prefix is the date-partitioned `%Y/%m/%d/SITE` form, with
    /// zero-padded month and day.
    ///
    /// A single-digit month is the case that distinguishes `%Y/%m/%d` from a
    /// hand-rolled `{}/{}/{}`.
    #[test]
    fn day_prefix_is_zero_padded_and_date_partitioned() {
        assert_eq!(day_prefix("KTLX", &date(2024, 5, 6)), "2024/05/06/KTLX");
        assert_eq!(day_prefix("KDMX", &date(2011, 11, 27)), "2011/11/27/KDMX");
    }

    /// A first-page URL is a `list-type=2` prefix query with no cursor.
    #[test]
    fn list_url_is_a_v2_prefix_query() {
        let url = list_url("bkt", "2024/05/20/KTLX", None, None).expect("url");
        assert!(url.starts_with("https://bkt.s3.amazonaws.com/?"), "{url}");
        assert!(url.contains("list-type=2"), "{url}");
        // `query_pairs_mut` percent-encodes the separators; the live bucket
        // accepts that form (verified against the real endpoint).
        assert!(url.contains("prefix=2024%2F05%2F20%2FKTLX"), "{url}");
        assert!(
            !url.contains("continuation-token"),
            "first page must not carry a cursor: {url}"
        );
    }

    /// `max-keys` is only present when asked for.
    #[test]
    fn list_url_carries_max_keys_only_when_set() {
        let bare = list_url("bkt", "p", None, None).expect("url");
        assert!(!bare.contains("max-keys"), "{bare}");
        let capped = list_url("bkt", "p", Some(7), None).expect("url");
        assert!(capped.contains("max-keys=7"), "{capped}");
    }

    /// The continuation token reaches the query string, percent-encoded.
    ///
    /// Real tokens contain `/` and can contain `+` and `=`. An unencoded `+`
    /// arrives at S3 as a space and the page is rejected, which is a distinct
    /// failure from never sending the token at all -- both produce a short
    /// listing, so both need their own assertion.
    #[test]
    fn list_url_percent_encodes_the_continuation_token() {
        let token = "abc/def+ghi=";
        let url = list_url("bkt", "p", None, Some(token)).expect("url");
        assert!(
            url.contains("continuation-token=abc%2Fdef%2Bghi%3D"),
            "token not encoded into the query: {url}"
        );
        assert!(
            !url.contains("abc/def+ghi="),
            "token appears unencoded: {url}"
        );
    }

    /// Object URLs are bucket-host plus the full key path.
    #[test]
    fn object_url_is_the_key_under_the_bucket_host() {
        assert_eq!(
            object_url("bkt", "2024/05/20/KTLX/KTLX20240520_000004_V06"),
            "https://bkt.s3.amazonaws.com/2024/05/20/KTLX/KTLX20240520_000004_V06"
        );
    }

    // -- status classification ---------------------------------------------

    /// Only `200` is a body; `404` is its own outcome; everything else fails.
    ///
    /// The non-200 2xx entries are the point: an implementation using
    /// `is_success()` or `error_for_status()` would accept `206 Partial
    /// Content` and hand a truncated volume to the decoder.
    #[test]
    fn only_200_is_treated_as_a_complete_body() {
        assert_eq!(classify(StatusCode::OK), StatusClass::Ok);
        assert_eq!(classify(StatusCode::NOT_FOUND), StatusClass::NotFound);
        for status in [
            StatusCode::PARTIAL_CONTENT,
            StatusCode::NO_CONTENT,
            StatusCode::NOT_MODIFIED,
            StatusCode::FORBIDDEN,
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::SERVICE_UNAVAILABLE,
        ] {
            assert_eq!(
                classify(status),
                StatusClass::Failed,
                "{status} must not be read as a body or as an absence"
            );
        }
    }

    // -- XML parsing --------------------------------------------------------

    /// Build a `ListBucketResult` document.
    fn listing(keys: &[&str], next_token: Option<&str>) -> String {
        let contents: String = keys
            .iter()
            .map(|k| {
                format!(
                    "<Contents><Key>{k}</Key>\
                     <LastModified>2025-07-15T22:47:45.000Z</LastModified>\
                     <Size>8717892</Size><StorageClass>STANDARD</StorageClass></Contents>"
                )
            })
            .collect();
        let truncated = next_token.is_some();
        let token = next_token
            .map(|t| format!("<NextContinuationToken>{t}</NextContinuationToken>"))
            .unwrap_or_default();
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
<Name>unidata-nexrad-level2</Name><Prefix>2024/05/20/KTLX</Prefix>
{token}<KeyCount>{}</KeyCount><MaxKeys>1000</MaxKeys>
<IsTruncated>{truncated}</IsTruncated>{contents}</ListBucketResult>"#,
            keys.len()
        )
    }

    /// Keys, the truncation flag and the cursor all come out of the document.
    #[test]
    fn parse_list_page_reads_keys_flag_and_cursor() {
        let page = parse_list_page(&listing(&["a/b/c/d/one", "a/b/c/d/two"], Some("TOK")))
            .expect("parses");
        assert_eq!(page.keys, vec!["a/b/c/d/one", "a/b/c/d/two"]);
        assert!(page.truncated);
        assert_eq!(page.next_token.as_deref(), Some("TOK"));
    }

    /// A final page reports no truncation and no cursor.
    #[test]
    fn parse_list_page_reads_a_final_page() {
        let page = parse_list_page(&listing(&["a/b/c/d/one"], None)).expect("parses");
        assert_eq!(page.keys, vec!["a/b/c/d/one"]);
        assert!(!page.truncated);
        assert_eq!(page.next_token, None);
    }

    /// A `<Key>` is only an object when it sits inside `<Contents>`.
    ///
    /// The fixture plants one in an `<Error>` block and adds a
    /// `<CommonPrefixes>` entry, because a plain `ListBucketResult` contains no
    /// stray `<Key>` at all -- a test using the unmodified fixture would assert
    /// the guard while proving only that the fixture has nothing to guard
    /// against. Without `if in_contents` the phantom key is returned as a
    /// volume and `crate::scan` tries to download it.
    #[test]
    fn parse_list_page_only_takes_keys_inside_contents() {
        let doc = listing(&["a/b/c/d/real"], None).replacen(
            "<Contents>",
            "<CommonPrefixes><Prefix>2024/05/20/KTLX/</Prefix></CommonPrefixes>\
             <Error><Code>NoSuchKey</Code><Key>a/b/c/d/phantom</Key></Error>\
             <Contents>",
            1,
        );
        let page = parse_list_page(&doc).expect("parses");
        assert_eq!(
            page.keys,
            vec!["a/b/c/d/real"],
            "captured a Key from outside Contents"
        );
    }

    /// An empty listing is an empty result, not an error.
    ///
    /// `crate::scan::list_files_with_fallback` distinguishes "no files today"
    /// from a failure, and rolls back a day on the former.
    #[test]
    fn parse_list_page_reads_an_empty_listing() {
        let page = parse_list_page(&listing(&[], None)).expect("parses");
        assert!(page.keys.is_empty());
        assert!(!page.truncated);
    }

    // -- pagination ---------------------------------------------------------

    /// Drive `collect_keys` over canned pages, recording the URLs requested.
    ///
    /// Returns `(keys, urls)`. `pollster` is not available here, so the future
    /// is driven by hand -- it never yields, because the fetcher is immediate.
    fn paginate(pages: Vec<std::result::Result<String, ArchiveError>>) -> (Result<Vec<String>>, Vec<String>) {
        let urls = std::cell::RefCell::new(Vec::new());
        let remaining = std::cell::RefCell::new(std::collections::VecDeque::from(pages));

        let outcome = {
            let fut = collect_keys(ARCHIVE_BUCKET, "2024/05/20/KTLX", Some(2), |url| {
                urls.borrow_mut().push(url);
                let next = remaining
                    .borrow_mut()
                    .pop_front()
                    .unwrap_or_else(|| Err(ArchiveError::MalformedListing("no more pages".into())));
                async move { next }
            });
            let mut fut = Box::pin(fut);
            let waker = std::task::Waker::noop();
            let mut cx = std::task::Context::from_waker(waker);
            match fut.as_mut().poll(&mut cx) {
                std::task::Poll::Ready(v) => v,
                std::task::Poll::Pending => panic!("fixture fetcher never yields"),
            }
        };

        (outcome, urls.into_inner())
    }

    /// A truncated listing is followed to completion, in order.
    ///
    /// The fixture is three pages, so this observes the token being threaded
    /// *between* pages and not just once. It fails if the continuation token is
    /// dropped (one page, two keys), if paging stops early (four keys), or if
    /// pages are gathered out of order.
    #[test]
    fn pagination_follows_the_cursor_across_every_page() {
        let (keys, urls) = paginate(vec![
            Ok(listing(&["a/b/c/d/k1", "a/b/c/d/k2"], Some("TOK1"))),
            Ok(listing(&["a/b/c/d/k3", "a/b/c/d/k4"], Some("TOK2"))),
            Ok(listing(&["a/b/c/d/k5"], None)),
        ]);
        let keys = keys.expect("listing should complete");

        assert_eq!(
            keys,
            vec![
                "a/b/c/d/k1",
                "a/b/c/d/k2",
                "a/b/c/d/k3",
                "a/b/c/d/k4",
                "a/b/c/d/k5",
            ],
            "paged listing lost or reordered keys"
        );
        assert_eq!(urls.len(), 3, "expected one request per page");
        assert!(
            !urls[0].contains("continuation-token"),
            "first request must not carry a cursor: {}",
            urls[0]
        );
        assert!(
            urls[1].contains("continuation-token=TOK1"),
            "second request did not carry the first page's cursor: {}",
            urls[1]
        );
        assert!(
            urls[2].contains("continuation-token=TOK2"),
            "third request did not carry the second page's cursor: {}",
            urls[2]
        );
    }

    /// An untruncated page ends the listing even if a cursor is echoed.
    ///
    /// Guards the loop condition specifically: an implementation that paged
    /// while `next_token.is_some()` would request forever here, since the
    /// fixture only supplies one page and every later fetch errors.
    #[test]
    fn pagination_stops_on_the_truncation_flag_not_the_cursor() {
        let body = listing(&["a/b/c/d/k1"], None)
            .replace("</ListBucketResult>", "<NextContinuationToken>STALE</NextContinuationToken></ListBucketResult>");
        let (keys, urls) = paginate(vec![Ok(body)]);
        assert_eq!(keys.expect("completes"), vec!["a/b/c/d/k1"]);
        assert_eq!(urls.len(), 1, "followed a cursor on an untruncated page");
    }

    /// A truncated page with no cursor is an error, not a short listing.
    ///
    /// This is the case upstream turned into `TruncatedListObjectsResponse`.
    /// Returning the partial result instead would be the silent data loss this
    /// module exists to avoid.
    #[test]
    fn pagination_refuses_to_truncate_silently() {
        let body = listing(&["a/b/c/d/k1"], None).replace(
            "<IsTruncated>false</IsTruncated>",
            "<IsTruncated>true</IsTruncated>",
        );
        let (keys, urls) = paginate(vec![Ok(body)]);
        let err = keys.expect_err("must not return a partial listing as success");
        assert!(
            matches!(err, ArchiveError::MalformedListing(_)),
            "unexpected error: {err:?}"
        );
        assert_eq!(urls.len(), 1);
    }

    /// A server that never stops truncating is bounded rather than hanging.
    #[test]
    fn pagination_gives_up_after_the_page_cap() {
        let pages = (0..MAX_LIST_PAGES + 5)
            .map(|i| Ok(listing(&["a/b/c/d/k"], Some(&format!("TOK{i}")))))
            .collect();
        let (keys, urls) = paginate(pages);
        let err = keys.expect_err("an endless listing must terminate as an error");
        assert!(
            matches!(err, ArchiveError::MalformedListing(_)),
            "unexpected error: {err:?}"
        );
        assert_eq!(urls.len(), MAX_LIST_PAGES, "page cap not enforced");
    }

    /// A fetch failure aborts the listing rather than returning what it had.
    #[test]
    fn pagination_propagates_a_page_failure() {
        let (keys, urls) = paginate(vec![
            Ok(listing(&["a/b/c/d/k1"], Some("TOK1"))),
            Err(ArchiveError::Status {
                status: StatusCode::SERVICE_UNAVAILABLE,
                url: "https://example.invalid/".into(),
                body: None,
            }),
        ]);
        let err = keys.expect_err("a failed page must not be reported as the end of the listing");
        assert!(matches!(err, ArchiveError::Status { .. }), "{err:?}");
        assert_eq!(urls.len(), 2);
    }

    // -- response handling --------------------------------------------------
    //
    // `classify` is a pure function and is covered above, but nothing there
    // proves `get_text` still *consults* it. These drive the real reqwest path
    // against a one-shot loopback server: hermetic (port 0 on 127.0.0.1, one
    // connection, no external network) but a genuine HTTP round trip.

    /// Serve exactly one canned HTTP response and return the URL to hit.
    fn serve_once(response: String) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("local addr").port();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            if let Ok((mut stream, _)) = listener.accept() {
                let mut scratch = [0u8; 4096];
                let _ = stream.read(&mut scratch);
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        format!("http://127.0.0.1:{port}/")
    }

    fn http_response(status_line: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status_line}\r\nContent-Type: application/xml\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    /// A cleartext-capable client.
    ///
    /// [`super::client`] sets `https_only`, which these loopback URLs cannot
    /// satisfy. `tls::init()` is still required: with `rustls-no-provider` and
    /// `aws-lc-rs` out of the graph, `build()` panics without a provider
    /// regardless of the scheme actually used.
    fn loopback_client() -> reqwest::Client {
        crate::tls::init();
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("client")
    }

    /// A `200` yields the body.
    ///
    /// The counterweight to the two below: without it, a `get_text` that
    /// errored on everything would satisfy them both.
    #[tokio::test]
    async fn get_text_returns_the_body_on_200() {
        let url = serve_once(http_response("200 OK", "<ListBucketResult/>"));
        let body = get_text(&loopback_client(), url)
            .await
            .expect("200 should yield a body");
        assert_eq!(body, "<ListBucketResult/>");
    }

    /// A `503` is an error, not an empty listing.
    ///
    /// This is the upstream bug in its original form: `nexrad-data` never
    /// looked at the status, so a throttled or broken S3 handed its `<Error>`
    /// document to the XML parser, which found no `<Contents>` and returned
    /// zero objects. `crate::scan::list_files_with_fallback` reads zero objects
    /// as "nothing recorded for this date", rolls back a day, gets the same
    /// answer and reports no data available -- an outage shown to the user as
    /// an absence of weather.
    #[tokio::test]
    async fn get_text_reports_a_server_error_instead_of_an_empty_listing() {
        let url = serve_once(http_response(
            "503 Service Unavailable",
            "<Error><Code>SlowDown</Code></Error>",
        ));
        let err = get_text(&loopback_client(), url)
            .await
            .expect_err("a 503 body must not be returned as a listing");
        match err {
            ArchiveError::Status { status, .. } => {
                assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            }
            other => panic!("expected Status, got {other:?}"),
        }
    }

    /// A `404` is reported as an absence, distinctly from other failures.
    #[tokio::test]
    async fn get_text_reports_a_404_as_not_found() {
        let url = serve_once(http_response(
            "404 Not Found",
            "<Error><Code>NoSuchKey</Code></Error>",
        ));
        let err = get_text(&loopback_client(), url)
            .await
            .expect_err("a 404 must not be returned as a body");
        assert!(
            matches!(err, ArchiveError::NotFound(_)),
            "expected NotFound, got {err:?}"
        );
    }

    // -- live ---------------------------------------------------------------
    //
    // Run with:
    //   cargo test -p rustdar-radar --lib -- --ignored --nocapture archive::tests::live_

    /// End-to-end against the real bucket: list, download, decode.
    ///
    /// Nothing here is stubbed. It fails if the prefix scheme is wrong (empty
    /// listing), if the key derivation is wrong (404), if the anonymous-access
    /// assumption is wrong (403), or if the bytes that come back are not a
    /// decodable Archive II volume.
    #[ignore = "hits the live unidata-nexrad-level2 S3 bucket"]
    #[tokio::test]
    async fn live_archive_lists_downloads_and_decodes_a_volume() {
        let day = date(2024, 5, 20);
        let files = list_files("KTLX", &day).await.expect("listing KTLX 2024-05-20");
        println!("listed {} objects", files.len());
        assert!(
            files.len() > 100,
            "a full site-day should hold hundreds of objects, got {}",
            files.len()
        );

        // The `_MDM` sidecars are in the listing on purpose; a volume is what
        // decodes.
        let volume = files
            .iter()
            .find(|f| f.name().ends_with("_V06"))
            .expect("at least one V06 volume");
        println!("downloading {}", volume.name());

        let file = download_file(volume.clone())
            .await
            .expect("download should succeed");
        println!("downloaded {} bytes", file.data().len());
        assert!(
            file.data().len() > 1_000_000,
            "a Level II volume should be megabytes, got {}",
            file.data().len()
        );

        let scan = file.scan().expect("volume should decode");
        let sweeps = scan.sweeps().len();
        println!("decoded {sweeps} sweeps");
        assert!(sweeps > 0, "decoded a volume with no sweeps");
    }

    /// Paging against real S3 reproduces the unpaginated listing exactly.
    ///
    /// This is the assertion the offline fixture cannot make: it proves the
    /// continuation token rustls-encoded into a real query string is accepted
    /// by S3 and advances the cursor, rather than being ignored (which would
    /// loop on page one) or rejected. A real site-day is ~235 keys against a
    /// 1000-key default page, so the small `max-keys` is what makes the
    /// truncation path reachable at all.
    #[ignore = "hits the live unidata-nexrad-level2 S3 bucket"]
    #[tokio::test]
    async fn live_paged_listing_equals_the_single_page_listing() {
        let client = shared_client();
        let prefix = day_prefix("KTLX", &date(2024, 5, 20));

        let mut single_page_requests = 0;
        let whole = collect_keys(ARCHIVE_BUCKET, &prefix, None, |url| {
            single_page_requests += 1;
            get_text(client, url)
        })
        .await
        .expect("unpaginated listing");

        let mut paged_requests = 0;
        let paged = collect_keys(ARCHIVE_BUCKET, &prefix, Some(20), |url| {
            paged_requests += 1;
            get_text(client, url)
        })
        .await
        .expect("paginated listing");

        println!(
            "{} keys in {single_page_requests} request(s); {} keys in {paged_requests} request(s)",
            whole.len(),
            paged.len()
        );

        assert_eq!(single_page_requests, 1, "the default page should hold a site-day");
        assert!(
            paged_requests > 5,
            "max-keys=20 over ~235 keys should need many pages, took {paged_requests}"
        );
        assert_eq!(
            paged, whole,
            "the paged listing differs from the single-page listing"
        );
    }

    /// A key the bucket does not hold is reported as an absence, not a body.
    ///
    /// Fails if the 404 branch is dropped in favour of `error_for_status()`
    /// (wrong variant) or, worse, if a 404 body is handed back as volume data.
    #[ignore = "hits the live unidata-nexrad-level2 S3 bucket"]
    #[tokio::test]
    async fn live_missing_volume_is_reported_as_not_found() {
        // Well-formed name, real site, real date, no such volume: this reaches
        // S3 and comes back 404 rather than failing key derivation locally.
        let missing = Identifier::new("KTLX20240520_010101_V06".to_string());
        let err = download_file(missing)
            .await
            .expect_err("a nonexistent volume must not download");
        println!("got: {err:?}");
        assert!(
            matches!(err, ArchiveError::NotFound(_)),
            "expected NotFound, got {err:?}"
        );
    }

    /// The bucket really is readable with no credentials.
    ///
    /// The claim this whole module rests on: no SigV4, no credential chain. If
    /// the bucket ever required signing, every other live test would fail with
    /// a confusing decode error while this one names the cause.
    #[ignore = "hits the live unidata-nexrad-level2 S3 bucket"]
    #[tokio::test]
    async fn live_listing_needs_no_credentials() {
        let url = list_url(ARCHIVE_BUCKET, "2024/05/20/KTLX", Some(1), None).expect("url");
        let response = shared_client()
            .get(&url)
            .send()
            .await
            .expect("request should reach S3");
        println!("anonymous LIST -> {}", response.status());
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "anonymous listing was refused; this module would need SigV4"
        );
    }
}
