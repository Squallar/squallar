/*
 * rustdar's service worker.
 *
 * ============================================================================
 * The one rule this file exists to enforce
 * ============================================================================
 *
 * rustdar is a live weather application. A cached radar sweep, alert polygon or
 * METAR that the UI presents as current is worse than no application at all:
 * someone opening this during severe weather and seeing a two-hour-old
 * reflectivity scan, with nothing on screen saying so, is the specific harm this
 * worker is written to make impossible.
 *
 * So the policy is not "cache carefully". It is:
 *
 *   * The **app shell** — index.html, the wasm-bindgen glue, the wasm module,
 *     the manifest and the icons — is cached. That is what makes the page load
 *     instantly and survive a flaky connection, and none of it carries a
 *     timestamp a user could misread.
 *
 *   * **Weather data is never cached, ever.** Not network-first, not
 *     stale-while-revalidate, not "cached with a staleness header". The worker
 *     does not call `respondWith()` for those requests at all, so the browser
 *     does its ordinary networking and this file is not even in the path. That
 *     is a stronger guarantee than `respondWith(fetch(req))`, because there is
 *     no code here that *could* write such a response to a cache.
 *
 *     Offline, those fetches fail. That is the honest answer, and index.html
 *     renders an explicit offline banner so the failure is visible rather than
 *     inferred from an empty map.
 *
 *   * **Basemap tiles are the one exception** and are cached aggressively. A
 *     slippy-map tile has no time dimension: `dark_nolabels/7/29/52.png` is the
 *     same picture of the same coastline today and next year, so a cached one
 *     cannot misrepresent current conditions the way a cached radar scan can.
 *     They are also the most expensive thing here to refetch. CartoDB itself
 *     says so — it serves them with `Cache-Control: public, max-age=15552000`
 *     (180 days), and `tileFetch` below honours exactly that number rather than
 *     inventing its own.
 *
 * Routing is **default-deny**: `routeFor()` returns `network` unless a request
 * positively matches the shell list or the basemap tile pattern. A new data
 * source added to `rustdar_radar::sources::DataSources` is therefore uncached
 * by default, with no change needed here. `NEVER_CACHE_HOSTS` is defence in
 * depth on top of that, checked first, and `tests/pwa_assets.rs` pins it
 * against `DataSources::production()` so the two cannot drift.
 *
 * ============================================================================
 * Serving from a subpath
 * ============================================================================
 *
 * The deployed page lives at `https://<user>.github.io/rustdar/`; development
 * happens at the root of `python3 -m http.server`. Nothing in this file is
 * written as an absolute path. Every URL is resolved against `ROOT`, which is
 * the directory this script was served from, so the same bytes work at
 * `/rustdar/` and at `/`.
 *
 * ============================================================================
 * How updates land
 * ============================================================================
 *
 * The classic failure of a precaching worker is pinning an old bundle forever:
 * the browser only reinstalls a worker whose *own bytes* changed, and this
 * file's bytes do not change when the 11 MB wasm module does. Hashing the
 * bundle into a constant here would fix that, but only with a build step that
 * rewrites this file — and the Pages deployment is not ours to change.
 *
 * So the shell version is discovered at runtime instead. `probeValidator()`
 * issues a `HEAD` for the wasm module and forms a token from the HTTP
 * validators the server already sends (`ETag` on GitHub Pages, `Last-Modified` +
 * `Content-Length` on `http.server`). One round trip, no body. The token names
 * the shell cache, so:
 *
 *   * token unchanged  -> the cached shell is current, nothing is transferred.
 *   * token changed    -> a new deploy. The whole shell is refetched into a
 *                         *new* cache, and only once that has completed is the
 *                         meta pointer moved. An interrupted update leaves the
 *                         previous complete shell in place; there is no window
 *                         in which half of one deploy is mixed with half of
 *                         another.
 *
 * This separates the two versions that people usually conflate:
 *
 *   * `SW_VERSION` below versions *this file's logic*, and is bumped by hand.
 *   * the validator token versions *the deployed content*, and moves by itself.
 *
 * ============================================================================
 * What happens to a client mid-session
 * ============================================================================
 *
 * Nothing is swapped underneath it. The running page keeps the wasm instance it
 * already booted — hot-swapping a weather app's code under the user is both
 * impossible (the module is instantiated) and undesirable. The worker finishes
 * the download in the background and posts `rustdar:shell-updated` to every
 * window, and index.html raises a small, dismissible "Reload" affordance. The
 * new version lands on the next navigation, at a moment the user chose.
 *
 * Navigations are served **cache-first**, which is deliberate and is the other
 * half of atomicity. A network-first navigation would hand a fresh index.html
 * to a client that then loads the *cached* glue and wasm — a wasm-bindgen
 * version mismatch and a blank page. The shell is consistent or it is not
 * served; it is never mixed.
 *
 * "Never mixed" is a claim about a *page load*, not about a moment in time, and
 * that distinction is load-bearing. A page load is not atomic: the navigation,
 * the glue and the 11 MB module are three separate fetches, seconds apart, and
 * a deploy can land in between. Resolving "the current shell" independently for
 * each of them is therefore not enough — it was in fact how this worker used to
 * hand a page index.html from one deploy and its wasm from the next, and the
 * symptom was the exact wasm-bindgen mismatch above.
 *
 * So the shell generation is pinned **per client**, at the navigation, in
 * `clientShells`. Every subresource that client goes on to request is served
 * from the generation its navigation was answered from, whatever the meta
 * pointer has done since. `purgeCaches` retains every pinned generation, plus
 * the one immediately superseded — the second is what covers a worker that is
 * killed and restarted mid-load, taking `clientShells` with it.
 */

"use strict";

/*
 * Bump when the logic in this file changes. It only names caches; the deployed
 * content's version is the validator token, discovered at runtime.
 */
const SW_VERSION = 2;

/*
 * The directory this worker was served from, with a trailing slash:
 * `https://user.github.io/rustdar/` deployed, `http://127.0.0.1:8000/` locally.
 *
 * `self.registration.scope` would usually say the same thing, but it is not
 * available while the module body is evaluating in every engine, and it can be
 * widened past the script's directory with a `Service-Worker-Allowed` header.
 * The script's own location is the thing the relative asset paths below are
 * actually relative to.
 */
const ROOT = new URL("./", self.location.href);

const META_CACHE = `rustdar-meta-v${SW_VERSION}`;
const TILE_CACHE = `rustdar-basemap-v${SW_VERSION}`;
const SHELL_PREFIX = `rustdar-shell-v${SW_VERSION}-`;

/* Key for the meta record. A synthetic URL: nothing is ever served from it. */
const META_KEY = new URL("__rustdar_sw_meta__", ROOT).href;

/*
 * The app shell, relative to ROOT.
 *
 * `""` is the directory index — `new URL("", ROOT)` is ROOT itself — and it is
 * the single entry every navigation is answered from, whether the user asked
 * for `/rustdar/`, `/rustdar/index.html` or `/rustdar/?station=KTLX`.
 * `index.html` is deliberately *not* listed separately: caching the same bytes
 * under two keys is how the two copies end up from different deploys.
 *
 * `pkg/` is what `wasm-pack build --target web` emits. The `.d.ts` files and
 * `package.json` it also writes are build-time artefacts and are not listed.
 */
const SHELL_PATHS = [
  "",
  "manifest.webmanifest",
  "pkg/rustdar_web.js",
  "pkg/rustdar_web_bg.wasm",
  "icons/icon-192.png",
  "icons/icon-512.png",
  "icons/icon-maskable-512.png",
  "icons/apple-touch-icon.png",
  "icons/favicon-32.png",
];

const SHELL_URLS = SHELL_PATHS.map((p) => new URL(p, ROOT).href);
const SHELL_URL_SET = new Set(SHELL_URLS);

/* The asset whose HTTP validators stand in for "which deploy is this". */
const SHELL_VERSION_PROBE = new URL("pkg/rustdar_web_bg.wasm", ROOT).href;

/*
 * Every origin `rustdar_radar::sources::DataSources::production()` reads from.
 *
 * Listing them is belt and braces: `routeFor()` is default-deny, so omitting a
 * host from this set would not cause it to be cached. The set exists so that
 * the policy is stated where a reader looks for it, and so that
 * `tests/pwa_assets.rs` can pin it against the Rust declaration and fail when a
 * new data source is added without anyone thinking about this file.
 *
 * The `.amazonaws.com` / `.noaa.gov` / `.weather.gov` suffix rules below cover
 * bucket and subdomain changes that do not reach this list.
 */
const NEVER_CACHE_HOSTS = new Set([
  // NEXRAD Level II archive volumes.
  "unidata-nexrad-level2.s3.amazonaws.com",
  // NEXRAD Level II real-time chunks.
  "unidata-nexrad-level2-chunks.s3.amazonaws.com",
  // NEXRAD Level III products.
  "unidata-nexrad-level3.s3.amazonaws.com",
  // HRRR model output.
  "noaa-hrrr-bdp-pds.s3.amazonaws.com",
  // GOES-East / GOES-West granules, for GLM lightning.
  "noaa-goes19.s3.amazonaws.com",
  "noaa-goes18.s3.amazonaws.com",
  // NWS public API: active alerts and zone geometry.
  "api.weather.gov",
  // Storm Prediction Center: outlooks, mesoscale discussions, storm reports.
  "www.spc.noaa.gov",
  // Iowa Environmental Mesonet: current ASOS/METAR observations.
  "mesonet.agron.iastate.edu",
]);

/* CartoDB's four tile subdomains, as `rustdar_egui::tiles::CartoDb` builds them. */
const BASEMAP_HOST = /^cartodb-basemaps-[a-d]\.global\.ssl\.fastly\.net$/;

/*
 * How many tiles to keep. At CartoDB's ~15-25 KB per 256px PNG this is roughly
 * 12-20 MB, which is a fraction of a browser's per-origin quota and about two
 * screenfuls of continental US at every zoom a user is likely to visit in a
 * session.
 */
const TILE_CACHE_MAX = 700;

/* Fall-back freshness for a tile whose response carries no `Cache-Control`. */
const TILE_DEFAULT_MAX_AGE_MS = 30 * 24 * 60 * 60 * 1000;

/* Don't re-probe for a new deploy more often than this. */
const UPDATE_PROBE_INTERVAL_MS = 60 * 1000;

// ---------------------------------------------------------------------------
// Routing
// ---------------------------------------------------------------------------

/**
 * Reduce a hostname to the form the rules below are written against.
 *
 * Two spellings of the same host reach here and neither is exotic:
 *
 *   * `api.weather.gov.` — the fully-qualified form, with the root label
 *     spelled out. `new URL()` accepts it and preserves the dot, DNS resolves
 *     it to the identical server, and `"api.weather.gov."` is not a member of
 *     `NEVER_CACHE_HOSTS` and does not end with `".weather.gov"`.
 *   * `API.Weather.GOV` — `new URL()` lowercases the host itself, so this only
 *     matters for a direct call, but hostnames are case-insensitive and the
 *     comparisons below are not.
 *
 * Neither was ever cacheable: `routeFor` is default-deny and both fall through
 * to `network`. They are normalised anyway because this function is the layer
 * that *states* the policy, and a future rule that leans on it must not inherit
 * a hole that only the default is currently covering.
 */
function normalizeHost(hostname) {
  let host = String(hostname).toLowerCase();
  while (host.endsWith(".")) host = host.slice(0, -1);
  return host;
}

function isWeatherDataHost(hostname) {
  const host = normalizeHost(hostname);
  if (NEVER_CACHE_HOSTS.has(host)) return true;
  // Suffix rules so a bucket rename or a new NWS subdomain is still never
  // cached without this file having to be updated first.
  return (
    host === "amazonaws.com" ||
    host.endsWith(".amazonaws.com") ||
    host === "noaa.gov" ||
    host.endsWith(".noaa.gov") ||
    host === "weather.gov" ||
    host.endsWith(".weather.gov")
  );
}

function isBasemapTile(url) {
  // The extension is matched case-insensitively because a URL path is
  // case-sensitive and `.PNG` is the same picture. The host is what actually
  // confines this rule, and it is an exact match against CartoDB's four
  // subdomains — the extension only distinguishes a tile from the other things
  // that host serves.
  return BASEMAP_HOST.test(normalizeHost(url.hostname)) && /\.png$/i.test(url.pathname);
}

function isShellAsset(url) {
  // Compare without the query string or fragment so a cache-busted
  // `?v=2` still resolves to the shell entry it names.
  return SHELL_URL_SET.has(url.origin + url.pathname);
}

/**
 * Classify one request. The whole caching policy is this function.
 *
 * Returns one of:
 *   "network"  - the worker must not touch it. No `respondWith`, no cache.
 *   "navigate" - a top-level navigation; answer from the cached shell index.
 *   "shell"    - a named app-shell asset.
 *   "tile"     - a CartoDB basemap tile.
 *
 * Takes a plain `{url, method, mode}` rather than a `Request` so it is callable
 * from a test harness that has no `Request` constructor.
 */
function routeFor({ url, method = "GET", mode = "no-cors" }) {
  // Only GET is ever cacheable, and rustdar issues nothing else.
  if (method !== "GET") return "network";

  let u;
  try {
    u = new URL(url, ROOT);
  } catch {
    return "network";
  }

  // `chrome-extension:`, `data:`, `blob:` and friends: `cache.put` rejects on
  // them anyway, and they are none of this worker's business.
  if (u.protocol !== "http:" && u.protocol !== "https:") return "network";

  // Credentials in the URL. Nothing rustdar issues has them, so their presence
  // means the request was not built by this application, and a cache entry keyed
  // by a URL carrying a password is a bad thing to own regardless of what the
  // host is.
  if (u.username || u.password) return "network";

  // Checked before anything else that could say "yes", so no later rule can
  // reach a data origin. This is not redundant with the default-deny below: it
  // is what keeps the two rules that *do* say yes — a same-origin navigation
  // and a same-origin shell asset — from matching a weather origin if rustdar
  // is ever served from one, behind a proxy or on a shared host.
  if (isWeatherDataHost(u.hostname)) return "network";

  if (isBasemapTile(u)) return "tile";

  // `ROOT.pathname` always ends in a slash, so this is a directory containment
  // test and not a prefix match: `/rustdar/` does not match `/rustdar-old/x`.
  // Without it, a user-site deploy at `https://<user>.github.io/` — which the
  // relative-URL work exists to support — would answer navigations for every
  // other project on that origin with rustdar's index.html. Service-worker
  // scope confines this today; scope is not the thing that should be relied on.
  if (u.origin === ROOT.origin && u.pathname.startsWith(ROOT.pathname)) {
    if (mode === "navigate") return "navigate";
    if (isShellAsset(u)) return "shell";
  }

  // Default deny.
  return "network";
}

// ---------------------------------------------------------------------------
// Shell versioning
// ---------------------------------------------------------------------------

/**
 * Form a version token from a response's HTTP validators.
 *
 * Both sides of every comparison come from a `HEAD` issued by
 * `probeValidator()`, never from a stored `GET`, so the token is not sensitive
 * to the header differences between the two (notably `Content-Length` under
 * content negotiation).
 *
 * `null` means the server publishes no validators at all. rustdar's two real
 * targets both do — GitHub Pages sends `ETag`, `http.server` sends
 * `Last-Modified` and `Content-Length` — but on a server that does not, updates
 * cannot be detected this way and the shell is left alone rather than
 * re-downloaded on every load.
 */
function validatorToken(response) {
  const etag = response.headers.get("etag");
  if (etag) return `etag:${etag}`;
  const lastModified = response.headers.get("last-modified");
  const length = response.headers.get("content-length");
  if (lastModified || length) return `lm:${lastModified || ""}|len:${length || ""}`;
  return null;
}

async function probeValidator() {
  // `no-store` so the browser's own HTTP cache cannot answer this and hide a
  // new deploy behind a `max-age` that has not expired.
  const response = await fetch(SHELL_VERSION_PROBE, {
    method: "HEAD",
    cache: "no-store",
  });
  if (!response.ok) throw new Error(`shell probe: HTTP ${response.status}`);
  return validatorToken(response);
}

function shellCacheName(token) {
  return SHELL_PREFIX + encodeURIComponent(token);
}

async function readMeta() {
  const cache = await caches.open(META_CACHE);
  const stored = await cache.match(META_KEY);
  if (!stored) return null;
  try {
    return await stored.json();
  } catch {
    return null;
  }
}

async function writeMeta(meta) {
  const cache = await caches.open(META_CACHE);
  await cache.put(
    META_KEY,
    new Response(JSON.stringify(meta), {
      headers: { "content-type": "application/json" },
    }),
  );
}

/*
 * The published meta record, memoised so the hot path is one cache lookup
 * rather than a meta read per subresource. Cleared whenever the pointer moves.
 */
let metaPromise = null;

function currentMeta() {
  if (!metaPromise) metaPromise = readMeta();
  return metaPromise;
}

/*
 * Which shell generation each live client is being served from.
 *
 * Keyed by the client id the navigation created. This is the whole of the
 * atomicity guarantee: a page load spans several fetches and a deploy can land
 * between any two of them, so "the current shell" has to mean "the shell this
 * client's navigation was answered from", not "the shell as of right now".
 *
 * Module state, so a worker killed for idleness loses it. That is why
 * `purgeCaches` also retains the generation it just superseded: a restart mid
 * page-load falls back to the previous shell rather than to a deleted one.
 */
const clientShells = new Map();

/**
 * Open a named shell cache, or null if it is not there any more.
 *
 * `caches.open` creates on demand, which is the wrong behaviour here: opening a
 * purged generation would silently manufacture an empty cache, every lookup in
 * it would miss, and the entry would sit in storage forever. Ask first.
 */
async function openShellCache(name) {
  if (!name) return null;
  if (!(await caches.has(name))) return null;
  return caches.open(name);
}

/** Drop pins for clients that have gone away, so the map cannot grow forever. */
async function forgetDepartedClients() {
  if (clientShells.size === 0) return;
  const live = new Set(
    (await self.clients.matchAll({ includeUncontrolled: true, type: "window" })).map((c) => c.id),
  );
  for (const id of [...clientShells.keys()]) if (!live.has(id)) clientShells.delete(id);
}

/** The shell generation `clientId` is pinned to, falling back to the current one. */
async function shellCacheForClient(clientId) {
  if (clientId) {
    const pinned = clientShells.get(clientId);
    if (pinned) {
      const cache = await openShellCache(pinned);
      // A pin whose cache is gone falls through rather than failing: the
      // network is always a correct answer, just a slower one.
      if (cache) return cache;
    }
  }
  const meta = await currentMeta();
  return openShellCache(meta && meta.cacheName);
}

/**
 * Download the whole shell into a cache named for `token`, then publish it.
 *
 * The publish is the `writeMeta` at the end, and it is the only step that makes
 * the new shell visible. `addAll` is all-or-nothing: it rejects without writing
 * anything if any request fails or answers non-2xx, so a partially downloaded
 * deploy can never be published.
 */
async function installShell(token, name = shellCacheName(token)) {
  const cache = await caches.open(name);
  /*
   * `no-cache`, not `reload`. Both bypass a stale `max-age` — GitHub Pages
   * serves `max-age=600`, so a plain fetch could rebuild the shell out of the
   * *previous* deploy's bytes and store them under the new token, which is the
   * pinned-bundle bug wearing a different hat.
   *
   * The difference is what happens when nothing changed. `reload` downloads
   * unconditionally; `no-cache` revalidates, so an unchanged asset costs a 304
   * and no body. Measured on a first visit against a server sending `ETag` and
   * `max-age=600`, which is what Pages sends:
   *
   *   index.html, the glue, the manifest and the icons  ->  304, no body
   *   the wasm module, Firefox                          ->  304, no body
   *   the wasm module, Chromium                         ->  304, then a full GET
   *
   * That last line is not something this file can fix, and it is worth knowing
   * before someone "optimises" it. The page instantiates the module with
   * `WebAssembly.instantiateStreaming`, and Chromium keeps the compiled result
   * rather than a reusable response body: it answers the revalidation, finds it
   * has no body to reuse, and refetches unconditionally. So a first visit in
   * Chromium transfers the 11 MB twice — once for the page, once for this cache
   * — and never again on any subsequent visit. Firefox transfers it once.
   *
   * The alternative, caching the module opportunistically when the fetch
   * handler first serves it instead of precaching it here, removes the second
   * transfer but breaks atomicity: the module would then be cached separately
   * from the glue that has to match it, which is a wasm-bindgen version
   * mismatch and a blank page. One extra transfer, once, is the cheaper bug.
   */
  try {
    await cache.addAll(SHELL_URLS.map((u) => new Request(u, { cache: "no-cache" })));
  } catch (e) {
    // `addAll` writes nothing when it rejects, but `caches.open` above already
    // created the cache. Left behind, it is an empty entry that
    // `openShellCache` would treat as a real generation and that a later
    // install under the same token would find pre-existing. Take it back out.
    await caches.delete(name);
    throw e;
  }
  await writeMeta({ token, cacheName: name, installedAt: Date.now() });
  metaPromise = null;
  return name;
}

/**
 * The caches an install must not delete.
 *
 * Every pinned generation, because a live client is mid-load in it, and the
 * generation being superseded, because `clientShells` is module state that a
 * worker restart loses — after which the only thing standing between a
 * half-loaded page and a 404 is the previous shell still existing.
 */
function cachesToKeep(newShellName, previousMeta) {
  const keep = new Set([META_CACHE, TILE_CACHE, newShellName]);
  for (const name of clientShells.values()) keep.add(name);
  if (previousMeta && previousMeta.cacheName) keep.add(previousMeta.cacheName);
  return keep;
}

async function purgeCaches(keep) {
  const names = await caches.keys();
  await Promise.all(
    names
      .filter((n) => n.startsWith("rustdar-") && !keep.has(n))
      .map((n) => caches.delete(n)),
  );
}

async function notifyClients(message) {
  const windows = await self.clients.matchAll({
    includeUncontrolled: true,
    type: "window",
  });
  for (const client of windows) client.postMessage(message);
}

/*
 * In-flight guard: concurrent navigations must share one probe, not race to
 * download 11 MB twice.
 */
let updateCheck = null;
let lastCheckedAt = 0;

/*
 * Consecutive probe failures, and the warnings already emitted.
 *
 * Both are module state and therefore reset when the worker is killed, which
 * makes them a floor rather than a count: a browser that is offline for a week
 * will not accumulate to the threshold, because each restart starts again. That
 * is the right way round. The failure worth shouting about is the one that
 * persists *while the worker is alive and busy* — a server that has started
 * refusing `HEAD`, which every navigation then re-probes and re-fails — and
 * that one reaches the threshold within a single lifetime. Being offline is
 * both temporary and already visible in the page's own offline banner.
 */
let probeFailures = 0;
const PROBE_FAILURE_WARN_AFTER = 3;
const warnedAbout = new Set();

function warnOnce(key, ...args) {
  if (warnedAbout.has(key)) return;
  warnedAbout.add(key);
  console.warn(...args);
}

/**
 * Probe for a new deploy and, if there is one, install it.
 *
 * Offline, or against a server with no validators, this is a no-op that leaves
 * the existing shell exactly as it was. It never deletes a working shell it
 * cannot replace.
 *
 * `force` bypasses the *time* throttle only. It cannot bypass the two ways this
 * function legitimately declines to act — a probe that threw, and a server with
 * no validators to compare — because in both cases there is genuinely no
 * evidence that anything changed. Overriding that is what `forceReinstall()` is
 * for, and it is a different operation: it does not ask whether to reinstall.
 */
function checkForUpdate({ force = false } = {}) {
  if (updateCheck) return updateCheck;
  if (!force && Date.now() - lastCheckedAt < UPDATE_PROBE_INTERVAL_MS) {
    return Promise.resolve();
  }

  updateCheck = (async () => {
    const meta = await currentMeta();

    let token;
    try {
      token = await probeValidator();
      probeFailures = 0;
    } catch (e) {
      // Offline, or the probe failed. Keep serving what we have.
      if (!meta) throw e;
      // A shell is installed and cannot be checked. If that persists, this
      // worker will serve the same deploy indefinitely with nothing on screen
      // saying so — which for a severe-weather application means quietly
      // running last month's code. Say so where a developer will see it.
      if (++probeFailures >= PROBE_FAILURE_WARN_AFTER) {
        console.warn(
          `rustdar sw: the shell version probe has failed ${probeFailures} ` +
            `times in a row (${e}). The cached shell cannot be checked for ` +
            `updates and will keep being served. If this is not simply an ` +
            `offline device, post {type:"rustdar:force-update"} to this ` +
            `worker to reinstall the shell unconditionally.`,
        );
      }
      return;
    }
    lastCheckedAt = Date.now();

    // A server with no validators: install once, then leave it alone. There is
    // no signal here that could tell a new deploy from the old one.
    if (token === null) {
      if (meta) {
        warnOnce(
          "unversioned",
          'rustdar sw: the server sends neither ETag nor Last-Modified for the ' +
            "wasm module, so a new deploy cannot be detected and the cached " +
            'shell is pinned. Post {type:"rustdar:force-update"} to reinstall it.',
        );
        return;
      }
      token = "unversioned";
    }

    if (meta && meta.token === token) return;

    const name = await installShell(token);
    await purgeCaches(cachesToKeep(name, meta));

    // Only announce a *replacement*. The first install has nothing to replace:
    // the page that triggered it is already running the code just cached, and
    // telling it to reload would be a lie.
    if (meta) await notifyClients({ type: "rustdar:shell-updated", token });
  })().finally(() => {
    updateCheck = null;
  });

  return updateCheck;
}

/**
 * Reinstall the shell from the network without asking whether it changed.
 *
 * The escape hatch for a degraded probe. `checkForUpdate` compares a token
 * against the stored one, so it is useless in exactly the cases where it is
 * most needed: a server that has started answering `HEAD` with 405, or one that
 * publishes no validators at all. Both leave `checkForUpdate` correctly
 * concluding "no evidence of a change" forever.
 *
 * The download lands in a cache of its own even when the token is unchanged,
 * because that token may name a generation live clients are pinned to and
 * refetching into it would rewrite a shell out from under a page mid-load. The
 * *token* recorded in the meta record is still the probe's, so the next
 * ordinary check compares like with like and does not reinstall again.
 */
async function forceReinstall() {
  if (updateCheck) await updateCheck.catch(() => {});

  const meta = await currentMeta();
  let token = null;
  try {
    token = await probeValidator();
    probeFailures = 0;
  } catch (e) {
    console.warn("rustdar sw: forced reinstall could not probe the version:", e);
  }
  if (token === null) token = meta ? meta.token : "unversioned";

  const name = `${shellCacheName(token)}-forced-${Date.now()}`;
  await installShell(token, name);
  await purgeCaches(cachesToKeep(name, meta));
  if (meta) await notifyClients({ type: "rustdar:shell-updated", token });
  return name;
}

// ---------------------------------------------------------------------------
// Fetch strategies
// ---------------------------------------------------------------------------

/**
 * Serve a shell asset from the generation `clientId` is pinned to.
 *
 * `clientId` is the empty string for a request whose client the browser cannot
 * name, which falls back to the current generation — correct, because such a
 * request is not part of a page load this worker pinned.
 */
async function serveShell(request, clientId, key) {
  const cache = await shellCacheForClient(clientId);
  if (cache) {
    const hit = await cache.match(key ?? request, { ignoreSearch: true });
    if (hit) return hit;
  }
  // No shell yet (first visit, or an update that has not finished): the network
  // is the source of truth and nothing is written here. `checkForUpdate` owns
  // every write to the shell cache, so this path cannot publish a stray entry.
  return fetch(request);
}

/**
 * Answer a navigation, and pin the answering generation to the new client.
 *
 * The pin is taken here and nowhere else, because this is the only point in a
 * page load at which "which deploy is this page" is still an open question.
 * After this, it is settled for every subresource that client will request.
 */
async function serveNavigation(event) {
  // Prune before pinning: the client this navigation creates does not exist
  // yet, so pruning afterwards would immediately drop the pin just taken.
  await forgetDepartedClients();

  const meta = await currentMeta();
  const clientId = event.resultingClientId || event.clientId;
  if (meta && meta.cacheName && clientId) clientShells.set(clientId, meta.cacheName);

  const cache = await openShellCache(meta && meta.cacheName);
  if (cache) {
    const hit = await cache.match(ROOT.href, { ignoreSearch: true });
    if (hit) return hit;
  }
  return fetch(event.request);
}

/** Milliseconds a cached tile is good for, per the response's own headers. */
function tileFreshFor(response) {
  const control = response.headers.get("cache-control") || "";
  const match = /(?:^|,)\s*max-age\s*=\s*(\d+)/i.exec(control);
  if (match) return Number(match[1]) * 1000;
  return TILE_DEFAULT_MAX_AGE_MS;
}

function tileIsStale(response) {
  // An opaque response (a `no-cors` fetch) exposes no headers at all. There is
  // nothing to reason about, and a basemap tile does not go dangerously wrong
  // with age, so it is kept.
  if (response.type === "opaque") return false;

  const date = response.headers.get("date");
  // A readable response with no `Date` is treated as stale rather than as
  // fresh. Freshness here is an assertion about age, and with no clock in the
  // response there is nothing to assert it from; declining to answer used to
  // mean such an entry was never revalidated for as long as it existed. The
  // cost of the other direction is one conditional request — this is
  // stale-while-revalidate, so the cached tile is still served immediately.
  if (!date) return true;

  const age = Date.now() - Date.parse(date);
  // An unparseable `Date` is the same situation as an absent one.
  if (!Number.isFinite(age)) return true;
  return age > tileFreshFor(response);
}

async function cacheTile(cache, request, response) {
  // `cache.put` throws on a 206 and on anything non-2xx. Status 0 is an opaque
  // response, which is cacheable and is what a `no-cors` tile fetch yields.
  if (response.status === 200 || response.type === "opaque") {
    await cache.put(request, response.clone());
  }
  return response;
}

/**
 * Cache-first, with revalidation once the origin's own `max-age` has passed.
 *
 * Serving the stale copy while revalidating is safe here in a way it would
 * never be for weather data: the worst outcome is one screen drawn with last
 * month's rendering of the same coastline.
 */
async function serveTile(event) {
  const cache = await caches.open(TILE_CACHE);
  const hit = await cache.match(event.request);

  if (hit) {
    if (tileIsStale(hit)) {
      event.waitUntil(
        fetch(event.request)
          .then((fresh) => cacheTile(cache, event.request, fresh))
          .catch(() => {}),
      );
    }
    return hit;
  }

  // Let a failure reject, exactly as an uncontrolled fetch would, so walkers
  // sees an ordinary network error rather than a synthetic response.
  const response = await fetch(event.request);
  await cacheTile(cache, event.request, response);
  event.waitUntil(trimTiles());
  return response;
}

/*
 * Trim bookkeeping.
 *
 * `cache.keys()` walks every entry, so it is amortised over a batch of writes
 * rather than paid per tile. The subtlety is that this counter is module state
 * and a service worker is killed after roughly thirty seconds idle — far more
 * often than a user finishes panning a map.
 *
 * Counting alone was therefore not a bound at all. A user panning slowly enough
 * to fetch fewer than `TILE_TRIM_BATCH` new tiles per worker lifetime never
 * reached the threshold, the counter went back to zero with the worker, and the
 * basemap cache grew without limit — which is quota pressure, which is what
 * makes the shell's all-or-nothing `addAll` fail.
 *
 * `trimmedThisLifetime` closes it: the first tile written by any worker
 * instance always pays for a full check. A restart is now the event that
 * guarantees a trim rather than the event that skips one, and within a single
 * lifetime the cache can exceed `TILE_CACHE_MAX` by at most one batch.
 */
const TILE_TRIM_BATCH = 50;
let tilePutsSinceTrim = 0;
let trimmedThisLifetime = false;

async function trimTiles() {
  if (trimmedThisLifetime && ++tilePutsSinceTrim < TILE_TRIM_BATCH) return;
  trimmedThisLifetime = true;
  tilePutsSinceTrim = 0;

  const cache = await caches.open(TILE_CACHE);
  const keys = await cache.keys();
  const excess = keys.length - TILE_CACHE_MAX;
  if (excess <= 0) return;
  // Cache.keys() yields insertion order, so the head is the oldest. FIFO rather
  // than LRU: tracking access times would mean a write per read.
  await Promise.all(keys.slice(0, excess).map((k) => cache.delete(k)));
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

self.addEventListener("install", () => {
  // Nothing here. Precaching during install would make a flaky network fail the
  // installation, and `skipWaiting()` would swap the controller under a running
  // page. Both are handled deliberately elsewhere: the shell is installed by
  // `checkForUpdate()` on activate, and the swap waits for the user to accept
  // the reload prompt (see the `message` handler).
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    (async () => {
      // Control the page that registered us, so the update channel works on the
      // very first visit instead of only after a reload.
      await self.clients.claim();
      // Bound the tile cache at every version change as well as at every worker
      // start. Cheap — one `keys()` walk — and it is the one moment at which
      // this worker is certain to be running and not in a hurry.
      await trimTiles().catch(() => {});
      try {
        await checkForUpdate({ force: true });
      } catch {
        // First visit while offline. There is nothing to cache and nothing to
        // do; the next navigation tries again.
      }
    })(),
  );
});

self.addEventListener("fetch", (event) => {
  const route = routeFor({
    url: event.request.url,
    method: event.request.method,
    mode: event.request.mode,
  });

  switch (route) {
    case "navigate":
      // Every page load is a chance to notice a new deploy. `waitUntil` keeps
      // the worker alive for the probe without delaying the response.
      //
      // Ordering matters: `serveNavigation` reads the meta pointer and pins the
      // new client to it, and it must do so against the pointer as it stands
      // *now*. `respondWith` is called first so that the pin is taken before
      // the probe above can move the pointer underneath it.
      event.respondWith(serveNavigation(event));
      event.waitUntil(checkForUpdate().catch(() => {}));
      return;
    case "shell":
      event.respondWith(serveShell(event.request, event.clientId));
      return;
    case "tile":
      event.respondWith(serveTile(event));
      return;
    default:
      // "network": return without calling respondWith. The browser performs its
      // ordinary networking and this worker is not in the request path at all.
      // Every weather-data request in the application ends here.
      return;
  }
});

self.addEventListener("message", (event) => {
  const type = event.data && event.data.type;
  if (type === "rustdar:skip-waiting") {
    // The user accepted the reload prompt. Only now is it safe to replace the
    // controller: the page is about to be torn down anyway.
    self.skipWaiting();
  } else if (type === "rustdar:check-update") {
    // A long-lived tab coming back to the foreground. Such a tab never
    // navigates, so this is its only chance to notice a deploy.
    event.waitUntil(checkForUpdate({ force: true }).catch(() => {}));
  } else if (type === "rustdar:force-update") {
    // Reinstall regardless of what the probe thinks. See `forceReinstall`.
    event.waitUntil(
      forceReinstall().catch((e) => console.warn("rustdar sw: forced reinstall failed:", e)),
    );
  }
});

/*
 * Test hook.
 *
 * `tests/sw_routing.test.mjs` loads this file's shipped bytes into a scope that
 * models a ServiceWorkerGlobalScope, calls `routeFor` directly, and drives the
 * `fetch`, `activate` and `message` handlers above. Asserting against the
 * shipped worker is the only way to test the policy that actually runs; a
 * re-implementation in the test would assert only that the test agrees with
 * itself. `rustdar-web/tests/sw_behaviour.rs` runs that suite under
 * `cargo test`, so it gates the same builds every other test here gates.
 */
self.__rustdarSwInternals = {
  ROOT,
  SHELL_URLS,
  NEVER_CACHE_HOSTS,
  TILE_CACHE,
  TILE_CACHE_MAX,
  TILE_TRIM_BATCH,
  SHELL_PREFIX,
  routeFor,
  isWeatherDataHost,
  isBasemapTile,
  isShellAsset,
  normalizeHost,
  validatorToken,
  tileFreshFor,
  tileIsStale,
};
