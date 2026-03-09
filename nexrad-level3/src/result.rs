//! Error and Result types for Level III decoding.

use thiserror::Error as ThisError;

/// A specialized `Result` type for Level III operations.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors that can occur during Level III product decoding.
#[derive(ThisError, Debug)]
pub enum Error {
    /// The input data is too short to contain the expected structure.
    #[error("unexpected end of data: need {expected} bytes at offset {offset}, have {available}")]
    UnexpectedEof {
        /// Byte offset into the data where the read was attempted.
        offset: usize,
        /// Number of bytes needed.
        expected: usize,
        /// Number of bytes actually available from `offset`.
        available: usize,
    },

    /// An invalid or unsupported message code was encountered.
    #[error("unsupported message code: {0}")]
    UnsupportedMessageCode(i16),

    /// An invalid or unsupported data packet code was encountered in the symbology block.
    #[error("unsupported data packet code: 0x{0:04X}")]
    UnsupportedPacketCode(u16),

    /// The symbology block header is invalid.
    #[error("invalid symbology block: {0}")]
    InvalidSymbologyBlock(String),

    /// Decompression of zlib-compressed product data failed.
    #[error("decompression failed: {0}")]
    DecompressionFailed(#[from] std::io::Error),

    /// A required field in the Product Description Block has an invalid value.
    #[error("invalid product description: {0}")]
    InvalidProductDescription(String),
}
