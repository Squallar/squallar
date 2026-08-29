//! Where the zone pack's bytes come from, which is the only thing about it
//! that differs between targets.
//!
//! The pack is **an asset, never a compiled-in constant.** `include_bytes!`
//! was the other candidate and it loses on both targets that would use it:
//!
//! - **Web.** The deployed module is ~13 MB raw / ~5.2 MB gzipped, and `sw.js`
//!   refetches the whole shell whenever the deploy's validator token changes —
//!   which is every push to main. Embedding would add the pack's gzipped
//!   megabytes to *every deploy's* download, forever, and `worker.js`
//!   instantiates the same module a second time, so the data segment would be
//!   resident twice. A separate same-origin asset carries its own `ETag`, is
//!   downloaded once, and survives redeploys of the module.
//! - **Native.** A pack is a snapshot of a published edition that the NWS
//!   supersedes on its own schedule; an embedded one can only be replaced by
//!   shipping a new binary. A file beside the zone cache can be replaced by
//!   re-running the converter — or, since the native entry points name the
//!   deployed copy via [`use_download_url`], by deleting it: the next
//!   connected session downloads the pack the web build serves and keeps it
//!   at the same path.
//!
//! And a third reason that applies to both: `include_bytes!` of a path that is
//! not in the tree does not compile. Making the app's build depend on a
//! multi-megabyte binary artifact — one that has to be regenerated from ~100 MB
//! of shapefiles whenever the NWS republishes — is a build that breaks for
//! everyone the moment the artifact is stale or absent. Absent is a *supported
//! state* here: [`installed`](super::zone_pack::installed) answers `None`, zone
//! resolution takes the HTTP path it took before the pack existed, and the only
//! thing lost is the request saving.
//!
//! **Why not the service worker's `SHELL_PATHS`**: that list is precached with
//! `cache.addAll`, which is all-or-nothing, so one asset failing to fetch takes
//! *offline support for the whole app* down with it — and `pwa_assets.rs` pins
//! the list in both directions. The pack is routed and cached by the worker on
//! its own, outside that list, so a pack that will not fetch costs requests and
//! nothing else.
//!
//! **Why not `LocalStorageKvStore`**: it is `String`-valued against a 5–10 MB
//! origin quota, and IndexedDB is unreachable from here — `web-sys` is pinned
//! to a feature set with no `Idb*` and `KvStore` is synchronous.

use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};

use super::zone_pack::{self, PACK_FILE_NAME, PackError};

/// Where a pack can be read from. One enum rather than a `cfg` inside the
/// loader, so the *policy* — try once, log what happened, never block a round —
/// has one body on every target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackSource {
    /// A file on disk. Native, iOS and Android.
    File(PathBuf),
    /// A same-origin static asset. Web, where there is no filesystem.
    Url(String),
}

/// Why a load did not produce a pack. Every variant means the same thing to the
/// round that asked — resolve over HTTP — but the log says which.
#[derive(Debug, Clone)]
pub enum LoadError {
    /// The file or the request did not produce bytes. Routine, and the reason
    /// this is logged at INFO: on a machine that has never run the converter
    /// there is simply no pack, and that is not a fault.
    Unavailable(String),
    Http(u16),
    /// The bytes arrived and [`zone_pack::ZonePack::open`] refused them.
    Rejected(PackError),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(why) => f.write_str(why),
            Self::Http(status) => write!(f, "HTTP {status}"),
            Self::Rejected(why) => write!(f, "{why}"),
        }
    }
}

/// The source a host has named, if any. Web sets one at boot because only the
/// browser knows the origin the app was served from.
static CONFIGURED: RwLock<Option<PackSource>> = RwLock::new(None);

/// Where a native host downloads the pack when the file beside its zone cache
/// is absent or refused. Named by the entry point, like [`use_source`],
/// because the deploy that serves the pack is the host's to know, not this
/// crate's. Web names no download URL: its source already *is* the download.
static DOWNLOAD_URL: RwLock<Option<String>> = RwLock::new(None);

/// Whether a load has been attempted this session.
///
/// One attempt, not one per round: a 404 does not become a 200 between two
/// polls two minutes apart, and retrying would spend a request every round to
/// re-learn the same thing — on the very layer whose request count is the
/// defect being repaired.
static ATTEMPTED: AtomicBool = AtomicBool::new(false);

/// Name the source this host wants. Called once, before the first alerts round.
pub fn use_source(source: PackSource) {
    if let Ok(mut slot) = CONFIGURED.write() {
        *slot = Some(source);
    }
}

/// Name the URL to download the pack from when the file beside the zone cache
/// does not produce one. Called once at boot by the native entry points.
///
/// What the download produces is installed for this session *and kept at the
/// file path*, so the next session — connected or not — reads the file and
/// spends no request. Failure changes nothing: absence stays a supported
/// state, and the round degrades to resolving zones over HTTP exactly as it
/// did before the pack existed.
pub fn use_download_url(url: String) {
    if let Ok(mut slot) = DOWNLOAD_URL.write() {
        *slot = Some(url);
    }
}

fn download_url() -> Option<String> {
    DOWNLOAD_URL.read().ok().and_then(|slot| slot.clone())
}

/// Resolve `file` against the URL of a page, the way a relative `<link href>`
/// would: everything up to and including the last `/`, with any query and
/// fragment discarded first.
///
/// A string operation rather than a `web_sys` call so it is testable on a host
/// where there is no document at all — and so the subpath deploy (`/squallar/`)
/// and the root deploy (`/`) go through the same one line of code.
pub fn asset_url(page_url: &str, file: &str) -> Option<String> {
    let without_fragment = page_url.split('#').next()?;
    let without_query = without_fragment.split('?').next()?;
    // Past `https://`, so the `//` of the scheme is never mistaken for the
    // directory separator of a page served from the origin's root.
    let scheme_end = without_query.find("://")? + 3;
    let (origin, path) = without_query.split_at(scheme_end);
    if origin.len() == without_query.len() {
        return None;
    }
    let directory = match path.rfind('/') {
        Some(at) => &path[..=at],
        // `https://host` with no path at all is the origin's root.
        None => return Some(format!("{origin}{path}/{file}")),
    };
    Some(format!("{origin}{directory}{file}"))
}

/// The pack that sits beside a zone cache directory: `.../squallar/zones.pack`
/// for a cache at `.../squallar/zones`.
///
/// Beside rather than inside, because the directory's contents are one file per
/// zone and the pack is not one of those.
pub fn pack_beside_cache(cache_dir: &Path) -> PathBuf {
    cache_dir.parent().unwrap_or(cache_dir).join(PACK_FILE_NAME)
}

/// The source to try: whatever a host named, else the file beside the zone
/// cache if this platform has one.
fn source_for(cache_dir: Option<&Path>) -> Option<PackSource> {
    if let Some(configured) = CONFIGURED.read().ok().and_then(|slot| slot.clone()) {
        return Some(configured);
    }
    cache_dir.map(|dir| PackSource::File(pack_beside_cache(dir)))
}

#[cfg(not(target_arch = "wasm32"))]
async fn read_file(path: &Path) -> Result<Vec<u8>, LoadError> {
    tokio::fs::read(path)
        .await
        .map_err(|e| LoadError::Unavailable(format!("{}: {e}", path.display())))
}

// Web has no filesystem. The same signature rather than a `cfg` at the call
// site, so the loader has one body on every target.
#[cfg(target_arch = "wasm32")]
async fn read_file(path: &Path) -> Result<Vec<u8>, LoadError> {
    Err(LoadError::Unavailable(format!(
        "{}: no filesystem on this target",
        path.display(),
    )))
}

async fn read_url(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, LoadError> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| LoadError::Unavailable(format!("{url}: {e}")))?;
    if !response.status().is_success() {
        return Err(LoadError::Http(response.status().as_u16()));
    }
    response
        .bytes()
        .await
        .map(|bytes| bytes.to_vec())
        .map_err(|e| LoadError::Unavailable(format!("{url}: {e}")))
}

/// Load `source` and install what it produced, without touching the
/// once-per-session gate. Separate from [`ensure_installed`] so a test can load
/// a pack twice.
pub async fn load(client: &reqwest::Client, source: &PackSource) -> Result<usize, LoadError> {
    let bytes = match source {
        PackSource::File(path) => read_file(path).await?,
        PackSource::Url(url) => read_url(client, url).await?,
    };
    let pack = zone_pack::install(bytes).map_err(LoadError::Rejected)?;
    Ok(pack.zone_count())
}

/// Fetch the pack from `url`, install it, and keep the bytes at `path` so the
/// next session reads the file with no request.
///
/// Install first, persist second: the file only ever holds bytes that
/// [`zone_pack::ZonePack::open`] accepted, so a rubbish response cannot leave
/// a pack on disk for every later session to refuse.
async fn download_to_file(
    client: &reqwest::Client,
    url: &str,
    path: &Path,
) -> Result<usize, LoadError> {
    let bytes = read_url(client, url).await?;
    let pack = zone_pack::install(bytes.clone()).map_err(LoadError::Rejected)?;
    keep_at(path, &bytes).await;
    Ok(pack.zone_count())
}

/// Best-effort: the session already holds the pack, so a failed write costs
/// only next session's re-download, and is logged rather than surfaced.
#[cfg(not(target_arch = "wasm32"))]
async fn keep_at(path: &Path, bytes: &[u8]) {
    let write = async {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        // Temp-then-rename: the rename is what publishes the file, so a
        // session killed mid-write leaves no truncated pack for the next boot
        // to refuse.
        let temp = path.with_extension("pack.part");
        tokio::fs::write(&temp, bytes).await?;
        tokio::fs::rename(&temp, path).await
    };
    match write.await {
        Ok(()) => log::info!("Kept the NWS zone pack at {}", path.display()),
        Err(e) => log::info!(
            "Could not keep the NWS zone pack at {}: {e}; this session has it, the next downloads again",
            path.display(),
        ),
    }
}

// Web never reaches this -- its source is a URL, not a file -- but the loader
// has one body on every target, so the signature exists on both.
#[cfg(target_arch = "wasm32")]
async fn keep_at(path: &Path, _bytes: &[u8]) {
    log::info!(
        "{}: no filesystem to keep the pack on this target",
        path.display()
    );
}

/// The body of [`ensure_installed`], without its once-per-session gate:
/// resolve the source, load it, and fall back to a named download when a
/// *file* source did not produce a pack. Separate so a test can drive the real
/// source resolution and the download path repeatedly, with the gate untouched.
async fn install_from(
    client: &reqwest::Client,
    cache_dir: Option<&Path>,
    download: Option<&str>,
) -> Result<usize, LoadError> {
    let Some(source) = source_for(cache_dir) else {
        log::info!("No NWS zone pack source on this target; zones resolve over HTTP");
        return Err(LoadError::Unavailable(
            "no pack source on this target".to_string(),
        ));
    };
    match load(client, &source).await {
        Ok(zones) => {
            log::info!("Installed the NWS zone pack: {zones} zones from {source:?}");
            Ok(zones)
        }
        Err(why) => match (&source, download) {
            // The file is absent or was refused; the deploy's copy is the
            // second chance, and it writes through to the same path. Only for
            // a file source: a host whose source is already a URL has named
            // the only download it has.
            (PackSource::File(path), Some(url)) => {
                match download_to_file(client, url, path).await {
                    Ok(zones) => {
                        log::info!(
                            "Installed the NWS zone pack: {zones} zones downloaded from {url} ({source:?}: {why})",
                        );
                        Ok(zones)
                    }
                    Err(dl) => {
                        log::info!(
                            "No NWS zone pack ({source:?}: {why}; {url}: {dl}); zones resolve over HTTP",
                        );
                        Err(dl)
                    }
                }
            }
            // INFO, not WARN: on a machine that has never run the converter
            // and named no download there is no pack, and a notice the reader
            // cannot act on is noise. What the reader *can* act on -- the
            // request count -- is already logged by the round itself.
            _ => {
                log::info!("No NWS zone pack ({source:?}: {why}); zones resolve over HTTP");
                Err(why)
            }
        },
    }
}

/// Install the pack if there is one to install, once per session.
///
/// Never fails a round: the answer to every failure is the behaviour that
/// shipped before the pack existed. Called from the alerts fetch, so it happens
/// on the fetch task rather than anywhere near a frame.
pub async fn ensure_installed(client: &reqwest::Client, cache_dir: Option<&Path>) {
    if zone_pack::installed().is_some() || ATTEMPTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let _ = install_from(client, cache_dir, download_url().as_deref()).await;
}

#[cfg(test)]
mod tests;
