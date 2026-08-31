use std::string::FromUtf8Error;

use thiserror::Error;

use crate::Compression;

/// A specialized [`Result`] type for `PMTiles` operations.
pub type PmtResult<T> = Result<T, PmtError>;

/// Errors that can occur while reading `PMTiles` files.
#[derive(Debug, Error)]
pub enum PmtError {
    /// Invalid magic number in the `PMTiles` file header.
    #[error("Invalid magic number")]
    InvalidMagicNumber,
    /// Unsupported `PMTiles` version.
    #[error("Invalid PMTiles version")]
    UnsupportedPmTilesVersion,
    /// Invalid compression type specified.
    #[error("Invalid compression")]
    InvalidCompression,
    /// Compression type is not supported.
    #[error("Unsupported compression {0:?}")]
    UnsupportedCompression(Compression),
    /// The `PMTiles` entry is invalid.
    #[error("Invalid PMTiles entry")]
    InvalidEntry,
    /// The `PMTiles` header is invalid.
    #[error("Invalid header")]
    InvalidHeader,
    /// The `PMTiles` metadata is invalid.
    #[error("Invalid metadata")]
    InvalidMetadata,
    #[cfg(feature = "write")]
    /// Directory index entry overflow occurred during writing.
    #[error("Directory index element overflow")]
    IndexEntryOverflow,
    /// Metadata contains invalid UTF-8 encoding.
    #[error("Invalid metadata UTF-8 encoding: {0}")]
    InvalidMetadataUtf8Encoding(#[from] FromUtf8Error),
    /// The tile type is invalid.
    #[error("Invalid tile type")]
    InvalidTileType,
    /// An I/O error occurred while reading.
    #[error("IO Error {0}")]
    Reading(#[from] std::io::Error),
    /// Unexpected number of bytes returned during reading.
    #[error("Unexpected number of bytes returned [expected: {0}, received: {1}].")]
    UnexpectedNumberOfBytesReturned(usize, usize),
    #[cfg(feature = "http-async")]
    /// The server does not support range requests.
    #[error("Range requests unsupported")]
    RangeRequestsUnsupported,
    #[cfg(feature = "http-async")]
    /// The HTTP response body exceeded the requested length.
    #[error("HTTP response body is too long, Response {0}B > requested {1}B")]
    ResponseBodyTooLong(usize, usize),
    #[cfg(feature = "http-async")]
    /// An HTTP client error occurred.
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[cfg(feature = "http-async")]
    /// Invalid header value encountered.
    #[error(transparent)]
    InvalidHeaderValue(#[from] reqwest::header::InvalidHeaderValue),
    /// The underlying data source was modified since the reader was created.
    #[error("Underlying data source was modified")]
    SourceModified,
    /// The tile coordinate is invalid.
    #[error("Invalid coordinate {0}/{1}/{2}")]
    InvalidCoordinate(u8, u32, u32),
    /// The tile ID is invalid.
    #[error("Tile ID is too large. Got {0}")]
    InvalidTileId(u64),
    /// Indicates an error occurred with the directory cache.
    #[error("An error occurred with the directory cache: {0}")]
    DirectoryCacheError(String),
}
