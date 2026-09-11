//! **Off-heap retention for the compressed half of a loop frame.**
//!
//! An archive is the cheapest retention this application has, and it is also
//! the precondition for evicting the decoded volume at all: both
//! decoded-eviction policies refuse a volume with no archive behind it, by
//! design, because their cost is a decode and a volume with nothing to decode
//! from would turn them into a re-download policy. So the archive ceiling
//! cannot simply be lowered. Taking an archive strands the larger decoded
//! volume as permanently un-evictable, which is why
//! `LOOP_ARCHIVE_CEILING_BYTES` carries a paragraph telling the reader not to.
//!
//! **How much larger is a question about the corpus, and the 15.5x this file
//! used to quote is the wrong corpus's answer.** Measured per volume as
//! `scan_size::scan_bytes` against the compressed file it was decoded from:
//!
//! | corpus | archive as % of decoded (min / median / max) | median leverage |
//! |---|---|---|
//! | 208-file `rd-t18-seam-corpus`, real Archive II | 12.9 / 35.1 / 53.5 % | **2.85x** |
//! | 171-file `nrot-synth`, synthesised | 5.4 / 9.1 / 40.3 % | 10.99x |
//!
//! The 6.45 % / 15.5x figure was taken over 39 volumes of the synthetic
//! corpus, is stated at `LOOP_ARCHIVE_CEILING_BYTES` with that denominator
//! named, and **arrives everywhere else in this tree with the denominator
//! dropped** — including at the two eviction policies, which run on real
//! archives and not on synthesised ones. On the real corpus the trade is
//! 2.85x, not 15.5x: trading a decoded volume for its archive gives back
//! 65 % of the frame's bytes rather than 94 %.
//!
//! **That does not reverse any decision here — it rescales them.** An archive
//! is still the cheapest retention available and a dropped one still strands a
//! volume 2.85x its size, so both the prohibition and this module survive; the
//! arithmetic every one of them is argued from is 5.4x less favourable than
//! the prose said.
//!
//! This module is the third option. Where the byte ceiling would DROP an
//! archive, the bytes go to disk and the **key stays in memory**, so every
//! question about whether a way back exists is still answered out of a
//! `HashMap` without touching a disk, and only the withdrawal of the bytes
//! pays for I/O.
//!
//! # Priced against what the alternative actually is
//!
//! Three costs over the 208-file local Archive II corpus at its minimum,
//! median, p90 and maximum compressed size — median of five draws, release,
//! on a box at loadavg 2.75. The **write** this module pays to put an archive
//! on the medium; the **cold read** to get it back, page cache dropped with
//! `posix_fadvise(POSIX_FADV_DONTNEED)`; and the single-threaded **bzip2
//! decode** that always follows the read, measured on the production path
//! (`scan::decode_shared`, the one call `jobs::DecodeJob::run` makes).
//!
//! | archive | spill write | cold read | bzip2 decode | read / decode |
//! |---|---|---|---|---|
//! | 0.34 MiB | 0.08 ms | 1.12 ms | 5.05 ms | 22 % |
//! | 5.58 MiB | 0.79 ms | 11.21 ms | 17.23 ms | 65 % |
//! | 10.99 MiB | 1.53 ms | 17.49 ms | 25.39 ms | 69 % |
//! | 17.96 MiB | 2.36 ms | 27.14 ms | 41.70 ms | 65 % |
//!
//! **Which figure is the price depends on which alternative is named, and
//! this module has two.** Against an archive that stays on the HEAP, the
//! whole marginal cost of putting this one on a medium is the write plus the
//! read — 1.2 ms to 29.5 ms — because the decode is paid either way and is
//! not a cost this module adds. Against an archive that is DROPPED, which is
//! the behaviour this module replaces, the comparison is not a decode at all:
//! a dropped archive is a volume with no way back, so the frame costs a
//! network round trip, and the whole table is small against that. Both
//! comparisons say spill. Neither of them is the comparison this file made
//! before 2026-09-11.
//!
//! **The decode column was overstated ~20x and is retracted** (`dfc61b542`).
//! It read 19.3 / 305.8 / 915.8 ms, and no release build of this tree
//! reproduces it; the column above was re-measured independently and agrees
//! with `dfc61b542`'s 4.7 / 15.2 / 25.1 / 41.3 ms to within a draw, and with
//! the release-profile reading at `LOOP_DECODED_LOOKAHEAD_FRAMES` (6.0 / 10.7
//! / 40.9 ms over a different corpus). What produced the old figures is not
//! established, so they are retracted rather than explained.
//!
//! **"A restore is I/O-bound, not CPU-bound" is retracted with them, and it
//! was an artefact of a loaded box.** That claim came from a read column
//! taken at loadavg 18–73 (1.23 / 19.09 / 35.04 / 55.18 ms), which put the
//! read at 134 % of the decode at the corpus maximum. Re-measured at loadavg
//! 2.75 the read is 1.12 / 11.21 / 17.49 / 27.14 ms and never exceeds the
//! decode: it is 22–69 % of it across the corpus, and a restore is
//! decode-dominated on a quiet box. The read is the load-sensitive half and
//! the decode is the CPU half, so on a busy box the two converge — but they
//! do not invert, and nothing here should be argued from the inverted
//! ordering.
//!
//! **The write is the cheap one and was never priced at all.** At 0.08–2.36 ms
//! it is 5–6 % of the decode. That matters for a ceiling low enough to spill
//! the same archive repeatedly: the recurring cost of such a treadmill is the
//! decode, not this module.
//!
//! # `read`, never `mmap` — the two counters do not move together
//!
//! The bytes are read into a fresh `Vec` and the file is closed. **A mapping
//! would be the same apparent cut on the wrong counter.**
//! `squallar_alloc::live_bytes` counts what the global allocator granted less
//! what it was handed back; a mapping never passes through the allocator at
//! all. So an `mmap` posts the *full* saving on the campaign's target metric
//! while the resident pages merely move from `RssAnon` to `RssFile` and stay
//! in `VmRSS` — the process costs the machine exactly what it did before.
//! `squallar_alloc::process::Resident` documents `RssFile` as already being
//! most of a 250 MiB budget on this workspace's arm before a byte of weather
//! data, so growing it is worse than neutral.
//!
//! **Nothing on this path may be memory-mapped, tests included**, so that the
//! pattern cannot be copied back out of a fixture.
//!
//! # What bounds the disk
//!
//! A byte ceiling that moves its overflow to a medium which also runs out is
//! a leak with a longer fuse. Two things bound it, and neither needs a
//! sweeper:
//!
//! 1. **A refusal, not an eviction.** `LoopDownloadManager` will not spill
//!    when the spill is already at
//!    `LOOP_ARCHIVE_SPILL_CEILING_BYTES`; it drops the archive instead, which
//!    is exactly today's behaviour. The disk therefore has a hard bound
//!    without a second eviction policy to get wrong.
//! 2. **The frame list reclaims it.** `retain_archives` deletes the spilled
//!    file of every frame the loop stops naming, on the same pass and by the
//!    same predicate that drops the heap-held ones, so the spill follows the
//!    frame list down as well as up.
//!
//! # A crash, and a directory from a previous run
//!
//! A spilled file whose in-memory key is gone is unreachable, and nothing
//! would ever collect it. Rather than reconstruct an index by trusting
//! filenames as timestamps, [`FsArchiveSpill::new`] **purges its directory on
//! construction**. The spill is a within-run cache: an archive's usefulness is
//! bounded by the loop's two-hour span, and a restart re-lists anyway, so
//! there is nothing in there a new process wants. A crash therefore leaks at
//! most one spill ceiling of disk until the next start, and nothing is ever
//! read back across processes.
//!
//! Writes land through a `.part` file and a rename, so an interrupted write
//! cannot leave a short file that [`ArchiveSpill::load`] would hand back as a
//! truncated archive for a decoder to fail on.

/// **Somewhere other than the heap to keep a compressed archive, keyed as the
/// caches key it.**
///
/// Three verbs. Deliberately **not** `squallar_kv::KvStore`, whose charter
/// test pins exactly `load`/`store`/`store_now` and says why: *"no
/// enumeration, no deletion, no transactions; … if a consumer needs a fourth
/// verb it is asking for a database, and this crate is deliberately not one."*
/// A byte ceiling needs deletion, so this is a separate contract rather than a
/// fourth verb on that one. That crate's wasm implementation is also
/// `LocalStorageKvStore`, at roughly 5–10 MB for a whole origin, against the
/// 250.8 MiB this exists to move.
///
/// There is no implementation on `wasm32`: the manager holds an
/// `Option<Box<dyn ArchiveSpill>>` and the web target is the `None` branch,
/// which is a `cfg` selecting a **value** and never a fork inside a function
/// body. Web needs IndexedDB — asynchronous, with its own quota story, and
/// Firefox governing over Chrome — and that is a separate piece of work.
pub trait ArchiveSpill: Send + Sync {
    /// Put these bytes where `load` will find them. `false` means it did not
    /// happen and the caller still owns the problem — it must then behave as
    /// though no spill existed.
    fn store(&self, site: &str, ts: chrono::NaiveDateTime, bytes: &[u8]) -> bool;

    /// The bytes back, or `None` if they are not there. **Reads into a fresh
    /// `Vec`; never maps.**
    fn load(&self, site: &str, ts: chrono::NaiveDateTime) -> Option<Vec<u8>>;

    /// Forget these bytes. Absent is success — the caller is asserting the
    /// end state, not that it won a race.
    fn delete(&self, site: &str, ts: chrono::NaiveDateTime);
}

/// [`ArchiveSpill`] over a directory of files, one per archive.
#[cfg(not(target_arch = "wasm32"))]
pub struct FsArchiveSpill {
    root: std::path::PathBuf,
}

#[cfg(not(target_arch = "wasm32"))]
impl FsArchiveSpill {
    /// **Purges `root` and recreates it**, for the reason in the module note:
    /// a file whose in-memory key did not survive the process is unreachable
    /// and nothing else would ever collect it.
    ///
    /// `None` if the directory cannot be made, which is a machine without room
    /// or permission for a spill; the caller then runs with no spill at all
    /// rather than with one that silently fails every store.
    pub fn new(root: std::path::PathBuf) -> Option<Self> {
        // Purge before create, and ignore the removal's own error: the
        // directory not being there is the state this wants.
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).ok()?;
        Some(Self { root })
    }

    /// `None` for a site this will not put in a path.
    ///
    /// The site string comes from a listing, so it is not this module's to
    /// trust: anything but ASCII alphanumerics is refused rather than escaped,
    /// which keeps a `..` or a separator out of the join by construction. Real
    /// sites are ICAO identifiers and pass.
    fn path(&self, site: &str, ts: chrono::NaiveDateTime) -> Option<std::path::PathBuf> {
        if site.is_empty() || !site.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return None;
        }
        // Fixed width and no separators, so one stamp is one filename.
        let stamp = ts.format("%Y%m%d%H%M%S%6f");
        Some(self.root.join(format!("{site}-{stamp}.spill")))
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl ArchiveSpill for FsArchiveSpill {
    fn store(&self, site: &str, ts: chrono::NaiveDateTime, bytes: &[u8]) -> bool {
        let Some(path) = self.path(site, ts) else {
            return false;
        };
        // Through a `.part` and a rename: a write cut short by a crash must
        // not leave a short file that `load` hands back as a whole archive.
        let part = path.with_extension("part");
        if std::fs::write(&part, bytes).is_err() {
            let _ = std::fs::remove_file(&part);
            return false;
        }
        if std::fs::rename(&part, &path).is_err() {
            let _ = std::fs::remove_file(&part);
            return false;
        }
        true
    }

    fn load(&self, site: &str, ts: chrono::NaiveDateTime) -> Option<Vec<u8>> {
        // `read`, not a mapping. See the module note: a mapping moves the
        // bytes between RSS classes without leaving the resident set, and
        // posts a full saving on `live_bytes` for no change in what the
        // process costs.
        std::fs::read(self.path(site, ts)?).ok()
    }

    fn delete(&self, site: &str, ts: chrono::NaiveDateTime) {
        if let Some(path) = self.path(site, ts) {
            let _ = std::fs::remove_file(path);
        }
    }
}

// **`all(test, not(wasm32))`, not a plain `cfg(test)`.** Everything below is
// native-only, and `--all-targets` compiles test modules for wasm too: a
// `cfg(test)` module reaching a native-only item reads green on the lib
// spelling and red on CI's, which is how 18 errors once sat on main.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
