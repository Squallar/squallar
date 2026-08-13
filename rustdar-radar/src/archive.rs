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
//! is a prefix query on `YYYY/MM/DD/SITE`. The trailing `_V06` names the volume
//! format rather than the layout: TDWR sites sit in the same bucket under the
//! same prefix with `_V08` keys (2026-08-10 held 239 of them for `TPIT` against
//! 226 `_V06` for `KTLX`), and nothing here reads the suffix. Each volume has a
//! `..._MDM` metadata sidecar, which [`list_files`] returns too (see
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
/// site, eight of `%Y%m%d`, a separator, then six of `%H%M%S`. Every offset
/// sliced here stops at byte 19, so what follows is not looked at: a TDWR's
/// `TPIT20260810_000139_V08` and either network's `_MDM` sidecar parse by the
/// same rule as a `_V06`.
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
    bucket_url: &str,
    prefix: &str,
    max_keys: Option<u32>,
    continuation_token: Option<&str>,
) -> Result<String> {
    list_url_inner(bucket_url, prefix, None, max_keys, continuation_token)
}

/// [`list_url`] plus a `delimiter`, which makes S3 collapse everything below it
/// into `CommonPrefixes` instead of returning the keys.
///
/// A separate entry point rather than a fifth parameter on [`list_url`], so its
/// four existing call sites keep reading as plain prefix queries. Both share
/// [`list_url_inner`] so the `query_pairs_mut` encoding discipline applies to
/// the delimiter too.
pub(crate) fn list_url_delimited(
    bucket_url: &str,
    prefix: &str,
    delimiter: &str,
    continuation_token: Option<&str>,
) -> Result<String> {
    list_url_inner(
        bucket_url,
        prefix,
        Some(delimiter),
        None,
        continuation_token,
    )
}

fn list_url_inner(
    bucket_url: &str,
    prefix: &str,
    delimiter: Option<&str>,
    max_keys: Option<u32>,
    continuation_token: Option<&str>,
) -> Result<String> {
    // The bucket's **root URL**, from `DataSources::s3_bucket_url`, rather than
    // its name with the origin built here: the shape of an S3 URL has one
    // definition, in the table that declares the origins, and a listing that
    // built its own could not be pointed anywhere the object GETs went.
    let mut url = reqwest::Url::parse(&format!("{bucket_url}/")).map_err(|e| {
        ArchiveError::MalformedListing(format!("bad bucket url {bucket_url:?}: {e}"))
    })?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("list-type", "2");
        query.append_pair("prefix", prefix);
        if let Some(delimiter) = delimiter {
            query.append_pair("delimiter", delimiter);
        }
        if let Some(max_keys) = max_keys {
            query.append_pair("max-keys", &max_keys.to_string());
        }
        if let Some(token) = continuation_token {
            query.append_pair("continuation-token", token);
        }
    }
    Ok(url.into())
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
    /// The `CommonPrefixes` a delimited listing collapsed everything below into.
    /// Always empty for the undelimited queries the archive path issues.
    common_prefixes: Vec<String>,
    truncated: bool,
    /// Present exactly when `truncated`.
    next_token: Option<String>,
}

/// Which element's character data is currently being accumulated.
#[derive(PartialEq, Eq)]
enum Field {
    Key,
    CommonPrefix,
    IsTruncated,
    NextToken,
}

/// Parse one `ListBucketResult` document. Only `Contents/Key`,
/// `CommonPrefixes/Prefix`, `IsTruncated` and `NextContinuationToken` are read.
///
/// `Key` is captured only inside `Contents`, so the document's own
/// `<Prefix>`/`<Name>` and any `CommonPrefixes` are not mistaken for objects.
/// `Prefix` is captured only inside `CommonPrefixes` for the mirror-image
/// reason: every `ListBucketResult` echoes the requested prefix at top level,
/// and that is not a directory the caller asked about.
///
/// Character data is accumulated rather than assigned: an XML parser may split
/// text across several events, and continuation tokens are long enough to hit
/// that.
fn parse_list_page(body: &str) -> Result<ListPage> {
    let mut page = ListPage::default();
    let mut field: Option<Field> = None;
    let mut in_contents = false;
    let mut in_common_prefixes = false;
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
                    "CommonPrefixes" => {
                        in_common_prefixes = true;
                        None
                    }
                    "Key" if in_contents => Some(Field::Key),
                    "Prefix" if in_common_prefixes => Some(Field::CommonPrefix),
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
                    "CommonPrefixes" => in_common_prefixes = false,
                    "Key" if field == Some(Field::Key) => {
                        page.keys.push(std::mem::take(&mut buffer));
                    }
                    "Prefix" if field == Some(Field::CommonPrefix) => {
                        page.common_prefixes.push(std::mem::take(&mut buffer));
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
    bucket_url: &str,
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
        let url = list_url(bucket_url, prefix, max_keys, token.as_deref())?;
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

/// The directory-style listing: one entry per `CommonPrefixes/Prefix`, no keys.
///
/// The real-time chunk bucket holds a site's ~55 chunks under each of 999
/// rotating volume directories, so "which volumes exist" is ~55 000 keys as a
/// flat query and one page as a delimited one.
///
/// Shares [`collect_keys`]'s paging discipline for the same reasons — keyed off
/// `IsTruncated` rather than the token, truncated-without-a-token is an error
/// rather than silent loss, and the page cap stops a server that never stops
/// saying `IsTruncated`.
pub(crate) async fn collect_common_prefixes<F, Fut>(
    bucket_url: &str,
    prefix: &str,
    delimiter: &str,
    mut fetch_page: F,
) -> Result<Vec<String>>
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Result<String>>,
{
    let mut prefixes = Vec::new();
    let mut token: Option<String> = None;

    for _ in 0..MAX_LIST_PAGES {
        let url = list_url_delimited(bucket_url, prefix, delimiter, token.as_deref())?;
        let page = parse_list_page(&fetch_page(url).await?)?;
        prefixes.extend(page.common_prefixes);

        if !page.truncated {
            return Ok(prefixes);
        }
        let Some(next) = page.next_token else {
            return Err(ArchiveError::MalformedListing(format!(
                "delimited listing for prefix {prefix:?} is truncated but carries \
                 no NextContinuationToken; {} prefixes would be silently lost",
                prefixes.len()
            )));
        };
        token = Some(next);
    }

    Err(ArchiveError::MalformedListing(format!(
        "delimited listing for prefix {prefix:?} did not terminate within \
         {MAX_LIST_PAGES} pages"
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
    let keys = collect_keys(
        &sources.s3_bucket_url(&sources.level2_bucket),
        &prefix,
        None,
        |url| get_text(client, url),
    )
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
    let url = sources.s3_object_url(&sources.level2_bucket, &key);

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
mod tests;
