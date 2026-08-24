//! Anonymous S3 access to the NEXRAD Level II archive bucket.
//!
//!
//! `unidata-nexrad-level2` is anonymously readable: an unsigned
//! `GET https://unidata-nexrad-level2.s3.amazonaws.com/?list-type=2&prefix=...`
//! returns `200` and a `ListBucketResult`, so there is no SigV4, no credential
//! chain and no clock-skew handling here. Upstream did not sign either.

use std::sync::OnceLock;

use chrono::NaiveDate;
use reqwest::StatusCode;
use xml::reader::{EventReader, XmlEvent};

use crate::sources::DataSources;

/// How long a single archive request may take, end to end. Upstream had *no*
const ARCHIVE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Upper bound on `ListObjectsV2` pages followed for one listing. A real
const MAX_LIST_PAGES: usize = 100;

/// Failures reaching or interpreting the archive bucket. Consumers render
#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error("S3 request failed: {0}")]
    Request(#[from] reqwest::Error),

    /// Distinct from [`ArchiveError::Status`]: "not in the archive" is an
    #[error("S3 object not found: {0}")]
    NotFound(String),

    /// Any other non-`200`.
    #[error("S3 returned {status} for {url}{}", body.as_deref().map(|b| format!(": {b}")).unwrap_or_default())]
    Status {
        status: StatusCode,
        url: String,
        body: Option<String>,
    },

    #[error("malformed S3 listing: {0}")]
    MalformedListing(String),

    /// An [`Identifier`] carries a name no object key can be derived from.
    #[error("cannot derive an archive key from identifier {0:?}")]
    UnkeyableIdentifier(String),
}

pub type Result<T> = std::result::Result<T, ArchiveError>;

/// A NEXRAD archive volume file, named but not keyed: a newtype over the bare
/// object *name*, with site and collection time recovered by fixed-offset
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

/// Build the `ListObjectsV2` URL for one page of a prefix query.
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
fn key_to_identifier(key: &str) -> Identifier {
    Identifier::new(key.split('/').skip(4).collect::<String>())
}

/// One page of a `ListObjectsV2` response.
#[derive(Debug, Default, PartialEq, Eq)]
struct ListPage {
    /// In the order S3 returned them, which is UTF-8 binary order.
    keys: Vec<String>,
    /// The `CommonPrefixes` a delimited listing collapsed everything below into.
    common_prefixes: Vec<String>,
    truncated: bool,
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

/// The shared, pooled client for every archive request.
pub(crate) fn shared_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        crate::tls::client(crate::tls::USER_AGENT, ARCHIVE_TIMEOUT)
            .build()
            .unwrap_or_else(|e| panic!("failed to build the archive HTTP client: {e}"))
    })
}

/// How a response status should be interpreted. Split out from the request so
/// the mapping is testable without a socket.
#[derive(Debug, PartialEq, Eq)]
enum StatusClass {
    Ok,
    NotFound,
    Failed,
}

/// Only `200` is success: this module never requests a partial or conditional
fn classify(status: StatusCode) -> StatusClass {
    match status {
        StatusCode::OK => StatusClass::Ok,
        StatusCode::NOT_FOUND => StatusClass::NotFound,
        _ => StatusClass::Failed,
    }
}

/// GET a URL and return the body as text, or an error describing the status.
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

/// The volumes the archive holds for one site-day, in the bucket's own key
/// order -- `crate::scan` relies on that order to break the tie between a
/// volume and its `_MDM` sidecar by taking the first match.
pub async fn list_files(
    sources: &DataSources,
    site: &str,
    date: &NaiveDate,
) -> Result<Vec<Identifier>> {
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

/// Download the volume an identifier names, as the bytes the bucket served.
/// The bucket comes from [`DataSources::level2_bucket`] — see [`list_files`].
pub async fn download_file(sources: &DataSources, identifier: Identifier) -> Result<Vec<u8>> {
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
            Ok(data)
        }
        StatusClass::NotFound => Err(ArchiveError::NotFound(key)),
        StatusClass::Failed => {
            let status = response.status();
            let body = response.text().await.ok();
            Err(ArchiveError::Status { status, url, body })
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
