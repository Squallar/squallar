//! Result and error types for NEXRAD data operations.

use thiserror::Error as ThisError;

/// A specialized Result type for NEXRAD data operations.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors that can occur during NEXRAD data operations.
#[derive(ThisError, Debug)]
pub enum Error {
    /// An I/O error occurred while reading or writing data.
    #[error("data file IO error")]
    Io(#[from] std::io::Error),
    /// Attempted to decompress data that is not compressed.
    #[error("error decompressing uncompressed data")]
    UncompressedData,
    /// An AWS-related error occurred (requires `aws` feature).
    #[cfg(feature = "aws")]
    #[error(transparent)]
    AWS(#[from] aws::AWSError),
    /// An error occurred during message decoding.
    #[error("error decoding NEXRAD data")]
    Decode(#[from] nexrad_decode::result::Error),
    /// An error occurred in the common model layer (requires `nexrad-model` feature).
    #[cfg(feature = "nexrad-model")]
    #[error("error in common model")]
    Model(#[from] nexrad_model::result::Error),
    /// Cannot decode compressed record data without decompression.
    #[error("compressed data cannot be decoded")]
    CompressedData,
    /// Cannot access records or scan from a gzip-compressed volume file.
    /// Call `decompress()` first.
    #[error("gzip-compressed volume file must be decompressed before accessing records")]
    CompressedFile,
    /// Volume file is missing the required VCP (message type 5).
    #[error("volume missing coverage pattern (message type 5)")]
    MissingCoveragePattern,
    /// BZIP2 decompression of an LDM record failed.
    ///
    /// Note that [`Record::decompress`](crate::volume::Record::decompress)
    /// does not produce this variant and never did: it drives the decoder
    /// through [`std::io::Read`], which converts a decoder error into an
    /// [`std::io::Error`], so a bad record surfaces as [`Error::Io`]. The
    /// variant exists so that a caller reaching the decoder directly has
    /// somewhere to put its error.
    #[error("ldm record decompression error")]
    // Upstream's payload here is `bzip2::Error`. Retyping it is the one edit
    // outside `src/volume/record.rs` that moving off `bzip2` forces, because a
    // public variant naming a type is a dependency on that type whether or not
    // anything constructs it -- and keeping `bzip2` as a normal dependency for
    // one unconstructed variant would put `libbz2-rs-sys` back in every
    // release binary, which is the whole thing the move exists to avoid.
    //
    // It is a public API change: a downstream `Err(Decompression(e))` pattern
    // still compiles, and anything naming the payload's type does not. Nothing
    // in this workspace does either -- this file is the only mention of the
    // variant in the entire tree.
    Decompression(#[from] bzip2_rs::decoder::DecoderError),
    /// LDM record was truncated and contains fewer bytes than expected.
    #[error("truncated record: expected {expected} bytes, got {actual}")]
    TruncatedRecord {
        /// Expected number of bytes.
        expected: usize,
        /// Actual number of bytes present.
        actual: usize,
    },
    /// LDM record decompresses to more than
    /// [`MAX_DECOMPRESSED_RECORD_BYTES`](crate::volume::MAX_DECOMPRESSED_RECORD_BYTES).
    ///
    /// A bzip2 stream does not declare its decompressed size, so this is the
    /// only point at which an over-large record can be detected. The record is
    /// abandoned at the ceiling rather than expanded to find out how big it
    /// really is, which is why no decompressed size is reported here.
    #[error("record decompresses past the {limit} byte ceiling (compressed size {compressed})")]
    RecordTooLarge {
        /// The ceiling that was exceeded, in bytes.
        limit: usize,
        /// The record's compressed size in bytes, excluding the size prefix.
        compressed: usize,
    },
    /// LDM record size is invalid at the given file offset.
    #[error("invalid record size {size} at offset {offset}")]
    InvalidRecordSize {
        /// The invalid size value.
        size: usize,
        /// File offset where the invalid size was found.
        offset: usize,
    },
}

/// AWS-related error types (requires `aws` feature).
#[cfg(feature = "aws")]
pub mod aws {
    use thiserror::Error as ThisError;

    /// Errors that can occur during AWS S3 operations.
    #[derive(ThisError, Debug)]
    pub enum AWSError {
        /// S3 list objects response was unexpectedly truncated.
        #[error("unexpected truncated S3 list objects response")]
        TruncatedListObjectsResponse,
        /// Failed to parse date/time from filename or metadata.
        #[error("error decoding date/time")]
        DateTime(String),
        /// The radar site identifier is not recognized.
        #[error("invalid radar site identifier")]
        InvalidSiteIdentifier(String),
        /// Real-time chunk data is in an unrecognized format.
        #[error("chunk data in unrecognized format")]
        UnrecognizedChunkFormat,
        /// Could not parse date/time from chunk filename.
        #[error("unrecognized chunk date time")]
        UnrecognizedChunkDateTime(String),
        /// Could not parse sequence number from chunk filename.
        #[error("unrecognized chunk sequence")]
        UnrecognizedChunkSequence(String),
        /// Chunk type character is not recognized.
        #[error("unrecognized chunk type")]
        UnrecognizedChunkType(Option<char>),
        /// S3 list objects request failed.
        #[error("error listing AWS S3 objects")]
        S3ListObjects(reqwest::Error),
        /// S3 get object request failed to send.
        #[error("error requesting AWS S3 object")]
        S3GetObjectRequest(reqwest::Error),
        /// S3 get object returned an error response.
        #[error("error getting AWS S3 object")]
        S3GetObject(Option<String>),
        /// Requested S3 object was not found (404).
        #[error("AWS S3 object not found")]
        S3ObjectNotFound,
        /// Error while streaming/downloading S3 object content.
        #[error("error streaming/downloading AWS S3 object")]
        S3Streaming(reqwest::Error),
        /// Could not find the latest volume for the requested site/date.
        #[error("failed to locate latest volume")]
        LatestVolumeNotFound,
        /// An expected chunk was not found during real-time polling.
        #[error("a chunk was not found as expected")]
        ExpectedChunkNotFound,
        /// Error in async channel communication during polling.
        #[error("error sending chunk to receiver")]
        PollingAsync,
        /// Could not determine the next chunk to poll.
        #[error("failed to determine next chunk")]
        FailedToDetermineNextChunk,
        /// Failed to decode XML response from S3 list objects.
        #[error("error decoding S3 list objects response")]
        S3ListObjectsDecoding,
    }
}
