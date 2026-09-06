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
pub const USER_AGENT: &str = "squallar/1.0 (https://github.com/Squallar/squallar)";

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

/// Everything that decides which client a call site wants: the origin's
/// preflight rule and the timeout. Two call sites that agree on both want the
/// same client, and get it.
type ClientKey = (bool, std::time::Duration);

/// The clients [`shared_client_for`] has handed out, and the count of what it
/// had to construct to do it.
///
/// **A `reqwest::Client` is expensive to construct and was being constructed
/// per fetch round.** Every overlay round that reads IEM or SPC built its own
/// inside `SourceHandler::create_fetch_tasks`, which runs on the frame thread
/// in `handle_redraw`'s tail. Measured on scene D, NVIDIA RTX 3090 / Vulkan,
/// 2026-09-06: 3,659–3,973 µs for a round that built one against 2–11 µs for a
/// round on the application-wide client, and one METAR round per gesture loop
/// was the whole p99 of `frame post (handle)` — 4,000 µs at p99 over 112
/// interact frames, 91% of that family's sum carried by 2 of them.
///
/// A client is also a connection pool, so a round that reuses one keeps the
/// TLS session the round before it opened, rather than handshaking again.
///
/// Not a map: the key set is the handful of (rule, timeout) pairs this
/// application's origins declare, and a linear scan over four entries is
/// cheaper than hashing one.
struct ClientCache {
    entries: Vec<(ClientKey, reqwest::Client)>,
    builds: u64,
}

impl ClientCache {
    const fn new() -> Self {
        Self {
            entries: Vec::new(),
            builds: 0,
        }
    }

    fn get_or_build(&mut self, key: ClientKey) -> Result<reqwest::Client, reqwest::Error> {
        if let Some((_, client)) = self.entries.iter().find(|(held, _)| *held == key) {
            return Ok(client.clone());
        }
        let client = client_for(key.0, key.1).build()?;
        self.builds += 1;
        self.entries.push((key, client.clone()));
        Ok(client)
    }
}

static SHARED_CLIENTS: std::sync::Mutex<ClientCache> = std::sync::Mutex::new(ClientCache::new());

/// **The client for this origin rule and timeout**, built on the first ask and
/// shared with every ask after it. See `ClientCache` for what that is worth.
///
/// A poisoned lock is taken anyway: the value behind it is a cache, so the
/// worst a panicking builder can leave is an entry that was never pushed.
pub fn shared_client_for(
    sends_user_agent: bool,
    timeout: std::time::Duration,
) -> Result<reqwest::Client, reqwest::Error> {
    SHARED_CLIENTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get_or_build((sends_user_agent, timeout))
}

/// **How many `reqwest::Client`s this process has constructed** for
/// [`shared_client_for`], however many rounds asked for one. Never above the
/// number of distinct (rule, timeout) pairs asked for; that ceiling is the
/// whole of what the cache buys and is what
/// `one_client_is_built_per_distinct_origin_rule_and_timeout` holds.
pub fn clients_built() -> u64 {
    SHARED_CLIENTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .builds
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
