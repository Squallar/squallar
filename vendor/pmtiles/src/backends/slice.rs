//! VENDORED — this file is not upstream's.
//!
//! It replaces `src/backends/mmap.rs`, which was `fmmap`/`memmap2` and cost a
//! dependency this workspace will not carry for a test helper. Everything the
//! removed file was used for here is a whole small archive read into memory:
//! upstream's own test suite opens a fixture and reads tiles back out of it,
//! and `std::fs::read` does that with no dependency at all.
//!
//! It is also the shape the reader's contract is most cheaply stated in. A
//! backend is "bytes at a `u64` offset"; this one is the identity case.

use std::io;
use std::path::Path;

use bytes::Bytes;

use crate::{
    AsyncBackend, AsyncPmTilesReader, BackendResponse, DirectoryCache, NoCache, PmtError, PmtResult,
};

impl AsyncPmTilesReader<SliceBackend, NoCache> {
    /// Creates a new `PMTiles` reader from a file path, reading it into memory.
    ///
    /// # Errors
    ///
    /// This function will return an error if the
    /// - file cannot be read,
    /// - backend fails to read the header/root directory or
    /// - root directory is malformed
    pub async fn new_with_path<P: AsRef<Path>>(path: P) -> PmtResult<Self> {
        Self::new_with_cached_path(NoCache, path).await
    }
}

impl<C: DirectoryCache + Sync + Send> AsyncPmTilesReader<SliceBackend, C> {
    /// Creates a new cached `PMTiles` reader from a file path, reading it into
    /// memory.
    ///
    /// # Errors
    ///
    /// This function will return an error if the
    /// - file cannot be read,
    /// - backend fails to read the header/root directory or
    /// - root directory is malformed
    pub async fn new_with_cached_path<P: AsRef<Path>>(cache: C, path: P) -> PmtResult<Self> {
        let backend = SliceBackend::try_from(path).await?;

        Self::try_from_cached_source(backend, cache).await
    }
}

/// Backend for reading a `PMTiles` archive that is already in memory.
pub struct SliceBackend {
    bytes: Bytes,
}

impl SliceBackend {
    /// Creates a backend over bytes already in hand.
    #[must_use]
    pub fn new(bytes: Bytes) -> Self {
        Self { bytes }
    }

    /// Reads a whole file into memory and creates a backend over it.
    ///
    /// # Errors
    ///
    /// This function will return an error if the file cannot be read.
    pub async fn try_from<P: AsRef<Path>>(p: P) -> PmtResult<Self> {
        Ok(Self::new(Bytes::from(std::fs::read(p)?)))
    }
}

impl AsyncBackend for SliceBackend {
    async fn read(&self, offset: u64, length: usize) -> PmtResult<BackendResponse> {
        // The `u64` offset is narrowed here and ONLY here, and it is checked:
        // this backend's bytes are in this process's memory, so an offset past
        // `usize::MAX` is by construction past the end of them.
        let Ok(start) = usize::try_from(offset) else {
            return Err(PmtError::Reading(io::Error::from(
                io::ErrorKind::UnexpectedEof,
            )));
        };
        let start = start.min(self.bytes.len());
        let end = start.saturating_add(length).min(self.bytes.len());
        Ok(BackendResponse::new(self.bytes.slice(start..end)))
    }
}
