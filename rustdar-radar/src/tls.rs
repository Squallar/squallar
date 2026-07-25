//! Process-wide TLS setup.
//!
//! rustdar's TLS trust decisions belong to the operating system. `reqwest` is
//! pinned workspace-wide to `rustls-no-provider`, which is
//! `rustls-platform-verifier` with **no** crypto provider compiled in and **no**
//! bundled root store: at handshake time the platform verifier asks Android's
//! `TrustManager`, Windows' CryptoAPI, Apple's `SecTrustEvaluateWithError` or the
//! system cert directories on other unixes. Nothing in the binary carries an
//! expiry date, and OS distrust lists, enterprise CAs and user CAs all apply.
//!
//! Because no provider is compiled in, one has to be installed at runtime, and
//! that is what [`init`] does. Two details make this easy to get wrong:
//!
//! * `reqwest::ClientBuilder::build` reads `CryptoProvider::get_default()` and,
//!   with `rustls-no-provider`, `panic!("No provider set")` if it is absent. The
//!   panic lands wherever the *first* client happens to be built -- which for
//!   [`crate::archive`] is inside a `OnceLock`, i.e. on the first S3 request,
//!   long after startup. Any test that does not open a socket passes.
//! * Installing late is just as bad as not installing: whoever builds the first
//!   client wins, and every client built afterwards silently keeps that provider.
//!
//! Rather than rely on entry points remembering to call [`init`] early enough,
//! every path in this workspace that can reach a client construction calls it
//! first: [`client`] is the only constructor the application uses, including
//! from [`crate::archive`], and [`crate::scan`] additionally calls [`init`] at
//! the top of its two network wrappers. Entry points call it too, which is
//! belt-and-braces rather than the load-bearing guarantee.
//!
//! Until the archive moved off `nexrad-data`'s `aws` feature, this was less
//! load-bearing than it looked: that feature pinned `reqwest/rustls`, which
//! compiled `aws-lc-rs` in, and a client built without [`init`] silently used
//! *that* instead of panicking. With `aws-lc-sys` out of the graph the panic is
//! real, which is why two `rustdar-overlays` tests that built their own client
//! had to start calling [`init`].

/// The User-Agent every rustdar client sends.
///
/// `api.weather.gov` rejects requests without a contact-bearing User-Agent, so
/// this is not merely cosmetic.
pub const USER_AGENT: &str = "rustdar/1.0 (https://github.com/USA-RedDragon/rustdar)";

/// Install *ring* as the process-wide default rustls [`CryptoProvider`].
///
/// Idempotent and safe to call from any thread at any time. Call it before
/// constructing anything that might open a TLS connection.
///
/// No-op on wasm32, where there is no rustls to configure: reqwest routes
/// through the browser's `fetch()`.
///
/// [`CryptoProvider`]: rustls::crypto::CryptoProvider
#[cfg(not(target_arch = "wasm32"))]
pub fn init() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        // `install_default` returns `Err` only if a provider is already
        // installed. That is not a failure we can act on -- the first installer
        // wins by design -- and `default_is_ring` is what actually asserts we
        // got the provider we wanted.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

#[cfg(target_arch = "wasm32")]
pub fn init() {}

/// Build the shared HTTP client, with the crypto provider guaranteed installed.
///
/// This is the only `reqwest::Client` constructor the application uses. Routing
/// every client through one function is what makes the [`init`] call impossible
/// to forget: there is no second place to forget it in.
///
/// `https_only` is set here rather than at each call site. Every rustdar endpoint
/// is HTTPS, and on Android it is load-bearing: it removes cleartext as a
/// downgrade target for the plain-HTTP CRL and OCSP URLs embedded in the
/// certificates we talk to (see `rustdar-android/android/network_security_config.xml`).
#[cfg(not(target_arch = "wasm32"))]
pub fn client(user_agent: &str, timeout: std::time::Duration) -> reqwest::ClientBuilder {
    init();
    reqwest::Client::builder()
        .user_agent(user_agent.to_owned())
        .timeout(timeout)
        .https_only(true)
}

/// wasm32 build of [`client`].
///
/// reqwest's wasm `ClientBuilder` has neither `timeout` nor `https_only`: the
/// request is a `fetch()` call, so the browser owns the timeout, the trust store
/// and the mixed-content policy that `https_only` stands in for elsewhere. The
/// `timeout` argument is accepted and ignored to keep one signature across targets.
///
/// # Why the user agent is dropped too
///
/// A page cannot set `User-Agent`, and trying to is actively harmful. It is a
/// forbidden header name: Chromium strips it silently, so the request stays a
/// *simple* CORS request and succeeds. Firefox instead lets it through, which
/// makes the request non-simple and forces a preflight `OPTIONS` — and a plain
/// tile CDN does not answer preflights, so every basemap tile fails with
/// "Access-Control-Allow-Origin missing" and the map renders on a blank
/// background. One browser working and the other not, from one line, with no
/// error on the working one.
///
/// So the argument is accepted and ignored here, exactly as `timeout` is. The
/// browser sends its own `User-Agent`, which is the only thing it will ever
/// send, and the tile provider's terms are satisfied by the `Referer` the
/// browser attaches instead.
#[cfg(target_arch = "wasm32")]
pub fn client(_user_agent: &str, _timeout: std::time::Duration) -> reqwest::ClientBuilder {
    init();
    reqwest::Client::builder()
}

/// Build a client that sends **no** `User-Agent`.
///
/// For origins whose CORS preflight fails. In a browser, any non-safelisted
/// request header — and `User-Agent` is one — upgrades the request from
/// "simple" to preflighted, so the browser sends `OPTIONS` first and refuses to
/// issue the real request unless that answers 2xx with the method and header
/// allowed.
///
/// Two of rustdar's origins are like this. `mesonet.agron.iastate.edu` answers
/// `405 Method Not Allowed` to `OPTIONS`, and `www.spc.noaa.gov` answers `403`
/// with no CORS headers at all — both while answering the plain `GET` with
/// `Access-Control-Allow-Origin: *`. So those feeds are reachable from the web
/// build *only* as simple requests, and adding a `User-Agent` for politeness
/// would break them — silently, and only on wasm, where nothing else in the
/// workspace would notice.
///
/// See [`crate::sources::DataSources`], which is where that rule is recorded
/// per origin, and prefer [`client_for`] over calling this directly so the
/// choice stays driven by the recorded rule.
///
/// Everything else [`client`] configures still applies.
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
/// rule.
///
/// This is the *only* place the choice is made, so no call site has to
/// remember which origins tolerate a `User-Agent`. The boolean comes from
/// [`crate::sources::DataSources`] — see
/// [`metar_client`](crate::sources::DataSources::metar_client) and
/// [`spc_client`](crate::sources::DataSources::spc_client), which are how
/// production reaches this.
///
/// # This is not self-evidently load-bearing on wasm today
///
/// The wasm builds of [`client`] and [`simple_client`] are byte-identical: a
/// page cannot set `User-Agent`, so the wasm `client` drops it too (see
/// [`client`]). Picking the wrong one therefore breaks nothing *right now*.
/// It is still the rule, because that identity is a property of one `#[cfg]`
/// arm and not of the CORS problem this exists to avoid — restore a
/// `User-Agent` to the wasm client, on any target where a header can be set,
/// and the METAR and SPC layers go dark in the browser with no error on
/// native. [`sends_user_agent`] is what pins it.
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
/// `reqwest::Client` exposes no getter for its default headers, so this reads
/// them out of the `Debug` representation, which prints `default_headers`
/// unconditionally:
///
/// ```text
/// Client { .., default_headers: {"accept": "*/*", "user-agent": "rustdar/1.0 (..)"}, .. }
/// Client { .., default_headers: {"accept": "*/*"}, .. }
/// ```
///
/// Observed rather than derived, and for the same reason
/// `client_rejects_cleartext_urls` observes `https_only` behaviourally: the
/// configuration is write-only, and asserting on a value the test itself
/// supplied would prove nothing. A request cannot be used to observe it —
/// both constructors set `https_only(true)`, so a loopback `http://` server
/// is rejected before any header is written.
///
/// Always `false` on wasm32, which is correct: a page cannot set the header
/// and the browser supplies its own.
pub fn sends_user_agent(client: &reqwest::Client) -> bool {
    format!("{client:?}").contains("\"user-agent\"")
}

/// Whether the installed default provider is *ring*.
///
/// Compares the address of the provider's `secure_random`, which is a distinct
/// `&'static` per backend, so this distinguishes *ring* from `aws-lc-rs` even
/// though both expose the same cipher suites. Returns `false` when no provider
/// is installed at all.
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
    /// Poll a future exactly once and throw away the result.
    ///
    /// Used to run the synchronous prologue of an `async fn` -- everything up to
    /// the point where it needs the outside world -- without completing it. An
    /// `async fn` body does not execute until polled, so a test that merely
    /// *creates* the future would pass even if the prologue were deleted.
    ///
    /// The poll is expected to panic: `crate::archive` builds its client and
    /// starts DNS resolution inside the first poll, and hyper's resolver needs a
    /// tokio reactor this crate has no dependency on. That panic happens *after*
    /// the `tls::init()` we are testing for, so it is caught and discarded rather
    /// than worked around -- the side effect is what the probe asserts on, and
    /// no request is ever issued.
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

    /// `init` installs *ring*, not `aws-lc-rs`.
    ///
    /// `aws-lc-rs` is no longer in the graph, so this can no longer fail by
    /// picking up the other backend. It still names the backend rather than
    /// asserting "a provider is installed", because that is what would catch
    /// `aws-lc-rs` coming *back* -- any dependency re-enabling `reqwest/rustls`
    /// would restore both the crate and the silent fallback that used to make
    /// this assertion necessary.
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

    // NOTE: there is deliberately no test here that `default_is_ring` "is a real
    // comparison rather than a constant". The obvious one -- assert that ring's
    // `secure_random` differs from some unrelated static -- asserts a property of
    // its own fixture and never calls `default_is_ring` at all, so no change to
    // the function can make it fail. What actually pins the function down is the
    // pair of subprocess probes below: each asserts `!default_is_ring()` on entry
    // and `default_is_ring()` after the production path has run, so both a
    // constant `true` and a constant `false` fail one of them.

    /// Poll a future to completion *only if* it never yields.
    ///
    /// Returns `None` if the future needed to wait on anything, which for a
    /// `reqwest` request means it got as far as opening a socket. Used to prove
    /// that a rejection happened before any IO.
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
    /// reqwest signals that rejection as a `Builder`-kind error returned before
    /// any IO (`error::url_bad_scheme`), which is distinguishable from every
    /// transport error a real request could produce. Polling past the check
    /// reaches hyper's connector, which panics without a tokio reactor -- this
    /// crate has no tokio dependency -- so that panic is caught and reported as
    /// "not rejected", which is exactly what it means here.
    ///
    /// The `is_builder()` narrowing is defensive and, in this setup, not
    /// falsifiable: with no reactor, the *only* way a request returns `Ready(Err)`
    /// is the scheme check, so relaxing it to "any error" changes no outcome. It
    /// is kept because it documents which rejection is being detected, not
    /// because a test would catch its removal.
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

    /// A client from [`super::client`] refuses cleartext URLs outright.
    ///
    /// This is the `https_only(true)` guarantee, observed behaviourally because
    /// `reqwest::ClientBuilder` exposes no getter for it. The rejection happens
    /// before any socket is opened, which is what makes this test offline-safe.
    ///
    /// Fails if `.https_only(true)` is removed from [`super::client`].
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

    /// The same client does *not* reject `https://`, so the test above is
    /// detecting the scheme check rather than a client that rejects everything.
    ///
    /// Without this, `client()` returning a permanently broken builder -- or
    /// `rejected_by_scheme_check` always returning `true` -- would still satisfy
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
    // These three run as a set. `sends_user_agent` reads a `Debug` string, so
    // on its own it could be a constant in either direction; the pair below
    // pins it from both sides, and `client_for` is then asserted to route to
    // the right constructor. Without the first two, a `sends_user_agent` that
    // always returned `false` would satisfy every "must not send a UA"
    // assertion in the workspace.

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

    /// [`super::simple_client`] does not.
    ///
    /// This is the whole reason that constructor exists: IEM answers `OPTIONS`
    /// with `405` and SPC with `403`, so a `User-Agent` turns the `GET` into a
    /// preflight the browser never gets past.
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

    /// [`super::client_for`] routes to those two, and reads its argument.
    ///
    /// Fails if the branch is inverted or collapsed to one constructor — which
    /// is the mutation that would silently re-break the METAR and SPC layers on
    /// web while leaving native untouched.
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
    /// exact shape that rustls-platform-verifier issue #221 trips over --
    /// Let's Encrypt R13, no OCSP responder, and a cleartext `http://` CRL
    /// distribution point. If the OS trust path is wrong, or no crypto provider
    /// got installed, or the verifier rejects the chain, this fails; nothing
    /// here is stubbed.
    ///
    /// Run with:
    ///   `cargo test -p rustdar-radar --lib -- --ignored --nocapture live_`
    #[ignore = "hits the live api.weather.gov endpoint"]
    #[tokio::test]
    async fn live_https_fetch_against_weather_gov() {
        let client = super::client(super::USER_AGENT, std::time::Duration::from_secs(30))
            .build()
            .expect("client should build");

        // Pins down *which* provider carried the handshake. Now that aws-lc-rs
        // is out of the graph this cannot silently be the other backend, but it
        // is the assertion that would notice if it came back.
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

    /// The subprocess probes below assert on process-global state, which the
    /// first test to touch it in a shared test binary would poison. Each runs in
    /// a freshly spawned copy of this binary so the assertion is not shadowed by
    /// whatever else `cargo test` happened to run first.
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

    /// Building the app's client must install the provider by itself.
    ///
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

    /// Entering the `nexrad-data` wrapper must install the provider before the
    /// first `.await`, because the client it reaches is built inside a `Lazy`.
    ///
    /// Fails if the `tls::init()` call is removed from `scan::list_files`.
    #[test]
    #[ignore = "spawned by nexrad_wrappers_install_provider_in_a_fresh_process"]
    fn probe_list_files_installs_ring() {
        assert!(
            !super::default_is_ring(),
            "a provider was already installed before the probe ran; \
             this probe is only meaningful in a fresh process"
        );
        let date = chrono::NaiveDate::from_ymd_opt(2024, 1, 1).unwrap();
        // Polled once: runs the wrapper up to its first `.await` and stops, so
        // no request is issued and the probe stays offline.
        poll_once(crate::scan::list_files("KTLX", &date));
        assert!(
            super::default_is_ring(),
            "scan::list_files did not install ring before its first await"
        );
    }

    /// The archive module must install the provider on its own, without the
    /// `crate::scan` wrapper's belt-and-braces `tls::init()` in front of it.
    ///
    /// This is the probe that actually pins `crate::archive` to [`super::client`].
    /// `probe_list_files_installs_ring` goes through `scan::list_files`, which
    /// calls `init()` itself, so it would still pass if `archive` built its
    /// client with a bare `reqwest::Client::builder()` -- and that client would
    /// then panic with "No provider set" for anyone who reached `archive`
    /// directly.
    ///
    /// Fails if `archive::shared_client` stops going through [`super::client`].
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
