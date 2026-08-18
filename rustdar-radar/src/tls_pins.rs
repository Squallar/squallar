//! The fresh-process pins tying this crate's network entry points to
//! `tls::init`.
//!
//! `tls` itself moved down to `rustdar-source` (reached here through the
//! `crate::tls` re-export), but these six tests stayed: three of them call
//! `crate::scan`, `crate::archive` and `crate::chunks`, and moving them down
//! with the module would have handed the substrate a dependency on this crate
//! — the exact reverse edge the move exists to rule out. Each `#[ignore]`d
//! probe below asserts on process-global provider state, which the first test
//! to touch it in a shared binary would poison, so its spawner re-executes
//! this binary and runs it `--exact`; a spawned probe must therefore live in
//! the **same test binary** as its spawner, which is why the pairs live
//! together in one radar file rather than splitting along the helper line.
//!
//! `poll_once` and `run_probe` are copies of the helpers in
//! `rustdar-source/src/tls/tests.rs`, where the tls-only pair
//! (`client_installs_provider_in_a_fresh_process` and its probe) still runs.

/// Run an `async fn`'s synchronous prologue without completing it. Merely
/// *creating* the future runs nothing, so a test that did that would pass
/// with the prologue deleted.
///
/// The poll is expected to panic — hyper's resolver needs a tokio reactor
/// this crate does not depend on — which happens after the `tls::init()`
/// being probed for. No request is issued.
fn poll_once<F: Future>(fut: F) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut fut = Box::pin(fut);
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        let _ = fut.as_mut().poll(&mut cx);
    }));
    std::panic::set_hook(previous);
}

/// The probes below assert on process-global state, which the first test to
/// touch it in a shared binary would poison. Each runs in a freshly spawned
/// copy of this binary.
fn run_probe(name: &str) {
    let exe = std::env::current_exe().expect("current_exe");
    let out = std::process::Command::new(&exe)
        .args([
            "--exact",
            name,
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("RUSTDAR_TLS_PROBE", "1")
        .output()
        .expect("failed to re-exec test binary");
    assert!(
        out.status.success(),
        "probe {name} failed in a fresh process\n--- stdout ---\n{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
fn nexrad_wrappers_install_provider_in_a_fresh_process() {
    run_probe("tls_pins::probe_list_files_installs_ring");
}

#[test]
fn archive_installs_provider_in_a_fresh_process() {
    run_probe("tls_pins::probe_archive_list_files_installs_ring");
}

#[test]
fn chunk_polling_installs_provider_in_a_fresh_process() {
    run_probe("tls_pins::probe_chunk_poll_installs_ring");
}

/// The client the `nexrad-data` wrapper reaches is built inside a `Lazy`, so
/// the provider must be installed before the first `.await`. Fails if
/// `tls::init()` is removed from `scan::list_files`.
#[test]
#[ignore = "spawned by nexrad_wrappers_install_provider_in_a_fresh_process"]
fn probe_list_files_installs_ring() {
    assert!(
        !crate::tls::default_is_ring(),
        "a provider was already installed before the probe ran; \
             this probe is only meaningful in a fresh process"
    );
    let date = chrono::NaiveDate::from_ymd_opt(2024, 1, 1).unwrap();
    // One poll: stops at the first `.await`, so the probe stays offline.
    poll_once(crate::scan::list_files("KTLX", &date));
    assert!(
        crate::tls::default_is_ring(),
        "scan::list_files did not install ring before its first await"
    );
}

/// The probe that pins `crate::archive` to [`crate::tls::client`].
/// `probe_list_files_installs_ring` — `#[ignore]`d, it needs the network —
/// goes through `scan::list_files`, which
/// calls `init()` itself, so it would still pass with `archive` on a bare
/// `reqwest::Client::builder()` — which panics "No provider set" for anyone
/// reaching `archive` directly. Fails if `archive::shared_client` stops
/// going through [`crate::tls::client`].
#[test]
#[ignore = "spawned by archive_installs_provider_in_a_fresh_process"]
fn probe_archive_list_files_installs_ring() {
    assert!(
        !crate::tls::default_is_ring(),
        "a provider was already installed before the probe ran; \
             this probe is only meaningful in a fresh process"
    );
    let date = chrono::NaiveDate::from_ymd_opt(2024, 1, 1).unwrap();
    let sources = crate::sources::DataSources::production();
    poll_once(crate::archive::list_files(&sources, "KTLX", &date));
    assert!(
        crate::tls::default_is_ring(),
        "archive::list_files did not install ring before its first await"
    );
}

/// The same pin for the real-time chunk feed. `ChunkPoller::poll` reaches
/// `archive::shared_client()` in its synchronous prologue for exactly this
/// reason; move that call after the first `.await` and merely polling the
/// future stops installing a provider, which is a panic for anyone who
/// reaches the poller without going through `scan::poll_chunks`.
#[test]
#[ignore = "spawned by chunk_polling_installs_provider_in_a_fresh_process"]
fn probe_chunk_poll_installs_ring() {
    assert!(
        !crate::tls::default_is_ring(),
        "a provider was already installed before the probe ran; \
             this probe is only meaningful in a fresh process"
    );
    let sources = crate::sources::DataSources::production();
    let mut poller = crate::chunks::ChunkPoller::new("KTLX");
    poll_once(poller.poll(&sources));
    assert!(
        crate::tls::default_is_ring(),
        "ChunkPoller::poll did not install ring before its first await"
    );
}
