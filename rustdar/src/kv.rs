//! Filesystem-backed [`KvStore`], shared by desktop, Android and iOS; the web
//! build supplies a `localStorage` one and writes inline, `localStorage` being
//! synchronous by specification and main-thread-only.
//!
//! The write leaves the calling thread, which is always the frame thread:
//! neither `create_dir_all` nor `write` has a bound. Only the bytes move —
//! `autosave_config` compares the serialized string against the last one it
//! wrote, so serializing stays with the caller.
//!
//! One dedicated thread rather than the job pool, for **ordering**: two
//! `store`s to the same key must land in the order they were made. The
//! producers are all one thread because `PlatformBridge::kv` hands out a
//! `Box<dyn KvStore>`, neither `Send` nor `Sync`; widening that takes the
//! ordering guarantee with it. Delivery across process death is not
//! guaranteed — the queue is a `static OnceLock` whose destructor never runs
//! and `exit_now` reaches `process::exit` without joining it.

use rustdar_kv::KvStore;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::mpsc::{Sender, channel};

enum WriteRequest {
    Write {
        dir: PathBuf,
        path: PathBuf,
        value: String,
        /// Present when a caller is blocked on the outcome, i.e. `store_now`.
        done: Option<Sender<Result<(), String>>>,
    },
    /// Reply once every request queued earlier has been handled. Tests only.
    #[cfg(test)]
    Drain(Sender<()>),
    /// Park the writer until the paired sender sends or drops. Tests only:
    /// with the writer parked, a queued write *cannot* have run.
    #[cfg(test)]
    Block(std::sync::mpsc::Receiver<()>),
}

/// How long [`store_now`](KvStore::store_now) waits for the writer. Bounded,
/// because it waits for everything queued ahead of it too, inside an Android
/// lifecycle callback that runs against a watchdog.
const STORE_NOW_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// The one thread that touches config files, started on first use.
/// Process-wide, because `kv()` hands out a fresh `FileKvStore` on every call
/// and per-instance threads would put concurrent writers on one directory.
fn writer() -> Option<&'static Sender<WriteRequest>> {
    static WRITER: OnceLock<Option<Sender<WriteRequest>>> = OnceLock::new();
    WRITER
        .get_or_init(|| {
            let (tx, rx) = channel::<WriteRequest>();
            std::thread::Builder::new()
                .name("config-writer".to_owned())
                .spawn(move || {
                    // Nothing ends this loop in practice: `WRITER` is a
                    // static `OnceLock` whose destructor never runs.
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
                                    // `store_now`: the caller owns the outcome.
                                    Some(done) => {
                                        let _ = done.send(result);
                                    }
                                    // Queued: logged here or nowhere.
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
                            // `Err` when the test dropped its sender, so a
                            // failed test unparks rather than wedging later.
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

/// Put `value` at `path`, creating `dir` first. Into a sibling temp file and
/// then renamed, never straight onto `path`: `fs::write` opens `O_TRUNC`, and
/// `exit_now` calls `process::exit` without joining this thread, so a death in
/// between would leave a zero-byte `ui.json`. The temp name carries the pid.
fn write_blob(dir: &Path, path: &Path, value: &str) -> Result<(), String> {
    // On write, not up front: an unwritten store leaves no empty directory.
    if let Err(e) = std::fs::create_dir_all(dir) {
        return Err(format!("failed to create config dir {:?}: {}", dir, e));
    }
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&tmp, value).map_err(|e| format!("failed to write {:?}: {}", tmp, e))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        // The rename is what publishes the value; a failed one would leave an
        // orphan temp file per failure.
        let _ = std::fs::remove_file(&tmp);
        format!("failed to replace {:?}: {}", path, e)
    })
}

/// Wait until every request queued before this call has been handled — a
/// sentinel through the same queue, never a sleep. Process-wide, so it cannot
/// assert that another test's write has not happened.
#[cfg(test)]
fn flush_writes() {
    let Some(writer) = writer() else {
        return;
    };
    let (reply, done) = channel();
    if writer.send(WriteRequest::Drain(reply)).is_ok() {
        let _ = done.recv();
    }
}

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

    /// Queue the write and return. `Ok` means the request was accepted, not
    /// that anything is on disk — a write that then fails is logged by the
    /// writer thread, so `autosave_config` records the value as written and a
    /// failing disk is retried only when the config next changes.
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
            // The thread died after it started; `WRITER` cannot notice, a
            // `OnceLock` being written once.
            return write_blob(&self.dir, &path, value);
        }
        Ok(())
    }

    /// Write, and wait for it — through the same queue rather than straight to
    /// the filesystem, because a direct write could overtake a queued one for
    /// the same key. It therefore drains everything ahead of it too, and is
    /// bounded by [`STORE_NOW_TIMEOUT`] so a stalled filesystem does not trade
    /// a dropped frame for a watchdog kill.
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
            return write_blob(&self.dir, &path, value);
        }
        match outcome.recv_timeout(STORE_NOW_TIMEOUT) {
            Ok(result) => result,
            // The writer is stuck inside a write, on the slow storage this
            // module exists for. Deliberately *not* retried inline: the queued
            // write is still live and a second writer could race it.
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(format!(
                "config write for {:?} did not finish within {:?}; it is still queued",
                path, STORE_NOW_TIMEOUT
            )),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                write_blob(&self.dir, &path, value)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// The only test here that fails against a synchronous `store`.
    #[test]
    fn store_returns_before_the_bytes_land() {
        let dir = TempDir::new("deferred");
        let store = FileKvStore::new(dir.0.clone());
        let writer = writer().expect("this test is about the writer thread");

        // Parked before anything is queued; dropping `release` unparks it.
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

    /// Two writes to one key, last one wins — only true if they are applied in
    /// the order they were made.
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

    /// The suspend path: the process may be killed the instant it returns. Read
    /// straight off the filesystem, because a `load` served after a later drain
    /// would prove nothing about when the write happened.
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
    /// Writing straight to the filesystem fails the second assertion.
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
        // Nothing may still be in flight for this key: an `early` queued behind
        // `late` lands right here.
        flush_writes();
        assert_eq!(
            std::fs::read_to_string(dir.0.join("ui.json"))
                .ok()
                .as_deref(),
            Some("late"),
            "a write queued before store_now landed after it, overwriting it"
        );
    }

    /// `Ok` from `store` means the queue accepted it, not that it worked: only
    /// the thread that ran the write can see the failure.
    #[test]
    fn a_doomed_write_still_reports_ok_from_store() {
        let dir = TempDir::new("doomed");
        std::fs::create_dir_all(&dir.0).expect("temp dir");
        // A regular file where the config directory should be, so
        // `create_dir_all` cannot succeed.
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

    /// A half-written config is worse than a stale one, and the temp file must
    /// not survive it either.
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
