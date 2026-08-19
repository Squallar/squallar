//! Filesystem-backed [`KvStore`], shared by desktop, Android and iOS. The
//! web build never compiles it and supplies a `localStorage` one instead.
//!
//! # The write does not happen on the calling thread
//!
//! Every caller of [`store`](KvStore::store) here is the frame thread:
//! `autosave_config` on its 3 s timer, `site_positions` learning where a radar
//! really is, the site catalogue landing from a fetch, the location memo. The
//! payloads are kilobytes, so `create_dir_all` plus `write` is usually well
//! under a millisecond — but *usually* is the whole problem. Neither call has
//! a bound: Android flash under write pressure and a network home directory
//! both stretch them past a frame, and this project does not leave unbounded
//! work on the thread that owes a frame every 16 ms because the common case is
//! fast.
//!
//! Only the bytes move. Serializing the config stays with the caller, because
//! `autosave_config` compares the serialized string against the last one it
//! wrote to decide whether to write at all — the value has to exist on the
//! frame thread for the comparison that avoids the write.
//!
//! # One dedicated thread, not the job pool
//!
//! This is the non-obvious choice, and it is about ordering rather than cost.
//! Two `store`s to the same key must land in the order they were made; if the
//! second one lands first, the file keeps the *older* config and nothing
//! reports it — the loss surfaces a session later as a layout that silently
//! reverted. A multi-threaded pool is free to run the second write first, so
//! it cannot offer that guarantee at all.
//!
//! A single consumer can, and cheaply. The producers are all one thread, so
//! the order requests enter the queue is the order the calls were made, and a
//! FIFO drained by exactly one thread preserves it end to end. That also makes
//! [`store_now`](KvStore::store_now) a flush of everything queued ahead of
//! it, since its own write cannot start until the earlier ones have finished.
//!
//! "The producers are all one thread" is an invariant the type system enforces,
//! not an observation about today's call sites: `LocationBridge::kv`
//! hands out a `Box<dyn KvStore>`, which is neither `Send` nor `Sync`, so a
//! store cannot reach a second thread to be called from. Anyone widening that
//! to `Arc<dyn KvStore + Send + Sync>` takes the ordering guarantee with
//! it — the queue would then be interleaving two producers, and which of two
//! writes to a key is "second" would stop being defined.
//!
//! # What is not guaranteed
//!
//! Delivery across process death. The queue lives behind a `static OnceLock`
//! whose destructor never runs, so the writer is killed by process teardown
//! with anything still queued discarded, and `exit_now` reaches
//! `process::exit` without joining it. Callers that may be about to die use
//! `store_now`; the write itself is atomic (see [`write_blob`]) so an
//! interrupted one loses the update rather than the file.
//!
//! # Why the web build is not fixed the same way
//!
//! `rustdar-web`'s `LocalStorageKvStore` writes with
//! `localStorage.set_item`, which is synchronous by specification and reachable
//! only from the main thread — a worker has no `window` and therefore no
//! `localStorage` at all. There is no thread to hand that write to, so the wasm
//! arm keeps writing inline. That is the platform refusing, not an arm left
//! half-done.

use rustdar_kv::KvStore;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::mpsc::{Sender, channel};

/// One unit of work for the writer thread.
enum WriteRequest {
    /// Put `value` at `path`, creating `dir` first if it is missing.
    Write {
        dir: PathBuf,
        path: PathBuf,
        value: String,
        /// Present when a caller is blocked on the outcome, i.e. `store_now`.
        /// Absent means nobody is listening and the writer owns the reporting.
        done: Option<Sender<Result<(), String>>>,
    },
    /// Reply once every request queued earlier has been handled.
    ///
    /// Tests only: production reaches the same barrier through `store_now`,
    /// which waits on a write it queued rather than on a bare marker.
    #[cfg(test)]
    Drain(Sender<()>),
    /// Park the writer until the paired sender sends or drops.
    ///
    /// Tests only, and the only way to observe the property this module exists
    /// for: with the writer parked, a queued write *cannot* have run, so the
    /// file's absence after `store` returns is proof that `store` deferred it
    /// rather than a race that happened to be won. Every other test here passes
    /// against a fully synchronous `store`.
    #[cfg(test)]
    Block(std::sync::mpsc::Receiver<()>),
}

/// How long [`store_now`](KvStore::store_now) waits for the writer.
///
/// Bounded, because `store_now` waits for everything queued ahead of it as well
/// as its own write: at an Android suspend that can be the UI config, the
/// learned site positions, the catalogue, the location memo and both device
/// memos, all paid inside a lifecycle callback that runs against a watchdog. An
/// unbounded wait there on the stalled flash this module exists for would turn
/// a dropped frame into a killed app, which is worse than the inline write it
/// replaced. Far longer than any healthy write, far short of the watchdog.
const STORE_NOW_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// The one thread that touches config files, started on first use.
///
/// Process-wide rather than per-store because `kv()` hands out a
/// fresh `FileKvStore` on every call — a thread per instance would be a
/// thread per call, and worse, would put concurrent writers back on the same
/// directory and lose the ordering the single consumer exists to provide.
///
/// `None` if the thread could not be spawned. Configuration is never allowed
/// to be load-bearing, so that degrades to writing inline rather than panicking
/// or silently dropping every write for the life of the process.
fn writer() -> Option<&'static Sender<WriteRequest>> {
    static WRITER: OnceLock<Option<Sender<WriteRequest>>> = OnceLock::new();
    WRITER
        .get_or_init(|| {
            let (tx, rx) = channel::<WriteRequest>();
            std::thread::Builder::new()
                .name("config-writer".to_owned())
                .spawn(move || {
                    // Nothing ends this loop in practice. `WRITER` is a static
                    // `OnceLock`, whose destructor never runs, so the `Sender`
                    // it holds never drops and the thread is killed by process
                    // teardown with whatever is still queued discarded. That is
                    // the reason the callers who may be about to die — the exit
                    // path, the suspend, the two device memos — use `store_now`
                    // rather than trusting this queue to get another turn.
                    for request in rx {
                        match request {
                            WriteRequest::Write {
                                dir,
                                path,
                                value,
                                done,
                            } => {
                                let result = write_blob(&dir, &path, &value);
                                match done {
                                    // `store_now`: the caller owns the outcome,
                                    // and an `Err` here means it gave up first.
                                    Some(done) => {
                                        let _ = done.send(result);
                                    }
                                    // Queued: the caller returned long ago, so
                                    // this is logged here or nowhere.
                                    None => {
                                        if let Err(e) = result {
                                            log::warn!("config write failed: {e}");
                                        }
                                    }
                                }
                            }
                            #[cfg(test)]
                            WriteRequest::Drain(reply) => {
                                let _ = reply.send(());
                            }
                            // `Err` when the test dropped its sender, including
                            // by panicking, so a failed test unparks rather
                            // than wedging every later one.
                            #[cfg(test)]
                            WriteRequest::Block(gate) => {
                                let _ = gate.recv();
                            }
                        }
                    }
                })
                .map_err(|e| log::warn!("no config writer thread ({e}); writing inline"))
                .ok()
                .map(|_handle| tx)
        })
        .as_ref()
}

/// Put `value` at `path`, creating `dir` first. The actual filesystem work.
///
/// Into a sibling temp file and then renamed, never straight onto `path`.
/// `fs::write` opens `O_TRUNC`, so a process that dies between the truncate and
/// the write leaves a zero-byte or half-written `ui.json` and the next launch
/// loses the *whole* layout rather than the last three seconds of it — and this
/// process can die exactly there, because `exit_now` calls `process::exit`
/// without joining this thread. A `rename` within one directory is atomic, so
/// every reader sees either the old file or the new one.
///
/// The temp name carries the pid so two instances of the app cannot land on one
/// another's half-written file.
fn write_blob(dir: &Path, path: &Path, value: &str) -> Result<(), String> {
    // On write, not up front: an unwritten store leaves no empty directory.
    if let Err(e) = std::fs::create_dir_all(dir) {
        return Err(format!("failed to create config dir {:?}: {}", dir, e));
    }
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&tmp, value).map_err(|e| format!("failed to write {:?}: {}", tmp, e))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        // The rename is what publishes the value. A failed one would otherwise
        // leave one orphan temp file per failure sitting in the config dir.
        let _ = std::fs::remove_file(&tmp);
        format!("failed to replace {:?}: {}", path, e)
    })
}

/// Wait until every request queued before this call has been handled.
///
/// A sentinel through the same queue, never a sleep. The writer takes requests
/// in order, so a reply to a marker enqueued after a write is proof the write
/// finished; a wall-clock wait would prove nothing and would fail on a loaded
/// machine.
///
/// The queue is process-wide, so this is a *global* barrier: it waits on every
/// test's queued writes, not just the calling test's. That is sound as a
/// barrier — each test owns its own directory, so waiting for more than you
/// queued can only ever wait longer — but it means this cannot be used to
/// assert that some *other* test's write has not happened yet.
#[cfg(test)]
fn flush_writes() {
    let Some(writer) = writer() else {
        // No thread, so `store` already wrote inline and there is nothing
        // queued to wait for.
        return;
    };
    let (reply, done) = channel();
    if writer.send(WriteRequest::Drain(reply)).is_ok() {
        let _ = done.recv();
    }
}

/// Stores each key as `<dir>/<key>.json`.
pub struct FileKvStore {
    dir: PathBuf,
}

impl FileKvStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn path_for(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{}.json", key))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

impl KvStore for FileKvStore {
    fn load(&self, key: &str) -> Option<String> {
        let path = self.path_for(key);
        match std::fs::read_to_string(&path) {
            Ok(content) => Some(content),
            Err(e) => {
                // A first run has no config yet; that is not worth a warning.
                if e.kind() != std::io::ErrorKind::NotFound {
                    log::warn!("Failed to read config {:?}: {}", path, e);
                }
                None
            }
        }
    }

    /// Queue the write and return; see the module docs for why it leaves this
    /// thread.
    ///
    /// `Ok` means the request was accepted, not that anything is on disk. A
    /// write that then fails is logged by the writer thread, which is the only
    /// place left that knows about it — the caller is several frames gone. That
    /// costs one thing worth naming: `autosave_config` records the value as
    /// written on `Ok`, so a failing disk is no longer retried on the next
    /// 3 s tick, only when the config next changes. The alternative is
    /// reporting a *previous* write's failure against the current call, which
    /// would be a worse lie than the one it fixes.
    fn store(&self, key: &str, value: &str) -> Result<(), String> {
        let path = self.path_for(key);
        let Some(writer) = writer() else {
            return write_blob(&self.dir, &path, value);
        };
        let queued = writer.send(WriteRequest::Write {
            dir: self.dir.clone(),
            path: path.clone(),
            value: value.to_owned(),
            done: None,
        });
        if queued.is_err() {
            // The thread died after it started — a panic in the writer, which
            // `WRITER` cannot notice because a `OnceLock` is written once. Take
            // the write here rather than returning `Err` forever, which is what
            // the doc above promises cannot happen.
            return write_blob(&self.dir, &path, value);
        }
        Ok(())
    }

    /// Write, and wait for it.
    ///
    /// Through the same queue rather than straight to the filesystem, which is
    /// the point: a direct write here could overtake a queued one for the same
    /// key and leave the older config on disk — exactly the reordering the
    /// single writer thread exists to prevent, reintroduced by the call that
    /// cares most. Going through the queue also drains everything ahead of it,
    /// so the suspend that calls this flushes the learned site positions and
    /// the memos too.
    ///
    /// The returned `Result` is this write's own. An earlier queued write that
    /// failed was reported by the writer thread and is invisible here, so `Ok`
    /// means "this value is on disk", never "every pending value is".
    ///
    /// Bounded by [`STORE_NOW_TIMEOUT`], because waiting for the whole backlog
    /// is what makes this correct *and* what makes it expensive: on the suspend
    /// path an unbounded wait on a stalled filesystem would trade a dropped
    /// frame for an app the watchdog kills.
    fn store_now(&self, key: &str, value: &str) -> Result<(), String> {
        let path = self.path_for(key);
        let Some(writer) = writer() else {
            return write_blob(&self.dir, &path, value);
        };
        let (done, outcome) = channel();
        let queued = writer.send(WriteRequest::Write {
            dir: self.dir.clone(),
            path: path.clone(),
            value: value.to_owned(),
            done: Some(done),
        });
        if queued.is_err() {
            // Writer gone; see `store`.
            return write_blob(&self.dir, &path, value);
        }
        match outcome.recv_timeout(STORE_NOW_TIMEOUT) {
            Ok(result) => result,
            // The writer is stuck inside a write, on the slow storage this
            // module exists for. Deliberately *not* retried inline: the queued
            // write is still live and will land, and a second writer racing it
            // could publish this value and then have the stuck one replace it.
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(format!(
                "config write for {:?} did not finish within {:?}; it is still queued",
                path, STORE_NOW_TIMEOUT
            )),
            // The thread died holding this request, so nothing else will do it.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                write_blob(&self.dir, &path, value)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp directory that removes itself on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let mut path = std::env::temp_dir();
            path.push(format!(
                "rustdar-kv-{}-{}-{:?}",
                tag,
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_stored_value_reads_back() {
        let dir = TempDir::new("roundtrip");
        let store = FileKvStore::new(dir.0.clone());

        assert_eq!(store.load("ui"), None, "nothing stored yet");
        store
            .store("ui", "{\"a\":1}")
            .expect("store should succeed");
        flush_writes();
        assert_eq!(store.load("ui"), Some("{\"a\":1}".to_string()));
    }

    #[test]
    fn different_keys_do_not_collide() {
        let dir = TempDir::new("keys");
        let store = FileKvStore::new(dir.0.clone());

        store.store("ui", "first").unwrap();
        store.store("other", "second").unwrap();
        flush_writes();

        assert_eq!(store.load("ui").as_deref(), Some("first"));
        assert_eq!(store.load("other").as_deref(), Some("second"));
    }

    /// The on-disk name is load-bearing: existing installs have a `ui.json`.
    #[test]
    fn the_ui_key_maps_to_ui_json() {
        let dir = TempDir::new("filename");
        let store = FileKvStore::new(dir.0.clone());
        store.store(rustdar_egui::UI_CONFIG_KEY, "payload").unwrap();
        flush_writes();

        let on_disk = dir.0.join("ui.json");
        assert!(on_disk.exists(), "expected {:?} to exist", on_disk);
        assert_eq!(std::fs::read_to_string(&on_disk).unwrap(), "payload");
    }

    /// The first-run path on every platform.
    #[test]
    fn store_creates_a_missing_directory() {
        let dir = TempDir::new("mkdir");
        let nested = dir.0.join("deeper");
        assert!(!nested.exists());

        let store = FileKvStore::new(nested.clone());
        store.store("ui", "x").expect("store should create the dir");
        flush_writes();
        assert!(nested.join("ui.json").exists());
    }

    /// The property this module exists for, and the only test here that fails
    /// against a synchronous `store`.
    ///
    /// Every other test in this file passes whether `store` defers or writes
    /// inline, which makes them tests of the *result* and not of where the work
    /// happened. Parking the writer first is what turns "the file is not there
    /// yet" from a race won into a fact: the writer provably cannot have run,
    /// so if the bytes are on disk, `store` put them there on this thread.
    #[test]
    fn store_returns_before_the_bytes_land() {
        let dir = TempDir::new("deferred");
        let store = FileKvStore::new(dir.0.clone());
        let writer = writer().expect("this test is about the writer thread");

        // Parked before anything is queued. Dropping `release` — including by
        // panicking below — unparks it, so a failure here cannot wedge the
        // tests that follow.
        let (release, gate) = channel();
        writer
            .send(WriteRequest::Block(gate))
            .expect("park the writer");

        store.store("ui", "deferred").expect("store should queue");
        assert!(
            !dir.0.join("ui.json").exists(),
            "the writer is parked and the bytes are already on disk, so store \
             wrote them on the calling thread"
        );

        drop(release);
        flush_writes();
        assert_eq!(
            store.load("ui").as_deref(),
            Some("deferred"),
            "the write never landed after the writer was released"
        );
    }

    /// The whole point of the queue: `store` returns before the bytes land, and
    /// the drain marker — not a sleep — is what says they have.
    #[test]
    fn a_queued_write_lands_by_the_time_the_queue_is_drained() {
        let dir = TempDir::new("queued");
        let store = FileKvStore::new(dir.0.clone());

        store.store("ui", "queued").expect("store should queue");
        flush_writes();

        assert_eq!(
            store.load("ui").as_deref(),
            Some("queued"),
            "a write the queue accepted never reached {:?}",
            dir.0
        );
    }

    /// The reason this is one thread and not the job pool.
    ///
    /// Two writes to one key, last one wins — which is only true if they are
    /// applied in the order they were made. A pool that ran the second first
    /// would leave `first` on disk and report nothing.
    #[test]
    fn two_writes_to_one_key_land_in_order() {
        let dir = TempDir::new("ordering");
        let store = FileKvStore::new(dir.0.clone());

        store.store("ui", "first").unwrap();
        store.store("ui", "second").unwrap();
        flush_writes();

        assert_eq!(
            store.load("ui").as_deref(),
            Some("second"),
            "the earlier write won, so the two were applied out of order"
        );
    }

    /// The suspend path: the process may be killed the instant it returns.
    ///
    /// Read straight off the filesystem with no flush, because a `load` that
    /// happened to be served after a later drain would pass without proving
    /// anything about when the write happened.
    #[test]
    fn store_now_has_written_before_it_returns() {
        let dir = TempDir::new("storenow");
        let store = FileKvStore::new(dir.0.clone());

        store.store_now("ui", "durable").expect("store_now");

        assert_eq!(
            std::fs::read_to_string(dir.0.join("ui.json"))
                .ok()
                .as_deref(),
            Some("durable"),
            "store_now returned before the bytes were on disk"
        );
    }

    /// `store_now` must not overtake what is already queued for the same key.
    ///
    /// Writing straight to the filesystem instead of through the queue passes
    /// the first assertion and fails the second: `late` goes down immediately,
    /// and then the queued `early` lands on top of it. That is a stale config
    /// written by the one call whose whole job is to be current, so the check
    /// after the drain is the one that catches it.
    #[test]
    fn store_now_does_not_overtake_a_queued_write() {
        let dir = TempDir::new("overtake");
        let store = FileKvStore::new(dir.0.clone());

        store.store("ui", "early").unwrap();
        store.store_now("ui", "late").expect("store_now");

        assert_eq!(
            std::fs::read_to_string(dir.0.join("ui.json"))
                .ok()
                .as_deref(),
            Some("late"),
            "store_now's value is not on disk: it either returned before its \
             own write, or the queued write overtook it and clobbered it"
        );
        // Nothing may still be in flight for this key. An `early` that was
        // queued behind `late` rather than ahead of it lands right here.
        flush_writes();
        assert_eq!(
            std::fs::read_to_string(dir.0.join("ui.json"))
                .ok()
                .as_deref(),
            Some("late"),
            "a write queued before store_now landed after it, overwriting it"
        );
    }

    /// `Ok` from `store` means the queue accepted it, not that it worked.
    ///
    /// The cost of moving the write: the only thread that can see the failure
    /// is the one that ran it, so it is logged there and the caller is told
    /// nothing. `autosave_config` records the value as written on this `Ok`,
    /// which is why a failing disk waits for the next genuine config change
    /// rather than the next 3 s tick — documented at both ends, and asserted
    /// here so it cannot drift into looking like a real success.
    #[test]
    fn a_doomed_write_still_reports_ok_from_store() {
        let dir = TempDir::new("doomed");
        std::fs::create_dir_all(&dir.0).expect("temp dir");
        // A regular file standing where the config directory should be, so
        // `create_dir_all` cannot succeed and the write is lost before it
        // starts.
        let occupied = dir.0.join("occupied");
        std::fs::write(&occupied, "not a directory").expect("occupy the path");

        let store = FileKvStore::new(occupied);
        assert_eq!(
            store.store("ui", "doomed"),
            Ok(()),
            "store reports the queue accepting the write, not its outcome"
        );
        flush_writes();
        assert_eq!(
            store.load("ui"),
            None,
            "the write could not have succeeded, so nothing may read back"
        );
    }

    /// The same failure, told truthfully, when the caller waited for it.
    #[test]
    fn store_now_reports_a_write_that_could_not_happen() {
        let dir = TempDir::new("doomednow");
        std::fs::create_dir_all(&dir.0).expect("temp dir");
        let occupied = dir.0.join("occupied");
        std::fs::write(&occupied, "not a directory").expect("occupy the path");

        let store = FileKvStore::new(occupied);
        assert!(
            store.store_now("ui", "doomed").is_err(),
            "store_now waited for this write, so it must report the failure"
        );
    }

    /// A half-written config is worse than a stale one: the next launch loses
    /// the whole layout rather than the last few seconds of it. The temp file
    /// the atomic write goes through must not survive it, either.
    #[test]
    fn a_write_replaces_the_file_without_leaving_a_temp_behind() {
        let dir = TempDir::new("atomic");
        let store = FileKvStore::new(dir.0.clone());

        store.store_now("ui", "first").expect("store_now");
        store.store_now("ui", "second").expect("store_now");

        assert_eq!(store.load("ui").as_deref(), Some("second"));
        let strays: Vec<_> = std::fs::read_dir(&dir.0)
            .expect("config dir")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name())
            .filter(|name| name != "ui.json")
            .collect();
        assert!(
            strays.is_empty(),
            "the config dir should hold only ui.json, found {strays:?}"
        );
    }
}
