//! Process-wide TLS setup.
//!
//! **rustdar does not own the trust decision, and must not start owning it.**
//! `reqwest` is pinned workspace-wide to `rustls-no-provider` —
//! `rustls-platform-verifier` with no crypto provider compiled in — so the *OS*
//! evaluates the chain at handshake time: Android `TrustManager`, Windows
//! CryptoAPI, Apple `SecTrustEvaluateWithError`, system cert dirs elsewhere.
//!
//! Two different alternatives get re-proposed, and they are wrong for two
//! different reasons. Do not accept either.
//!
//! * `rustls-native-certs` / reqwest's `rustls-tls-native-roots` bundle nothing,
//!   but they collect certificates at startup and hand rustls a flat root list,
//!   so **rustls** does the verifying and the platform verifier is out of the
//!   loop. That is the objection: on Android `rustls-platform-verifier` goes
//!   through JNI to `CertificateVerifier.verifyCertificateChain`, i.e. the
//!   system TrustManager, which is what applies Network Security Config, user
//!   and enterprise CAs and distrust lists — and its arm that would call
//!   `load_native_certs()` is `not(target_os = "android")`. `rustls-native-certs`
//!   does not read Android's trust store in any case: it defers to
//!   `openssl-probe`, whose Android search path is the Termux bundle.
//! * `webpki-roots` is the actual compiled-in snapshot, and that one gives the
//!   binary an expiration date.
//!
//! Because no provider is compiled in, one has to be installed at runtime —
//! [`init`]. Two ways to get that wrong:
//!
//! * `reqwest::ClientBuilder::build` reads `CryptoProvider::get_default()` and
//!   `panic!("No provider set")` if it is absent. The panic lands wherever the
//!   *first* client is built, which for [`crate::archive`] is inside a `OnceLock`
//!   on the first S3 request. Any test that does not open a socket passes.
//! * Installing late is as bad as not installing: the first installer wins and
//!   every later client silently keeps that provider.
//!
//! So every path that can reach a client construction calls it first. [`client`]
//! is the only constructor the application uses, and [`crate::scan`] also calls
//! [`init`] at the top of its two network wrappers.
//!
//! This only became load-bearing when the archive moved off `nexrad-data`'s
//! `aws` feature: that feature compiled `aws-lc-rs` in, so a client built without
//! [`init`] silently used it instead of panicking.

/// `api.weather.gov` rejects requests without a contact-bearing User-Agent.
pub const USER_AGENT: &str = "rustdar/1.0 (https://github.com/USA-RedDragon/rustdar)";

/// Install *ring* as the process-wide default rustls [`CryptoProvider`].
///
/// Idempotent and thread-safe. No-op on wasm32: reqwest routes through the
/// browser's `fetch()`.
///
/// [`CryptoProvider`]: rustls::crypto::CryptoProvider
#[cfg(not(target_arch = "wasm32"))]
pub fn init() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        // `Err` only means a provider is already installed; the first installer
        // wins by design and `default_is_ring` asserts which one it was.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

#[cfg(target_arch = "wasm32")]
pub fn init() {}

/// Build the shared HTTP client, with the crypto provider guaranteed installed.
/// The only `reqwest::Client` constructor the application uses.
///
/// `https_only` is set here, not per call site: on Android it removes cleartext
/// as a downgrade target for the plain-HTTP CRL and OCSP URLs embedded in the
/// certificates we talk to (see
/// `rustdar-android/android/network_security_config.xml`).
#[cfg(not(target_arch = "wasm32"))]
pub fn client(user_agent: &str, timeout: std::time::Duration) -> reqwest::ClientBuilder {
    init();
    reqwest::Client::builder()
        .user_agent(user_agent.to_owned())
        .timeout(timeout)
        .https_only(true)
}

/// wasm32 build of [`client`]. Both arguments are accepted and ignored.
///
/// reqwest's wasm `ClientBuilder` has neither `timeout` nor `https_only`; the
/// browser owns both.
///
/// `User-Agent` is dropped because setting it is actively harmful: it is a
/// forbidden header name, Chromium strips it silently (request stays simple, so
/// it works), Firefox forwards it, which makes the request non-simple and forces
/// a preflight `OPTIONS` that a plain tile CDN does not answer — every basemap
/// tile then fails with "Access-Control-Allow-Origin missing" in one browser and
/// not the other. The tile provider's terms are satisfied by the `Referer` the
/// browser attaches.
#[cfg(target_arch = "wasm32")]
pub fn client(_user_agent: &str, _timeout: std::time::Duration) -> reqwest::ClientBuilder {
    init();
    reqwest::Client::builder()
}

/// Build a client that sends **no** `User-Agent`, for origins whose CORS
/// preflight fails.
///
/// `mesonet.agron.iastate.edu` answers `OPTIONS` with `405` and
/// `www.spc.noaa.gov` with `403` and no CORS headers — both while answering the
/// plain `GET` with `Access-Control-Allow-Origin: *`. Any non-safelisted request
/// header (`User-Agent` is one) makes the request non-simple and triggers that
/// preflight, so those feeds break silently, on wasm only.
///
/// Prefer [`client_for`]; the per-origin rule lives in
/// [`crate::sources::DataSources`].
#[cfg(not(target_arch = "wasm32"))]
pub fn simple_client(timeout: std::time::Duration) -> reqwest::ClientBuilder {
    init();
    reqwest::Client::builder().timeout(timeout).https_only(true)
}

/// wasm32 build of [`simple_client`]. See [`client`] for why the timeout goes.
#[cfg(target_arch = "wasm32")]
pub fn simple_client(_timeout: std::time::Duration) -> reqwest::ClientBuilder {
    init();
    reqwest::Client::builder()
}

/// Pick between [`client`] and [`simple_client`] from an origin's preflight
/// rule. The only place that choice is made; the boolean comes from
/// [`crate::sources::DataSources`].
///
/// The wasm builds of the two are currently byte-identical — a page cannot set
/// `User-Agent` either way — so picking wrong breaks nothing *today*. That is a
/// property of one `#[cfg]` arm, not of the CORS problem: restore a `User-Agent`
/// to the wasm client and METAR and SPC go dark in the browser with no error on
/// native. [`sends_user_agent`] pins it.
pub fn client_for(
    sends_user_agent: bool,
    timeout: std::time::Duration,
) -> reqwest::ClientBuilder {
    if sends_user_agent {
        client(USER_AGENT, timeout)
    } else {
        simple_client(timeout)
    }
}

/// Whether this client attaches a `User-Agent` to every request it issues.
///
/// `reqwest::Client` exposes no getter for its default headers, so this scrapes
/// the `Debug` representation, which prints `default_headers` unconditionally.
/// A request cannot be used instead: both constructors set `https_only(true)`,
/// so a loopback `http://` server is rejected before any header is written.
///
/// Always `false` on wasm32, correctly — the browser supplies its own.
pub fn sends_user_agent(client: &reqwest::Client) -> bool {
    format!("{client:?}").contains("\"user-agent\"")
}

/// Compares the address of the provider's `secure_random`, a distinct
/// `&'static` per backend, so *ring* and `aws-lc-rs` are told apart despite
/// exposing the same cipher suites. `false` when no provider is installed.
#[cfg(not(target_arch = "wasm32"))]
pub fn default_is_ring() -> bool {
    let Some(installed) = rustls::crypto::CryptoProvider::get_default() else {
        return false;
    };
    let ring = rustls::crypto::ring::default_provider();
    std::ptr::eq(
        installed.secure_random as *const _ as *const (),
        ring.secure_random as *const _ as *const (),
    )
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
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
            .args(["--exact", name, "--ignored", "--nocapture", "--test-threads=1"])
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
    /// `probe_list_files_installs_ring` goes through `scan::list_files`, which
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
        poll_once(crate::archive::list_files("KTLX", &date));
        assert!(
            super::default_is_ring(),
            "archive::list_files did not install ring before its first await"
        );
    }
}
