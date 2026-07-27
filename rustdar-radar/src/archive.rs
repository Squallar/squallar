//! Anonymous S3 access to the NEXRAD Level II archive bucket.
//!
//! Reimplements the `nexrad_data::aws` surface (`Identifier`, `list_files`,
//! `download_file`) so that crate's `aws` feature can stay off: it turns on
//! `reqwest/rustls`, which resolves to `__rustls-aws-lc-rs` and drags
//! `aws-lc-sys` in beside the *ring* stack [`crate::tls`] installs. Do not
//! re-enable it, and do not answer the resulting trust-store question by
//! bundling CA roots -- shipping our own gives the binary an expiration date.
//! `nexrad-data` still does all the decoding; only the HTTP layer moved.
//!
//! `unidata-nexrad-level2` is anonymously readable: an unsigned
//! `GET https://unidata-nexrad-level2.s3.amazonaws.com/?list-type=2&prefix=...`
//! returns `200` and a `ListBucketResult`, so there is no SigV4, no credential
//! chain and no clock-skew handling here. Upstream did not sign either.
//!
//! Objects are keyed `YYYY/MM/DD/SITE/SITEYYYYMMDD_HHMMSS_V06`, so one site-day
//! is a prefix query on `YYYY/MM/DD/SITE`. Each volume has a `..._V06_MDM`
//! metadata sidecar, which [`list_files`] returns too (see
//! [`key_to_identifier`]).

use std::sync::OnceLock;

use chrono::NaiveDate;
use reqwest::StatusCode;
use xml::reader::{EventReader, XmlEvent};

use crate::sources::DataSources;

/// How long a single archive request may take, end to end. Upstream had *no*
/// timeout, so a stalled connection hung the fetch task, and the pane's
/// "fetching" state, forever. Generous because volumes run to ~9 MB: this
/// bounds a hang, it does not enforce latency.
const ARCHIVE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Upper bound on `ListObjectsV2` pages followed for one listing. A real
/// site-day is 200-350 keys, i.e. one page; this only stops a server that keeps
/// returning `IsTruncated` from spinning forever inside a UI fetch.
const MAX_LIST_PAGES: usize = 100;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Failures reaching or interpreting the archive bucket. Consumers render
/// these with `{:?}` rather than matching on them.
#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error("S3 request failed: {0}")]
    Request(#[from] reqwest::Error),

    /// Distinct from [`ArchiveError::Status`]: "not in the archive" is an
    /// ordinary outcome, "the bucket returned 503" is not.
    #[error("S3 object not found: {0}")]
    NotFound(String),

    /// Any other non-`200`.
    #[error("S3 returned {status} for {url}{}", body.as_deref().map(|b| format!(": {b}")).unwrap_or_default())]
    Status {
        status: StatusCode,
        url: String,
        /// The response body, when one could be read.
        body: Option<String>,
    },

    #[error("malformed S3 listing: {0}")]
    MalformedListing(String),

    /// An [`Identifier`] carries a name no object key can be derived from.
    #[error("cannot derive an archive key from identifier {0:?}")]
    UnkeyableIdentifier(String),
}

pub type Result<T> = std::result::Result<T, ArchiveError>;

// ---------------------------------------------------------------------------
// Identifier
// ---------------------------------------------------------------------------

/// A NEXRAD archive volume file, named but not keyed: a newtype over the bare
/// object *name*, with site and collection time recovered by fixed-offset
/// slicing. Names look like `KTLX20240520_000004_V06` -- four characters of
/// site, eight of `%Y%m%d`, a separator, then six of `%H%M%S`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct Identifier(String);

impl Identifier {
    pub fn new(name: String) -> Self {
        Identifier(name)
    }

    pub fn name(&self) -> &str {
        &self.0
    }

    /// The radar site, e.g. `KDMX`. `get` rather than indexing: names come
    /// from bucket keys, so a short or non-ASCII one must not panic.
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
/// Everything goes through `query_pairs_mut` because continuation tokens are
/// opaque blobs that routinely contain `/` and can contain `+` and `=`; an
/// unencoded `+` reaches S3 as a space and the page is rejected.
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

/// Build the object URL for a full bucket key. The key is interpolated, not
/// encoded: archive keys are drawn from `[A-Za-z0-9_/]`.
fn object_url(bucket: &str, key: &str) -> String {
    crate::sources::DataSources::s3_object_url(bucket, key)
}

/// The prefix under which one site's volumes for one day are stored.
fn day_prefix(site: &str, date: &NaiveDate) -> String {
    format!("{}/{}", date.format("%Y/%m/%d"), site)
}

/// Recover a volume name from a full bucket key. Reproduces upstream exactly,
/// including two load-bearing details: a key with fewer than five segments
/// yields an empty name (which `crate::scan` then skips, so an unexpected key
/// is dropped rather than fatal), and `collect` concatenates any trailing
/// segments *without* re-inserting the `/`.
fn key_to_identifier(key: &str) -> Identifier {
    Identifier::new(key.split('/').skip(4).collect::<String>())
}

// ---------------------------------------------------------------------------
// ListObjectsV2 parsing
// ---------------------------------------------------------------------------

/// One page of a `ListObjectsV2` response.
#[derive(Debug, Default, PartialEq, Eq)]
struct ListPage {
    /// In the order S3 returned them, which is UTF-8 binary order.
    keys: Vec<String>,
    truncated: bool,
    /// Present exactly when `truncated`.
    next_token: Option<String>,
}

/// Which element's character data is currently being accumulated.
#[derive(PartialEq, Eq)]
enum Field {
    Key,
    IsTruncated,
    NextToken,
}

/// Parse one `ListBucketResult` document. Only `Contents/Key`, `IsTruncated`
/// and `NextContinuationToken` are read.
///
/// `Key` is captured only inside `Contents`, so the document's own
/// `<Prefix>`/`<Name>` and any `CommonPrefixes` are not mistaken for objects.
/// Character data is accumulated rather than assigned: an XML parser may split
/// text across several events, and continuation tokens are long enough to hit
/// that.
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

/// Follow `NextContinuationToken` until the listing is complete. `fetch_page`
/// takes the fully-built URL so a test can see what would have gone on the wire.
///
/// Deliberately unlike upstream, which returned
/// `AWSError::TruncatedListObjectsResponse` rather than paging. Measured
/// against the live bucket, the busiest site-days sampled (2011-04-27/KBMX,
/// 2013-05-20/KTLX) were 311 and 323 keys against a 1000-key page, so neither
/// path normally fires.
///
/// Paging keys off `IsTruncated`, not off the presence of a token: a final page
/// that echoed a stale token would otherwise loop forever.
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
/// Must be built through [`crate::tls::client`], which installs the *ring*
/// provider: under `rustls-no-provider` there is no compiled-in default and
/// `ClientBuilder::build` panics without one. That constructor is also what
/// keeps this module off a bundled root store.
///
/// One client per process: `list_scans_for_range` lists per day and a loop
/// replay downloads a volume per frame, so losing connection reuse would hurt.
pub(crate) fn shared_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        crate::tls::client(crate::tls::USER_AGENT, ARCHIVE_TIMEOUT)
            .build()
            // A client that cannot be constructed is a build-configuration
            // fault, not something a caller could recover from.
            .unwrap_or_else(|e| panic!("failed to build the archive HTTP client: {e}"))
    })
}

/// How a response status should be interpreted. Split out from the request so
/// the mapping is testable without a socket.
#[derive(Debug, PartialEq, Eq)]
enum StatusClass {
    /// `200`: the body is the object.
    Ok,
    /// `404`: the object is absent.
    NotFound,
    /// Anything else, including other 2xx.
    Failed,
}

/// Only `200` is success: this module never requests a partial or conditional
/// fetch, so treating a `206`/`304` as a body would hand the decoder a
/// truncated volume.
fn classify(status: StatusCode) -> StatusClass {
    match status {
        StatusCode::OK => StatusClass::Ok,
        StatusCode::NOT_FOUND => StatusClass::NotFound,
        _ => StatusClass::Failed,
    }
}

/// GET a URL and return the body as text, or an error describing the status.
///
/// The status check is deliberate: upstream fed any body to the XML parser, so
/// a `403` or `503` parsed as an *empty listing*, which
/// `crate::scan::list_files_with_fallback` reads as "no data for this date" --
/// an outage rendered to the user as an absence of weather.
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

/// The binary counterpart of [`get_text`], with the same status handling.
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

// No byte-range helper here on purpose: `rustdar_overlays::hrrr::fetch` needs
// semantics `classify` cannot express -- a `200` to a `Range` request means the
// server ignored the header and is about to stream ~130 MB, which is an error
// there rather than a body to slice locally.

// ---------------------------------------------------------------------------
// Public surface
// ---------------------------------------------------------------------------

/// The volumes the archive holds for one site-day, in the bucket's own key
/// order -- `crate::scan` relies on that order to break the tie between a
/// volume and its `_MDM` sidecar by taking the first match.
///
/// The bucket comes from [`DataSources::level2_bucket`], as
/// [`crate::level3::list_day`] takes its bucket from the same table: the
/// derived validations (the Android network-security-config, the web
/// service-worker never-cache list) read [`DataSources`], so a bucket named
/// anywhere else is invisible to both.
pub async fn list_files(
    sources: &DataSources,
    site: &str,
    date: &NaiveDate,
) -> Result<Vec<Identifier>> {
    // Before the first `.await`, so that merely polling this future installs
    // the crypto provider. `crate::tls` has a probe that depends on it.
    let client = shared_client();
    let prefix = day_prefix(site, date);

    log::debug!("Listing archive objects for prefix {prefix:?}");
    let keys = collect_keys(&sources.level2_bucket, &prefix, None, |url| {
        get_text(client, url)
    })
    .await?;
    log::debug!("Listing for {prefix:?} returned {} keys", keys.len());

    Ok(keys.iter().map(|key| key_to_identifier(key)).collect())
}

/// Download the volume an identifier names. [`nexrad_data::volume::File`] still
/// owns decompression and decoding. The bucket comes from
/// [`DataSources::level2_bucket`] — see [`list_files`].
pub async fn download_file(
    sources: &DataSources,
    identifier: Identifier,
) -> Result<nexrad_data::volume::File> {
    let client = shared_client();

    let date = identifier
        .date_time()
        .ok_or_else(|| ArchiveError::UnkeyableIdentifier(identifier.name().to_string()))?;
    let site = identifier
        .site()
        .ok_or_else(|| ArchiveError::UnkeyableIdentifier(identifier.name().to_string()))?;

    let key = format!("{}/{}/{}", date.format("%Y/%m/%d"), site, identifier.name());
    let url = object_url(&sources.level2_bucket, &key);

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

// Native-only: `#[tokio::test]` (the dev-dependency is target-gated) and
// `ClientBuilder::timeout`, which reqwest's wasm builder does not have. Keeps
// this crate compiling under the wasm32 CI row.
#[cfg(all(test, not(target_arch = "wasm32")))]
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
        assert_eq!(
            dt.format("%Y-%m-%d %H:%M:%S").to_string(),
            "2024-05-20 00:00:04"
        );
    }

    /// A name too short to slice must yield `None`, not panic —
    /// `key_to_identifier` emits an empty name for any unexpected key shape.
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

    /// Fails on an off-by-one either way: `skip(3)` leaves the site glued to
    /// the front, `skip(5)` empties the name.
    #[test]
    fn key_to_identifier_drops_exactly_the_date_and_site_segments() {
        let id = key_to_identifier("2024/05/20/KTLX/KTLX20240520_000004_V06");
        assert_eq!(id.name(), "KTLX20240520_000004_V06");
    }

    /// `_MDM` sidecars are returned, not filtered: `crate::scan` breaks ties by
    /// taking the first name, and the bucket orders `..._V06` before
    /// `..._V06_MDM`. Filtering here looks like a cleanup but changes behaviour.
    #[test]
    fn key_to_identifier_keeps_mdm_sidecars() {
        let id = key_to_identifier("2024/05/20/KTLX/KTLX20240520_000004_V06_MDM");
        assert_eq!(id.name(), "KTLX20240520_000004_V06_MDM");
    }

    // -- URLs ---------------------------------------------------------------

    /// The single-digit month is the case that distinguishes `%Y/%m/%d` from a
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
        // The live bucket accepts the percent-encoded separators.
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

    /// Real tokens contain `/` and can contain `+` and `=`; an unencoded `+`
    /// arrives at S3 as a space and the page is rejected. That is a distinct
    /// failure from never sending the token, so both are asserted.
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

    /// The non-200 2xx entries are the point: `is_success()` or
    /// `error_for_status()` would accept `206 Partial Content` and hand a
    /// truncated volume to the decoder.
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

    #[test]
    fn parse_list_page_reads_keys_flag_and_cursor() {
        let page = parse_list_page(&listing(&["a/b/c/d/one", "a/b/c/d/two"], Some("TOK")))
            .expect("parses");
        assert_eq!(page.keys, vec!["a/b/c/d/one", "a/b/c/d/two"]);
        assert!(page.truncated);
        assert_eq!(page.next_token.as_deref(), Some("TOK"));
    }

    #[test]
    fn parse_list_page_reads_a_final_page() {
        let page = parse_list_page(&listing(&["a/b/c/d/one"], None)).expect("parses");
        assert_eq!(page.keys, vec!["a/b/c/d/one"]);
        assert!(!page.truncated);
        assert_eq!(page.next_token, None);
    }

    /// A `<Key>` is only an object inside `<Contents>`. The fixture plants one
    /// in an `<Error>` block and adds a `<CommonPrefixes>` entry, because a
    /// plain `ListBucketResult` has no stray `<Key>` to guard against. Without
    /// `if in_contents` the phantom key is returned and `crate::scan` tries to
    /// download it.
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

    /// An empty listing is an empty result, not an error:
    /// `crate::scan::list_files_with_fallback` rolls back a day on the former.
    #[test]
    fn parse_list_page_reads_an_empty_listing() {
        let page = parse_list_page(&listing(&[], None)).expect("parses");
        assert!(page.keys.is_empty());
        assert!(!page.truncated);
    }

    // -- pagination ---------------------------------------------------------

    /// Drive `collect_keys` over canned pages, recording the URLs requested.
    /// The future is driven by hand (no `pollster` here); it never yields,
    /// because the fetcher is immediate.
    fn paginate(
        pages: Vec<std::result::Result<String, ArchiveError>>,
    ) -> (Result<Vec<String>>, Vec<String>) {
        let urls = std::cell::RefCell::new(Vec::new());
        let remaining = std::cell::RefCell::new(std::collections::VecDeque::from(pages));

        let sources = DataSources::production();
        let outcome = {
            let fut = collect_keys(&sources.level2_bucket, "2024/05/20/KTLX", Some(2), |url| {
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

    /// Three pages, so the token is observed threading *between* pages. Fails
    /// if the token is dropped (one page, two keys), if paging stops early
    /// (four keys), or if pages are gathered out of order.
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

    /// An untruncated page ends the listing even if a cursor is echoed: paging
    /// while `next_token.is_some()` would request forever here.
    #[test]
    fn pagination_stops_on_the_truncation_flag_not_the_cursor() {
        let body = listing(&["a/b/c/d/k1"], None).replace(
            "</ListBucketResult>",
            "<NextContinuationToken>STALE</NextContinuationToken></ListBucketResult>",
        );
        let (keys, urls) = paginate(vec![Ok(body)]);
        assert_eq!(keys.expect("completes"), vec!["a/b/c/d/k1"]);
        assert_eq!(urls.len(), 1, "followed a cursor on an untruncated page");
    }

    /// A truncated page with no cursor is an error: returning the partial
    /// result would be silent data loss.
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
    // `classify` is covered above, but nothing there proves `get_text` still
    // *consults* it. These drive the real reqwest path against a one-shot
    // loopback server: hermetic, but a genuine HTTP round trip.

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

    /// A cleartext-capable client: [`super::client`] sets `https_only`, which
    /// loopback URLs cannot satisfy. `tls::init()` is still required — with
    /// `rustls-no-provider` and `aws-lc-rs` out of the graph, `build()` panics
    /// without a provider whatever scheme is used.
    fn loopback_client() -> reqwest::Client {
        crate::tls::init();
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("client")
    }

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

    /// A `503` is an error, not an empty listing. This is the upstream bug:
    /// zero objects reads as "nothing recorded for this date", so an outage
    /// was shown to the user as an absence of weather.
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

    /// End-to-end against the real bucket. Fails if the prefix scheme is wrong
    /// (empty listing), the key derivation is wrong (404), anonymous access
    /// stops working (403), or the bytes are not a decodable Archive II volume.
    #[ignore = "hits the live unidata-nexrad-level2 S3 bucket"]
    #[tokio::test]
    async fn live_archive_lists_downloads_and_decodes_a_volume() {
        let sources = DataSources::production();
        let day = date(2024, 5, 20);
        let files = list_files(&sources, "KTLX", &day)
            .await
            .expect("listing KTLX 2024-05-20");
        println!("listed {} objects", files.len());
        assert!(
            files.len() > 100,
            "a full site-day should hold hundreds of objects, got {}",
            files.len()
        );

        // The `_MDM` sidecars are in the listing on purpose; only a volume
        // decodes.
        let volume = files
            .iter()
            .find(|f| f.name().ends_with("_V06"))
            .expect("at least one V06 volume");
        println!("downloading {}", volume.name());

        let file = download_file(&sources, volume.clone())
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

    /// Paging against real S3 reproduces the unpaginated listing exactly: the
    /// token is accepted and advances the cursor, rather than being ignored
    /// (looping on page one) or rejected. A real site-day is ~235 keys against
    /// a 1000-key default page, so the small `max-keys` is what makes the
    /// truncation path reachable at all.
    #[ignore = "hits the live unidata-nexrad-level2 S3 bucket"]
    #[tokio::test]
    async fn live_paged_listing_equals_the_single_page_listing() {
        let sources = DataSources::production();
        let client = shared_client();
        let prefix = day_prefix("KTLX", &date(2024, 5, 20));

        let mut single_page_requests = 0;
        let whole = collect_keys(&sources.level2_bucket, &prefix, None, |url| {
            single_page_requests += 1;
            get_text(client, url)
        })
        .await
        .expect("unpaginated listing");

        let mut paged_requests = 0;
        let paged = collect_keys(&sources.level2_bucket, &prefix, Some(20), |url| {
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

        assert_eq!(
            single_page_requests, 1,
            "the default page should hold a site-day"
        );
        assert!(
            paged_requests > 5,
            "max-keys=20 over ~235 keys should need many pages, took {paged_requests}"
        );
        assert_eq!(
            paged, whole,
            "the paged listing differs from the single-page listing"
        );
    }

    /// Fails if the 404 branch is dropped in favour of `error_for_status()`
    /// (wrong variant) or, worse, if a 404 body is handed back as volume data.
    #[ignore = "hits the live unidata-nexrad-level2 S3 bucket"]
    #[tokio::test]
    async fn live_missing_volume_is_reported_as_not_found() {
        // Well-formed name, real site, real date, no such volume: this reaches
        // S3 and comes back 404 rather than failing key derivation locally.
        let missing = Identifier::new("KTLX20240520_010101_V06".to_string());
        let err = download_file(&DataSources::production(), missing)
            .await
            .expect_err("a nonexistent volume must not download");
        println!("got: {err:?}");
        assert!(
            matches!(err, ArchiveError::NotFound(_)),
            "expected NotFound, got {err:?}"
        );
    }

    /// The claim this whole module rests on: no SigV4, no credential chain. If
    /// the bucket ever required signing, every other live test would fail with
    /// a confusing decode error while this one names the cause.
    #[ignore = "hits the live unidata-nexrad-level2 S3 bucket"]
    #[tokio::test]
    async fn live_listing_needs_no_credentials() {
        let sources = DataSources::production();
        let url = list_url(&sources.level2_bucket, "2024/05/20/KTLX", Some(1), None).expect("url");
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
