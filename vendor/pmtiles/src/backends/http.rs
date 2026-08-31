use reqwest::header::{ETAG, HeaderValue, LAST_MODIFIED, RANGE};
use reqwest::{Client, IntoUrl, Method, Request, StatusCode, Url};

use crate::{
    AsyncBackend, AsyncPmTilesReader, BackendResponse, DirectoryCache, NoCache, PmtError, PmtResult,
};

impl AsyncPmTilesReader<HttpBackend, NoCache> {
    /// Creates a new `PMTiles` reader from a URL using the Reqwest backend.
    ///
    /// Fails if `url` does not exist or is an invalid archive. (Note: HTTP requests are made to validate it.)
    ///
    /// # Errors
    ///
    /// This function will return an error if the
    /// - URL is invalid,
    /// - the backend fails to read the header/root directory,
    /// - or if the root directory is malformed
    pub async fn new_with_url<U: IntoUrl>(client: Client, url: U) -> PmtResult<Self> {
        Self::new_with_cached_url(NoCache, client, url).await
    }
}

impl<C: DirectoryCache + Sync + Send> AsyncPmTilesReader<HttpBackend, C> {
    /// Creates a new `PMTiles` reader with cache from a URL using the Reqwest backend.
    ///
    /// Fails if `url` does not exist or is an invalid archive. (Note: HTTP requests are made to validate it.)
    ///
    /// # Errors
    ///
    /// This function will return an error if the
    /// - URL is invalid,
    /// - the backend fails to read the header/root directory,
    /// - or if the root directory is malformed
    pub async fn new_with_cached_url<U: IntoUrl>(
        cache: C,
        client: Client,
        url: U,
    ) -> PmtResult<Self> {
        let backend = HttpBackend::try_from(client, url)?;

        Self::try_from_cached_source(backend, cache).await
    }
}

/// Backend for reading `PMTiles` over HTTP.
pub struct HttpBackend {
    client: Client,
    url: Url,
}

impl HttpBackend {
    /// Creates a new HTTP backend.
    ///
    /// # Errors
    ///
    /// This function will return an error if the URL cannot be parsed into a valid URL.
    pub fn try_from<U: IntoUrl>(client: Client, url: U) -> PmtResult<Self> {
        Ok(HttpBackend {
            client,
            url: url.into_url()?,
        })
    }
}

impl AsyncBackend for HttpBackend {
    async fn read(&self, offset: u64, length: usize) -> PmtResult<BackendResponse> {
        // VENDORED: `offset` is `u64`; the arithmetic that builds the Range
        // header is done in `u64` so it cannot wrap on a 32-bit target.
        let end = offset + length as u64 - 1;
        let range = format!("bytes={offset}-{end}");
        let range = HeaderValue::try_from(range)?;

        let mut req = Request::new(Method::GET, self.url.clone());
        req.headers_mut().insert(RANGE, range);

        let response = self.client.execute(req).await?.error_for_status()?;
        if response.status() != StatusCode::PARTIAL_CONTENT {
            return Err(PmtError::RangeRequestsUnsupported);
        }

        let headers = response.headers();
        let data_version_header_value = headers.get(ETAG).or_else(|| headers.get(LAST_MODIFIED));
        let data_version = data_version_header_value
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);

        let response_bytes = response.bytes().await?;

        if response_bytes.len() > length {
            Err(PmtError::ResponseBodyTooLong(response_bytes.len(), length))
        } else {
            Ok(match data_version {
                Some(v) => BackendResponse::new_with_version(response_bytes, v),
                None => BackendResponse::new(response_bytes),
            })
        }
    }
}

// VENDORED: upstream's `mod tests` is deleted. One of its four fns hits
// the live network (protomaps.github.io), which a gate must not depend on,
// and the other three are built on `mockito` -- a dev-dependency this copy
// does not carry. Nothing in this workspace constructs an `HttpBackend`;
// the reader reaches every archive through `squallar-egui`'s own
// `RangeBackend`. See VENDORED.md.
