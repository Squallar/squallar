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
//! * `rustls-native-certs` (and reqwest's pre-0.13 `rustls-tls-native-roots`,
//!   which the 0.13.4 pin no longer has) bundles nothing, but it collects
//!   certificates at startup and hands rustls a flat root list, so **rustls**
//!   does the verifying and the platform verifier is out of the loop. That is
//!   the objection: on Android `rustls-platform-verifier` goes through JNI to
//!   `CertificateVerifier.verifyCertificateChain`, i.e. the system TrustManager,
//!   which is what applies Network Security Config, user and enterprise CAs and
//!   distrust lists — and its arm that would call `load_native_certs()` is
//!   `not(target_os = "android")`.
//!
//!   On Android it is not even a subtly different answer. `rustls-native-certs`
//!   defers to `openssl-probe`, whose only Android locations are the Termux
//!   bundle and an `/etc/ssl/certs` fallback; neither is Android's trust store,
//!   so it returns whatever happens to sit at those two paths rather than the
//!   platform's roots.
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
/// native.
///
/// [`sends_user_agent`] pins the routing here, but only over the arm the test
/// runner compiled — always the native one. The wasm arms are held to the
/// no-`User-Agent` property by `the_browser_clients_attach_no_user_agent`,
/// which reads them as source because nothing in this workspace executes them.
pub fn client_for(sends_user_agent: bool, timeout: std::time::Duration) -> reqwest::ClientBuilder {
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
mod tests;
