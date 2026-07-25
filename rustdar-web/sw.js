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
 */

"use strict";

/*
 * Bump when the logic in this file changes. It only names caches; the deployed
 * content's version is the validator token, discovered at runtime.
 */
const SW_VERSION = 1;

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

function isWeatherDataHost(hostname) {
  if (NEVER_CACHE_HOSTS.has(hostname)) return true;
  // Suffix rules so a bucket rename or a new NWS subdomain is still never
  // cached without this file having to be updated first.
  return (
    hostname === "amazonaws.com" ||
    hostname.endsWith(".amazonaws.com") ||
    hostname === "noaa.gov" ||
    hostname.endsWith(".noaa.gov") ||
    hostname === "weather.gov" ||
    hostname.endsWith(".weather.gov")
  );
}

function isBasemapTile(url) {
  return BASEMAP_HOST.test(url.hostname) && url.pathname.endsWith(".png");
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

  // Checked before anything else, so no later rule can reach a data origin.
  if (isWeatherDataHost(u.hostname)) return "network";

  if (isBasemapTile(u)) return "tile";

  if (u.origin === ROOT.origin) {
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
 * The shell cache for the currently published deploy, memoised so the hot path
 * is one `caches.open` rather than a meta read per subresource. Cleared
 * whenever the meta pointer moves.
 */
let shellCachePromise = null;

function currentShellCache() {
  if (!shellCachePromise) {
    shellCachePromise = (async () => {
      const meta = await readMeta();
      return meta ? caches.open(meta.cacheName) : null;
    })();
  }
  return shellCachePromise;
}

/**
 * Download the whole shell into a cache named for `token`, then publish it.
 *
 * The publish is the `writeMeta` at the end, and it is the only step that makes
 * the new shell visible. `addAll` is all-or-nothing: it rejects without writing
 * anything if any request fails or answers non-2xx, so a partially downloaded
 * deploy can never be published.
 */
async function installShell(token) {
  const name = shellCacheName(token);
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
  await cache.addAll(SHELL_URLS.map((u) => new Request(u, { cache: "no-cache" })));
  await writeMeta({ token, cacheName: name, installedAt: Date.now() });
  shellCachePromise = null;
  return name;
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

/**
 * Probe for a new deploy and, if there is one, install it.
 *
 * Offline, or against a server with no validators, this is a no-op that leaves
 * the existing shell exactly as it was. It never deletes a working shell it
 * cannot replace.
 */
function checkForUpdate({ force = false } = {}) {
  if (updateCheck) return updateCheck;
  if (!force && Date.now() - lastCheckedAt < UPDATE_PROBE_INTERVAL_MS) {
    return Promise.resolve();
  }

  updateCheck = (async () => {
    const meta = await readMeta();

    let token;
    try {
      token = await probeValidator();
    } catch (e) {
      // Offline, or the probe failed. Keep serving what we have.
      if (!meta) throw e;
      return;
    }
    lastCheckedAt = Date.now();

    // A server with no validators: install once, then leave it alone. There is
    // no signal here that could tell a new deploy from the old one.
    if (token === null) {
      if (meta) return;
      token = "unversioned";
    }

    if (meta && meta.token === token) return;

    const name = await installShell(token);
    await purgeCaches(new Set([META_CACHE, TILE_CACHE, name]));

    // Only announce a *replacement*. The first install has nothing to replace:
    // the page that triggered it is already running the code just cached, and
    // telling it to reload would be a lie.
    if (meta) await notifyClients({ type: "rustdar:shell-updated", token });
  })().finally(() => {
    updateCheck = null;
  });

  return updateCheck;
}

// ---------------------------------------------------------------------------
// Fetch strategies
// ---------------------------------------------------------------------------

async function serveShell(request, key) {
  const cache = await currentShellCache();
  if (cache) {
    const hit = await cache.match(key ?? request, { ignoreSearch: true });
    if (hit) return hit;
  }
  // No shell yet (first visit, or an update that has not finished): the network
  // is the source of truth and nothing is written here. `checkForUpdate` owns
  // every write to the shell cache, so this path cannot publish a stray entry.
  return fetch(request);
}

/** Milliseconds a cached tile is good for, per the response's own headers. */
function tileFreshFor(response) {
  const control = response.headers.get("cache-control") || "";
  const match = /(?:^|,)\s*max-age\s*=\s*(\d+)/i.exec(control);
  if (match) return Number(match[1]) * 1000;
  return TILE_DEFAULT_MAX_AGE_MS;
}

function tileIsStale(response) {
  const date = response.headers.get("date");
  // An opaque response (a `no-cors` fetch) exposes no headers at all. There is
  // nothing to reason about, and a basemap tile does not go dangerously wrong
  // with age, so it is kept.
  if (!date) return false;
  const age = Date.now() - Date.parse(date);
  return Number.isFinite(age) && age > tileFreshFor(response);
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

let tilePutsSinceTrim = 0;

async function trimTiles() {
  // `keys()` walks the whole cache, so amortise it rather than paying it per
  // tile. The cache is allowed to overshoot TILE_CACHE_MAX by up to one batch.
  if (++tilePutsSinceTrim < 50) return;
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
      event.waitUntil(checkForUpdate().catch(() => {}));
      event.respondWith(serveShell(event.request, ROOT.href));
      return;
    case "shell":
      event.respondWith(serveShell(event.request));
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
  }
});

/*
 * Test hook.
 *
 * `tests/sw_routing.test.mjs` evaluates this file in a sandbox and calls
 * `routeFor` directly. Asserting against the shipped worker is the only way to
 * test the policy that actually runs; a re-implementation in the test would
 * assert only that the test agrees with itself.
 */
self.__rustdarSwInternals = {
  ROOT,
  SHELL_URLS,
  NEVER_CACHE_HOSTS,
  routeFor,
  isWeatherDataHost,
  isBasemapTile,
  isShellAsset,
  validatorToken,
  tileFreshFor,
};
