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

/// Names the backend rather than asserting "a provider is installed", so
/// that a dependency re-enabling `reqwest/rustls` — which brings back
/// `aws-lc-rs` and the silent fallback — fails here.
#[test]
fn init_installs_ring() {
    super::init();
    assert!(
        super::default_is_ring(),
        "default provider is not ring; something installed another backend first"
    );
}

#[test]
fn init_is_idempotent() {
    super::init();
    super::init();
    assert!(super::default_is_ring());
}

// No test that `default_is_ring` is a real comparison: the subprocess probes
// below already pin it from both sides (`!default_is_ring()` on entry,
// `default_is_ring()` after), so a constant in either direction fails one.

/// Poll to completion *only if* the future never yields. `None` means it
/// waited on something, which for reqwest means it opened a socket.
fn poll_ready<F: Future>(fut: F) -> Option<F::Output> {
    let mut fut = Box::pin(fut);
    let waker = std::task::Waker::noop();
    let mut cx = std::task::Context::from_waker(waker);
    match fut.as_mut().poll(&mut cx) {
        std::task::Poll::Ready(v) => Some(v),
        std::task::Poll::Pending => None,
    }
}

/// Did this request get rejected by the `https_only` scheme check?
///
/// reqwest signals it as a `Builder`-kind error returned before any IO.
/// Polling past the check reaches hyper's connector, which panics without a
/// tokio reactor; that panic is caught and reported as "not rejected".
///
/// `is_builder()` is not falsifiable here — with no reactor, the scheme check
/// is the only way to get `Ready(Err)` — but it records which rejection is
/// meant.
fn rejected_by_scheme_check(client: &reqwest::Client, url: &str) -> bool {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        poll_ready(client.get(url).send())
    }));
    std::panic::set_hook(previous);

    match outcome {
        Ok(Some(Err(e))) => e.is_builder(),
        // Completed some other way, or reached the connector and panicked.
        _ => false,
    }
}

/// Fails if `.https_only(true)` is removed from [`super::client`]. Observed
/// behaviourally because `ClientBuilder` exposes no getter, and offline-safe
/// because the rejection happens before any socket is opened.
#[test]
fn client_rejects_cleartext_urls() {
    let client = super::client("probe", std::time::Duration::from_secs(1))
        .build()
        .expect("client should build");
    assert!(
        rejected_by_scheme_check(&client, "http://api.weather.gov/"),
        "an http:// request was not rejected; https_only is not set"
    );
}

/// Without this, a permanently broken builder — or a
/// `rejected_by_scheme_check` that always returned `true` — would satisfy
/// `client_rejects_cleartext_urls`.
#[test]
fn client_accepts_https_urls() {
    let client = super::client("probe", std::time::Duration::from_secs(1))
        .build()
        .expect("client should build");
    assert!(
        !rejected_by_scheme_check(&client, "https://api.weather.gov/"),
        "an https:// request was rejected by the scheme check; it is \
             rejecting more than cleartext"
    );
}

// ── The preflight rule ────────────────────────────────────────────────
//
// These three run as a set. `sends_user_agent` scrapes a `Debug` string and
// could be a constant either way, so the first two pin it from both sides
// before the third asserts `client_for` routes correctly.

/// [`super::client`] does attach the User-Agent it is given.
#[test]
fn the_ordinary_client_sends_a_user_agent() {
    let c = super::client(super::USER_AGENT, std::time::Duration::from_secs(1))
        .build()
        .expect("client should build");
    assert!(
        super::sends_user_agent(&c),
        "client() built something with no User-Agent; api.weather.gov \
             rejects requests without one",
    );
}

/// [`super::simple_client`] does not: IEM answers `OPTIONS` with `405` and
/// SPC with `403`, so a `User-Agent` turns the `GET` into a preflight the
/// browser never gets past.
#[test]
fn the_simple_client_sends_no_user_agent() {
    let c = super::simple_client(std::time::Duration::from_secs(1))
        .build()
        .expect("client should build");
    assert!(
        !super::sends_user_agent(&c),
        "simple_client() attached a User-Agent, which makes every request \
             to a preflight-hostile origin fail in the browser",
    );
}

/// Fails if the branch is inverted or collapsed to one constructor, the
/// mutation that silently re-breaks METAR and SPC on web only.
#[test]
fn client_for_routes_on_the_preflight_rule() {
    let t = std::time::Duration::from_secs(1);
    let permitted = super::client_for(true, t).build().expect("client");
    let forbidden = super::client_for(false, t).build().expect("client");
    assert!(
        super::sends_user_agent(&permitted),
        "client_for(true) must give the User-Agent-bearing client",
    );
    assert!(
        !super::sends_user_agent(&forbidden),
        "client_for(false) must give the preflight-safe client",
    );
}

/// The source of one `#[cfg]`-gated function in this file, body included.
///
/// Scraped rather than called because the wasm32 arms are not compiled by
/// any build this workspace tests: `cargo test` runs on the host, and the
/// wasm rows of the gauntlet are `cargo check`, which compiles the arms but
/// runs nothing. Adding a `.user_agent(…)` to either browser constructor
/// therefore compiles, checks and tests clean while silently killing METAR,
/// SPC and every basemap tile in Firefox — see
/// [`the_browser_clients_attach_no_user_agent`].
///
/// Byte-scraping is the weaker tool and it is deliberate here: pinning the
/// property properly needs a `wasm-bindgen-test` runner, which would make
/// `wasm-pack` a prerequisite of `cargo test` for the whole workspace. That
/// is a decision to take on purpose, not a side effect of this test.
fn cfg_gated_source(cfg: &str, signature: &str) -> String {
    let source = include_str!("../tls.rs");
    // The shipped half only. The signatures below are line-continued string
    // literals, so today they do not appear in this module's own source —
    // but that is an accident of formatting, not a property, and `cargo fmt`
    // could end it without a word.
    //
    // Cut on the `#[cfg]` rather than the `mod tests {` it guards: the
    // attribute is what survives the test body moving to its own file,
    // where the `mod tests {` line does not. `cfg(all(test` appears exactly
    // once in this file, so the cut lands in the same place either way.
    let (code, _) = source
        .split_once("\n#[cfg(all(test")
        .expect("tls.rs no longer has a test module");
    let needle = format!("{cfg}\n{signature}");
    // Exactly one definition, checked before it is read: two would mean the
    // scrape is reading whichever came first, and a decoy in a doc comment
    // or a string would be one.
    let occurrences = code.matches(&needle).count();
    assert_eq!(
        occurrences, 1,
        "expected exactly one\n{needle}\nin tls.rs, found {occurrences}"
    );
    let at = code.find(&needle).expect("just counted one");
    let source = code;
    let open = at + source[at..].find('{').expect("no function body");
    let mut depth = 0usize;
    for (offset, c) in source[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return source[open..=open + offset].to_string();
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced braces after {signature}");
}

/// Every `.method(` called in a block of source, sorted and deduplicated.
///
/// `::` paths are not method calls and are skipped, so
/// `reqwest::Client::builder()` does not appear.
fn method_calls(body: &str) -> Vec<String> {
    let bytes: Vec<char> = body.chars().collect();
    let mut found: Vec<String> = Vec::new();
    for (i, &c) in bytes.iter().enumerate() {
        if c != '.' || i == 0 {
            continue;
        }
        // `::name(` is a path, `1.0` is a float, `..base` is a struct update.
        if matches!(bytes[i - 1], ':' | '.' | '0'..='9') {
            continue;
        }
        let name: String = bytes[i + 1..]
            .iter()
            .take_while(|c| c.is_alphanumeric() || **c == '_')
            .collect();
        if !name.is_empty() && bytes.get(i + 1 + name.chars().count()) == Some(&'(') {
            found.push(name);
        }
    }
    found.sort();
    found.dedup();
    found
}

/// **Exactly which builder calls each of the four constructors makes.**
///
/// `User-Agent` is a forbidden header name in a browser. Chromium strips it
/// silently, so the request stays simple and works; Firefox forwards it,
/// which makes the request non-simple and forces a preflight `OPTIONS` that
/// `mesonet.agron.iastate.edu` answers with `405`, `www.spc.noaa.gov` with
/// `403`, and a plain tile CDN with no CORS headers at all. Every METAR,
/// every SPC outlook and every basemap tile then fails in one browser and
/// not the other, with nothing wrong on native.
///
/// [`the_simple_client_sends_no_user_agent`] above looks like it covers
/// this and does not: it builds a `reqwest::Client` and so tests the arm the
/// test runner was compiled for, which is always the native one. The wasm
/// arms have never been executed by anything.
///
/// # Why an allowlist and not "contains `.user_agent(`"
///
/// Because `.user_agent(` is not the only way to set the header, and the
/// other way was reachable. reqwest 0.13.4's **wasm** `ClientBuilder`
/// exposes `default_headers` (`src/wasm/client.rs:329`); a review set
/// `USER_AGENT` through a `HeaderMap` on the wasm client and the substring
/// check passed with `cargo check --target wasm32-unknown-unknown` at 0 —
/// the exact Firefox outage described above, shipped. Enumerating forbidden
/// spellings only moves the goalposts, so this enumerates the *permitted*
/// calls instead: every builder method is a `.method(`, so an exact set is
/// the whole configuration surface. Adding any call to any of the four
/// fails here until it is written down.
///
/// The native arms are checked the same way, and not as a courtesy: they
/// are the control that keeps this from passing vacuously. A scrape that
/// matched the wrong text, or nothing, would report an empty call set for
/// all four.
#[test]
fn the_browser_clients_attach_no_user_agent() {
    const WASM: &str = r#"#[cfg(target_arch = "wasm32")]"#;
    const NATIVE: &str = r#"#[cfg(not(target_arch = "wasm32"))]"#;

    for (cfg, signature, permitted) in [
        (
            WASM,
            "pub fn client(_user_agent: &str, _timeout: std::time::Duration) \
                 -> reqwest::ClientBuilder {",
            // Nothing at all: the browser owns the timeout, the scheme and
            // every forbidden header, so there is nothing left to configure.
            &[][..],
        ),
        (
            WASM,
            "pub fn simple_client(_timeout: std::time::Duration) -> reqwest::ClientBuilder {",
            &[][..],
        ),
        (
            NATIVE,
            "pub fn client(user_agent: &str, timeout: std::time::Duration) \
                 -> reqwest::ClientBuilder {",
            // `to_owned` is the argument to `user_agent`, not a builder
            // call; it is listed because this reads the whole body.
            &["https_only", "timeout", "to_owned", "user_agent"][..],
        ),
        (
            NATIVE,
            "pub fn simple_client(timeout: std::time::Duration) -> reqwest::ClientBuilder {",
            &["https_only", "timeout"][..],
        ),
    ] {
        let body = cfg_gated_source(cfg, signature);
        // The scrape found a real constructor and not, say, a doc comment.
        assert!(
            body.contains("reqwest::Client::builder()"),
            "{signature} no longer builds a client:\n{body}"
        );
        assert_eq!(
            method_calls(&body),
            permitted,
            "{cfg}\n{signature}\nconfigures something this list does not \
                 permit. On wasm the list is empty on purpose: `user_agent`, \
                 `default_headers` and anything else that attaches a header \
                 make the request non-simple, and the preflight that follows is \
                 refused by IEM, SPC and every tile CDN — in Firefox only, with \
                 nothing wrong on native.\nbody:\n{body}"
        );
    }
}

/// End-to-end proof that the platform verifier actually verifies.
///
/// `api.weather.gov` is deliberately the endpoint: its certificate is the
/// shape rustls-platform-verifier issue #221 trips over — Let's Encrypt R13,
/// no OCSP responder, cleartext `http://` CRL distribution point.
///
/// `cargo test -p rustdar-radar --lib -- --ignored --nocapture live_`
#[ignore = "hits the live api.weather.gov endpoint"]
#[tokio::test]
async fn live_https_fetch_against_weather_gov() {
    let client = super::client(super::USER_AGENT, std::time::Duration::from_secs(30))
        .build()
        .expect("client should build");

    // Pins *which* provider carried the handshake, so aws-lc-rs coming back
    // into the graph is noticed.
    assert!(
        super::default_is_ring(),
        "handshake would not be carried by ring"
    );

    let resp = client
        .get("https://api.weather.gov/")
        .send()
        .await
        .expect("TLS handshake / request to api.weather.gov failed");

    println!("status: {}", resp.status());
    assert!(resp.status().is_success(), "unexpected status");

    let body = resp.text().await.expect("body should read");
    println!("body bytes: {}", body.len());
    assert!(!body.is_empty(), "empty body");
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
fn client_installs_provider_in_a_fresh_process() {
    run_probe("tls::tests::probe_client_installs_ring");
}

#[test]
fn nexrad_wrappers_install_provider_in_a_fresh_process() {
    run_probe("tls::tests::probe_list_files_installs_ring");
}

#[test]
fn archive_installs_provider_in_a_fresh_process() {
    run_probe("tls::tests::probe_archive_list_files_installs_ring");
}

#[test]
fn chunk_polling_installs_provider_in_a_fresh_process() {
    run_probe("tls::tests::probe_chunk_poll_installs_ring");
}

/// Fails if the `init()` call is removed from [`super::client`].
#[test]
#[ignore = "spawned by client_installs_provider_in_a_fresh_process"]
fn probe_client_installs_ring() {
    assert!(
        !super::default_is_ring(),
        "a provider was already installed before the probe ran; \
             this probe is only meaningful in a fresh process"
    );
    let _builder = super::client("probe", std::time::Duration::from_secs(1));
    assert!(
        super::default_is_ring(),
        "tls::client() did not install ring"
    );
}

/// The client the `nexrad-data` wrapper reaches is built inside a `Lazy`, so
/// the provider must be installed before the first `.await`. Fails if
/// `tls::init()` is removed from `scan::list_files`.
#[test]
#[ignore = "spawned by nexrad_wrappers_install_provider_in_a_fresh_process"]
fn probe_list_files_installs_ring() {
    assert!(
        !super::default_is_ring(),
        "a provider was already installed before the probe ran; \
             this probe is only meaningful in a fresh process"
    );
    let date = chrono::NaiveDate::from_ymd_opt(2024, 1, 1).unwrap();
    // One poll: stops at the first `.await`, so the probe stays offline.
    poll_once(crate::scan::list_files("KTLX", &date));
    assert!(
        super::default_is_ring(),
        "scan::list_files did not install ring before its first await"
    );
}

/// The probe that pins `crate::archive` to [`super::client`].
/// `probe_list_files_installs_ring` — `#[ignore]`d, it needs the network —
/// goes through `scan::list_files`, which
/// calls `init()` itself, so it would still pass with `archive` on a bare
/// `reqwest::Client::builder()` — which panics "No provider set" for anyone
/// reaching `archive` directly. Fails if `archive::shared_client` stops
/// going through [`super::client`].
#[test]
#[ignore = "spawned by archive_installs_provider_in_a_fresh_process"]
fn probe_archive_list_files_installs_ring() {
    assert!(
        !super::default_is_ring(),
        "a provider was already installed before the probe ran; \
             this probe is only meaningful in a fresh process"
    );
    let date = chrono::NaiveDate::from_ymd_opt(2024, 1, 1).unwrap();
    let sources = crate::sources::DataSources::production();
    poll_once(crate::archive::list_files(&sources, "KTLX", &date));
    assert!(
        super::default_is_ring(),
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
        !super::default_is_ring(),
        "a provider was already installed before the probe ran; \
             this probe is only meaningful in a fresh process"
    );
    let sources = crate::sources::DataSources::production();
    let mut poller = crate::chunks::ChunkPoller::new("KTLX");
    poll_once(poller.poll(&sources));
    assert!(
        super::default_is_ring(),
        "ChunkPoller::poll did not install ring before its first await"
    );
}
