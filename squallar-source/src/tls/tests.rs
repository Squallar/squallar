/// Names the backend rather than asserting "a provider is installed", so that a
/// dependency re-enabling `reqwest/rustls` — which brings back `aws-lc-rs` —
/// fails here.
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
/// reqwest signals it as a `Builder`-kind error returned before any IO. Polling
/// past the check reaches hyper's connector, which panics without a tokio
/// reactor; that panic is caught and reported as "not rejected".
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
/// behaviourally, and offline-safe: the rejection precedes any socket.
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

/// Without this, a permanently broken builder would satisfy
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

// ── The preflight rule ──
//
// These three run as a set: `sends_user_agent` scrapes a `Debug` string and
// could be a constant either way, so the first two pin it from both sides.

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

/// [`super::simple_client`] does not: IEM answers `OPTIONS` with `405` and SPC
/// with `403`, so a `User-Agent` turns the `GET` into a preflight.
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
/// Scraped rather than called because the wasm32 arms are not compiled by any
/// build this workspace tests. Pinning the property properly needs a
/// `wasm-bindgen-test` runner, which would make `wasm-pack` a prerequisite of
/// `cargo test` for the whole workspace.
fn cfg_gated_source(cfg: &str, signature: &str) -> String {
    let source = include_str!("../tls.rs");
    // The shipped half only. Cut on the `#[cfg]`, which survives the test body
    // moving to its own file.
    let (code, _) = source
        .split_once("\n#[cfg(all(test")
        .expect("tls.rs no longer has a test module");
    let needle = format!("{cfg}\n{signature}");
    // Exactly one definition, checked before it is read.
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

/// Every `.method(` called in a block of source, sorted and deduplicated.
/// `::` paths are skipped, so `reqwest::Client::builder()` does not appear.
#[test]
fn the_browser_clients_attach_no_user_agent() {
    const WASM: &str = r#"#[cfg(target_arch = "wasm32")]"#;
    const NATIVE: &str = r#"#[cfg(not(target_arch = "wasm32"))]"#;

    for (cfg, signature, permitted) in [
        (
            WASM,
            "pub fn client(_user_agent: &str, _timeout: std::time::Duration) \
                 -> reqwest::ClientBuilder {",
            // Nothing at all: the browser owns the timeout, the scheme and every
            // forbidden header.
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
            // `to_owned` is the argument to `user_agent`, not a builder call.
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
/// `api.weather.gov` is deliberately the endpoint: its certificate is the shape
/// rustls-platform-verifier issue #221 trips over — Let's Encrypt R13, no OCSP
/// responder, cleartext `http://` CRL distribution point.
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

/// The probes below assert on process-global state, so each runs in a freshly
/// spawned copy of this binary.
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
fn client_installs_provider_in_a_fresh_process() {
    run_probe("tls::tests::probe_client_installs_ring");
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
