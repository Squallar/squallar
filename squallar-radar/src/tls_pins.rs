//! The fresh-process pins tying this crate's network entry points to
//! `tls::init`.

/// Run an `async fn`'s synchronous prologue without completing it.
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

/// The probes below assert on process-global state, which the first test to touch it in
/// a shared binary would poison.
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
        .env("SQUALLAR_TLS_PROBE", "1")
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

/// The client the `nexrad-data` wrapper reaches is built inside a `Lazy`, so the
/// provider must be installed before the first `.await`.
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

/// The same pin for the real-time chunk feed.
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
