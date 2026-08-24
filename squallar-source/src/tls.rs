//! Process-wide TLS setup.
//!
//! **squallar does not own the trust decision.** `reqwest` is pinned
//! workspace-wide to `rustls-no-provider` — `rustls-platform-verifier` with no
//! crypto provider compiled in — so the *OS* evaluates the chain at handshake
//! time.
//!
//! Do not swap it for `rustls-native-certs`: it hands rustls a flat root list,
//! so on Android the system TrustManager — which applies Network Security
//! Config, user and enterprise CAs and distrust lists — is bypassed.
//!
//! Because no provider is compiled in, one has to be installed at runtime —
//! [`init`] — before the first client is built. Every path that can reach a
//! client construction calls it first.

/// `api.weather.gov` rejects requests without a contact-bearing User-Agent.
pub const USER_AGENT: &str = "squallar/1.0 (https://github.com/USA-RedDragon/rustdar)";

/// Install *ring* as the process-wide default rustls [`CryptoProvider`].
///
/// Idempotent and thread-safe. No-op on wasm32.
///
/// [`CryptoProvider`]: rustls::crypto::CryptoProvider
#[cfg(not(target_arch = "wasm32"))]
pub fn init() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        // `Err` only means a provider is already installed; the first installer wins.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

#[cfg(target_arch = "wasm32")]
pub fn init() {}

/// Build the shared HTTP client, with the crypto provider guaranteed installed.
/// The only `reqwest::Client` constructor the application uses.
///
/// `https_only` is set here, not per call site: on Android it removes cleartext
/// as a downgrade target for the plain-HTTP CRL and OCSP URLs in the
/// certificates we talk to.
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
/// forbidden header name, Chromium strips it silently while Firefox forwards
/// it, which makes the request non-simple and forces a preflight `OPTIONS` a
/// plain tile CDN does not answer.
#[cfg(target_arch = "wasm32")]
pub fn client(_user_agent: &str, _timeout: std::time::Duration) -> reqwest::ClientBuilder {
    init();
    reqwest::Client::builder()
}

/// Build a client that sends **no** `User-Agent`, for origins whose CORS
/// preflight fails.
///
/// `mesonet.agron.iastate.edu` answers `OPTIONS` with `405` and
/// `www.spc.noaa.gov` with `403` and no CORS headers, both while answering the
/// plain `GET` with `ACAO: *`. Prefer [`client_for`]; the per-origin rule lives
/// in [`crate::origins::DataSources`].
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
/// rule. The wasm builds of the two are currently byte-identical — a page cannot
/// set `User-Agent` either way — so picking wrong breaks nothing *today*, and
/// [`sends_user_agent`] only ever pins the native arm.
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
/// the `Debug` representation. Always `false` on wasm32, correctly.
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
