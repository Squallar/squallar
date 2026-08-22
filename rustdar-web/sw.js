/*
 * rustdar's service worker: shell cached, weather data never cached, basemap
 * tiles cached aggressively.
 *
 * "Never" is literal — the worker does not call `respondWith()` for weather
 * data, so no code here *can* write it to a cache. Those fetches fail offline
 * and index.html shows a banner. Routing is default-deny, so a new entry in
 * `rustdar_radar::sources::DataSources` is uncached with no change here.
 *
 * Tiles are the exception because a slippy-map tile has no time dimension.
 * CartoDB serves them `Cache-Control: public, max-age=15552000` (180 days) and
 * `tileFreshFor` honours that number rather than inventing one.
 *
 * Every URL resolves against `ROOT`, so the same bytes work at `/rustdar/`
 * (Pages) and at `/` (http.server).
 *
 * SHELL VERSIONING. The browser only reinstalls a worker whose *own* bytes
 * changed, and these do not change when the wasm module does. Hashing the bundle
 * in would need a build step that rewrites this file, and the Pages deployment
 * is not ours to change — so the version is discovered at runtime instead:
 * `probeValidator()` HEADs the wasm module and the directory index and joins
 * the validators the server already sends for each into one token. A changed
 * token refetches the whole shell into a *new* cache and moves the meta pointer
 * only once that completes. `SW_VERSION` versions this file's logic and is
 * bumped by hand.
 *
 * MIXED SHELLS are what per-client pinning exists to prevent. A page load is
 * three fetches seconds apart (navigation, glue, wasm) and a deploy can land
 * between any two. Only one direction of a glue/wasm mismatch is loud:
 *
 *   old glue + new wasm  ->  LinkError at instantiate.
 *   new glue + old wasm  ->  instantiates cleanly, the page paints, then dies at
 *                            the first closure dispatch (a winit event or rAF)
 *                            with `TypeError: wasm.__wasm_bindgen_func_elem_12978
 *                            is not a function`. The glue reads trampolines off
 *                            `wasm.` lazily, so nothing surfaces at load.
 *
 * wasm-bindgen 0.2.126 names closure trampolines after the compiled
 * function-table index, so an *interface* change renames all 15 of them; an
 * ordinary code change emits byte-identical glue and is harmless.
 *
 * So the generation is pinned per client at the navigation, navigations are
 * served cache-first, and the pin is written to the meta cache as well as held
 * in memory — the worker is killed after ~30s idle, and a restart between a
 * page's navigation and its wasm request would otherwise put that page back onto
 * whatever is current. Retaining "the generation just superseded" instead was
 * tried and is strictly worse: it keeps a cache that may have no reader while
 * still allowing deletion of one that does.
 *
 * A client mid-session is never swapped; it gets `rustdar:shell-updated` and
 * index.html offers a Reload.
 */

"use strict";

/* Names caches only. The deployed content's version is the validator token. */
const SW_VERSION = 2;

/*
 * The directory this worker was served from, with a trailing slash. Not
 * `self.registration.scope`: that is unavailable in some engines while the module
 * body evaluates, and `Service-Worker-Allowed` can widen it past the script's
 * directory, which the relative asset paths below are relative to.
 */
const ROOT = new URL("./", self.location.href);

const META_CACHE = `rustdar-meta-v${SW_VERSION}`;
const TILE_CACHE = `rustdar-basemap-v${SW_VERSION}`;
const SHELL_PREFIX = `rustdar-shell-v${SW_VERSION}-`;

/* Synthetic keys for the meta and client-pin records; nothing is served from them. */
const META_KEY = new URL("__rustdar_sw_meta__", ROOT).href;
const PINS_KEY = new URL("__rustdar_sw_pins__", ROOT).href;

/*
 * The app shell, relative to ROOT. Ten entries, but the wasm module
 * (10,161,914 B) and its glue (117,911 B) are essentially all of it — index.html
 * and the icons together are under 260 KB.
 *
 * `""` is the directory index (`new URL("", ROOT)` is ROOT) and is the single
 * entry every navigation is answered from. `index.html` is deliberately not
 * listed too: caching the same bytes under two keys is how the two copies end up
 * from different deploys. `pkg/` is what `wasm-pack build --target web` emits;
 * its `.d.ts` and `package.json` are build-time artefacts.
 *
 * `worker.js` boots the rasterization worker. It is a shell asset and not an
 * afterthought: without it in the precache, the first offline load renders
 * radar frames on the main thread — correct, but a fifth of a second of frozen
 * UI per frame. It loads no bytes of its own beyond the `pkg/` pair already
 * here, which is what keeps this list one deploy generation wide.
 */
const SHELL_PATHS = [
  "",
  "manifest.webmanifest",
  "worker.js",
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

/*
 * The assets whose HTTP validators stand in for "which deploy is this". Two,
 * because a deploy has two independent halves: the wasm bundle, and the shell
 * around it. A probe that watched only the wasm never saw a deploy that changed
 * index.html without rebuilding the module — and navigations are cache-first,
 * so the old shell was served indefinitely. The directory index is the asset a
 * shell-side deploy is most certain to touch (it inlines the CSS and the
 * bootstrap); an icon-only deploy still slips through, which is what
 * `rustdarForceUpdate()` is for — closing it would cost nine HEADs per check.
 */
const SHELL_VERSION_PROBES = [
  new URL("pkg/rustdar_web_bg.wasm", ROOT).href,
  new URL("", ROOT).href,
];

/*
 * Every origin `rustdar_source::origins::DataSources::production()` reads from,
 * and nothing else. Belt and braces on top of default-deny, but
 * `tests/pwa_assets.rs` pins the set against the Rust declaration in BOTH
 * directions -- every origin appears here, and every entry here is an origin --
 * so a new data source cannot be added without someone reading this file, and a
 * retired one cannot linger in the list once it is gone from `DataSources`.
 *
 * Basemap tiles are NOT here on purpose: they are cached deliberately, by
 * `BASEMAP_HOST` and its own route below.
 */
const NEVER_CACHE_HOSTS = new Set([
  "unidata-nexrad-level2.s3.amazonaws.com",
  "unidata-nexrad-level2-chunks.s3.amazonaws.com",
  "unidata-nexrad-level3.s3.amazonaws.com",
  "noaa-hrrr-bdp-pds.s3.amazonaws.com",
  "noaa-goes19.s3.amazonaws.com",
  "noaa-goes18.s3.amazonaws.com",
  "noaa-mrms-pds.s3.amazonaws.com",
  "api.weather.gov",
  "www.spc.noaa.gov",
  "mesonet.agron.iastate.edu",
  "api.open-meteo.com",
]);

/* CartoDB's four tile subdomains, as `rustdar_egui::tiles::CartoDb` builds them. */
const BASEMAP_HOST = /^cartodb-basemaps-[a-d]\.global\.ssl\.fastly\.net$/;

/* ~15-25 KB per 256px PNG, so roughly 12-20 MB. */
const TILE_CACHE_MAX = 700;

const TILE_DEFAULT_MAX_AGE_MS = 30 * 24 * 60 * 60 * 1000;
const UPDATE_PROBE_INTERVAL_MS = 60 * 1000;

// ---------------------------------------------------------------------------
// Routing
// ---------------------------------------------------------------------------

/**
 * `new URL()` preserves the root label of a fully-qualified `api.weather.gov.`,
 * which is neither in `NEVER_CACHE_HOSTS` nor a `".weather.gov"` suffix match.
 * Case only matters for a direct call, since `new URL()` lowercases the host.
 * Default-deny already catches both; normalising here keeps a future rule that
 * leans on this from inheriting a hole only the default covers.
 */
function normalizeHost(hostname) {
  let host = String(hostname).toLowerCase();
  while (host.endsWith(".")) host = host.slice(0, -1);
  return host;
}

function isWeatherDataHost(hostname) {
  const host = normalizeHost(hostname);
  if (NEVER_CACHE_HOSTS.has(host)) return true;
  // Suffix rules so a bucket rename or a new NWS subdomain is still never cached
  // without this file having to be updated first.
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
  // The host is what confines this rule; the extension only distinguishes a tile
  // from the other things that host serves, so `.PNG` matches too.
  return BASEMAP_HOST.test(normalizeHost(url.hostname)) && /\.png$/i.test(url.pathname);
}

function isShellAsset(url) {
  // Query and fragment dropped so a cache-busted `?v=2` still resolves.
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

  // Defence in depth, currently unreachable and untestable: a `blob:` pathname
  // is the inner URL, never under `ROOT.pathname`, so default-deny already
  // answers every non-http(s) scheme. Removing it is an equivalent mutant. Keep
  // it for a future rule that matches on something other than the path.
  if (u.protocol !== "http:" && u.protocol !== "https:") return "network";

  // Nothing rustdar issues carries credentials, so their presence means the
  // request was not built by this application — and a cache entry keyed by a URL
  // containing a password is a bad thing to own regardless of host.
  if (u.username || u.password) return "network";

  // First, so no later rule can reach a data origin. Not redundant with
  // default-deny: it stops the two rules that *do* say yes from matching a
  // weather origin if rustdar is ever served from one, behind a proxy or on a
  // shared host.
  if (isWeatherDataHost(u.hostname)) return "network";

  if (isBasemapTile(u)) return "tile";

  // `ROOT.pathname` ends in a slash, so this is directory containment, not a
  // prefix match: `/rustdar/` does not match `/rustdar-old/x`. Without it a
  // user-site deploy at `https://<user>.github.io/` would answer navigations for
  // every other project on that origin with rustdar's index.html. Worker scope
  // confines this today; scope is not the thing to rely on.
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
 * Both sides of every comparison come from a `HEAD` issued by
 * `probeValidator()`, never from a stored `GET`, so the token is insensitive to
 * the header differences between the two (notably `Content-Length` under content
 * negotiation).
 *
 * `null` means the server publishes no validators for that asset. When every
 * probe says so, updates cannot be detected and the shell is left alone rather
 * than re-downloaded on every load. Pages sends `ETag`; `http.server` sends
 * `Last-Modified` + `Content-Length`.
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
  // `no-store` so the browser's own HTTP cache cannot answer these and hide a
  // new deploy behind an unexpired `max-age`. Any probe failing fails them all:
  // a token built from the assets that did answer would change again when the
  // outage ended, and a phantom token change is a pointless reinstall. Throwing
  // instead lands in `checkForUpdate`'s keep-serving path, same as one probe.
  const tokens = await Promise.all(
    SHELL_VERSION_PROBES.map(async (url) => {
      const response = await fetch(url, { method: "HEAD", cache: "no-store" });
      if (!response.ok) throw new Error(`shell probe: HTTP ${response.status} for ${url}`);
      return validatorToken(response);
    }),
  );
  // All-null keeps its meaning: nothing here can detect a deploy. One null
  // degrades only that asset — a change in the other is still a changed token.
  if (tokens.every((t) => t === null)) return null;
  return tokens.map((t) => t ?? "none").join("+");
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

/* Memoised so the hot path is one cache lookup, not a meta read per subresource. */
let metaPromise = null;

function currentMeta() {
  if (!metaPromise) metaPromise = readMeta();
  return metaPromise;
}

/*
 * Which shell generation each live client is being served from, keyed by the
 * client id its navigation created. Mirrored into the meta cache by `writePins`,
 * since this is module state and the worker is killed after ~30s idle. The
 * durable copy is also what makes `cachesToKeep` exact rather than a guess.
 */
const clientShells = new Map();

/**
 * `caches.open` creates on demand, so opening a purged generation would silently
 * manufacture an empty cache that every lookup misses and that sits in storage
 * forever. Ask first; null means gone.
 */
async function openShellCache(name) {
  if (!name) return null;
  if (!(await caches.has(name))) return null;
  return caches.open(name);
}

async function readPins() {
  const cache = await caches.open(META_CACHE);
  const stored = await cache.match(PINS_KEY);
  if (!stored) return {};
  try {
    return await stored.json();
  } catch {
    return {};
  }
}

async function writePins(pins) {
  const cache = await caches.open(META_CACHE);
  await cache.put(
    PINS_KEY,
    new Response(JSON.stringify(pins), {
      headers: { "content-type": "application/json" },
    }),
  );
}

async function liveClientIds() {
  const windows = await self.clients.matchAll({ includeUncontrolled: true, type: "window" });
  return new Set(windows.map((c) => c.id));
}

/**
 * Record `cacheName` as the generation serving `clientId`, durably. Departed
 * clients are pruned in the same write, bounding the record by open tabs rather
 * than by tabs ever opened; `clientId` is exempt because the navigation creating
 * it has not finished, so it is not yet in `live`.
 */
async function pinClient(clientId, cacheName) {
  clientShells.set(clientId, cacheName);

  const live = await liveClientIds();
  const pins = await readPins();
  for (const id of Object.keys(pins)) {
    if (id !== clientId && !live.has(id)) delete pins[id];
  }
  pins[clientId] = cacheName;
  await writePins(pins);

  for (const id of [...clientShells.keys()]) {
    if (id !== clientId && !live.has(id)) clientShells.delete(id);
  }
}

/** The shell generation `clientId` is pinned to, falling back to the current one. */
async function shellCacheForClient(clientId) {
  if (clientId) {
    let pinned = clientShells.get(clientId);
    if (!pinned) {
      // Either this client never navigated through this worker, or the worker
      // restarted since it did. The second is why the pin is written down.
      const pins = await readPins();
      pinned = pins[clientId];
      if (pinned) clientShells.set(clientId, pinned);
    }
    if (pinned) {
      const cache = await openShellCache(pinned);
      // A pin whose cache is gone falls through: the network is always a correct
      // answer, just a slower one.
      if (cache) return cache;
    }
  }
  const meta = await currentMeta();
  return openShellCache(meta && meta.cacheName);
}

/**
 * Download the whole shell into a cache named for `token`, then publish it. The
 * trailing `writeMeta` is the publish and the only step that makes the new shell
 * visible; `addAll` is all-or-nothing, so a partially downloaded deploy can never
 * be published.
 */
async function installShell(token, name = shellCacheName(token)) {
  // A rollback re-issues a token this worker has installed before, so `name`
  // can already exist — and can be the very generation a live client is pinned
  // to, retained through every purge because of that pin. Note which case this
  // is before `caches.open` creates the cache and erases the distinction.
  const preExisting = await caches.has(name);
  const cache = await caches.open(name);
  /*
   * `no-cache`, not `reload`. Both bypass a stale `max-age` (Pages serves
   * `max-age=600`, so a plain fetch could rebuild the shell out of the previous
   * deploy's bytes under the new token), but `reload` downloads unconditionally
   * where `no-cache` revalidates for a 304.
   *
   * Against `ETag` + `max-age=600`, a first visit is bodyless 304s except the
   * wasm module in Chromium, which 304s and then issues a full GET: the page
   * instantiated it with `WebAssembly.instantiateStreaming`, which keeps the
   * compiled result and no reusable body. Chromium therefore transfers the
   * ~10 MB module twice on a first visit and never again; Firefox transfers it
   * once.
   * Not fixable here. Caching the module opportunistically in the fetch handler
   * would remove the second transfer and break atomicity — the module would be
   * cached separately from the glue that has to match it.
   */
  try {
    await cache.addAll(SHELL_URLS.map((u) => new Request(u, { cache: "no-cache" })));
  } catch (e) {
    // `addAll` writes nothing when it rejects, so `name` now holds exactly what
    // it held before the call. For a cache this install created, that is the
    // empty husk `caches.open` above manufactured: left behind, `openShellCache`
    // would treat it as a real generation and a later install under the same
    // token would find it pre-existing. For a cache that predated this install
    // — a rollback — it is a complete shell, possibly the one a pinned page is
    // mid-load in, and deleting it would be this installer causing the exact
    // mixed shell the pinning exists to prevent. Delete only what was created.
    if (!preExisting) await caches.delete(name);
    throw e;
  }
  await writeMeta({ token, cacheName: name, installedAt: Date.now() });
  metaPromise = null;
  return name;
}

/**
 * The caches an install must not delete: the new shell, and every pinned one.
 * Pins come from storage, not just `clientShells`, so a restarted worker still
 * knows a live page's generation is in use.
 */
async function cachesToKeep(newShellName) {
  const keep = new Set([META_CACHE, TILE_CACHE, newShellName]);
  for (const name of clientShells.values()) keep.add(name);
  for (const name of Object.values(await readPins())) keep.add(name);
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

/* In-flight guard: concurrent navigations share one probe rather than racing. */
let updateCheck = null;
let lastCheckedAt = 0;

/*
 * Module state, so a restart resets the count. Deliberate: the failure worth
 * shouting about is one that persists while the worker is alive and busy (a
 * server that has started refusing `HEAD`, re-probed by every navigation), and
 * that reaches the threshold within one lifetime. Being offline is temporary and
 * already visible in the page's offline banner.
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
 * Probe for a new deploy and, if there is one, install it. Never deletes a
 * working shell it cannot replace.
 *
 * `force` bypasses the *time* throttle only — not a probe that threw or a server
 * with no validators, since in both cases there is genuinely no evidence
 * anything changed. `forceReinstall()` is the operation that does not ask.
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
      // A shell that is installed and cannot be checked will be served
      // indefinitely with nothing on screen saying so — for a severe-weather
      // application, quietly running last month's code.
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

    // No validators: install once, then leave it alone. Nothing here could tell
    // a new deploy from the old one.
    if (token === null) {
      if (meta) {
        warnOnce(
          "unversioned",
          'rustdar sw: the server sends neither ETag nor Last-Modified for the ' +
            "wasm module or the directory index, so a new deploy cannot be " +
            "detected and the cached shell is pinned. Post " +
            '{type:"rustdar:force-update"} to reinstall it.',
        );
        return;
      }
      token = "unversioned";
    }

    if (meta && meta.token === token) return;

    const name = await installShell(token);
    await purgeCaches(await cachesToKeep(name));

    // Only announce a *replacement*. The page that triggered a first install is
    // already running the code just cached, so telling it to reload would lie.
    if (meta) await notifyClients({ type: "rustdar:shell-updated", token });
  })().finally(() => {
    updateCheck = null;
  });

  return updateCheck;
}

/**
 * The escape hatch for a degraded probe: a server answering `HEAD` with 405, or
 * publishing no validators, leaves `checkForUpdate` correctly concluding "no
 * evidence of a change" forever.
 *
 * The download lands in a cache of its own even when the token is unchanged,
 * because that token may name a generation live clients are pinned to and
 * refetching into it would rewrite a shell out from under a page mid-load. The
 * *token* recorded in meta is still the probe's, so the next ordinary check
 * compares like with like.
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
  await purgeCaches(await cachesToKeep(name));
  if (meta) await notifyClients({ type: "rustdar:shell-updated", token });
  return name;
}

// ---------------------------------------------------------------------------
// Fetch strategies
// ---------------------------------------------------------------------------

/**
 * `clientId` is the empty string for a request the browser cannot attribute,
 * which falls back to the current generation — correct, because such a request
 * is not part of a page load this worker pinned.
 */
async function serveShell(request, clientId, key) {
  const cache = await shellCacheForClient(clientId);
  if (cache) {
    const hit = await cache.match(key ?? request, { ignoreSearch: true });
    if (hit) return hit;
  }
  // No shell yet (first visit, or an unfinished update). Nothing is written
  // here: `checkForUpdate` owns every write to the shell cache.
  return fetch(request);
}

/**
 * The pin is taken here and nowhere else: this is the only point in a page load
 * at which "which deploy is this page" is still open.
 */
async function serveNavigation(event) {
  const meta = await currentMeta();
  const clientId = event.resultingClientId || event.clientId;
  if (meta && meta.cacheName && clientId) await pinClient(clientId, meta.cacheName);

  const cache = await openShellCache(meta && meta.cacheName);
  if (cache) {
    const hit = await cache.match(ROOT.href, { ignoreSearch: true });
    if (hit) return hit;
  }
  return fetch(event.request);
}

/** How long a cached tile is good for, per the response's own `Cache-Control`. */
function tileFreshFor(response) {
  const control = response.headers.get("cache-control") || "";
  const match = /(?:^|,)\s*max-age\s*=\s*(\d+)/i.exec(control);
  if (match) return Number(match[1]) * 1000;
  return TILE_DEFAULT_MAX_AGE_MS;
}

function tileIsStale(response) {
  // An opaque (`no-cors`) response exposes no headers, and a basemap tile does
  // not go dangerously wrong with age.
  if (response.type === "opaque") return false;

  const date = response.headers.get("date");
  // No clock, nothing to assert freshness from. Treating that as fresh used to
  // mean such an entry was never revalidated for as long as it existed; the cost
  // of this direction is one conditional request, since the cached tile is still
  // served immediately.
  if (!date) return true;

  const age = Date.now() - Date.parse(date);
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
 * Cache-first, revalidating once the origin's own `max-age` has passed. Serving
 * stale is safe here in a way it never would be for weather data: the worst
 * outcome is last month's rendering of the same coastline.
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
 * `cache.keys()` walks every entry, so trimming is amortised over a batch.
 * Counting alone was not a bound: the counter is module state, the worker dies
 * after ~30s idle, and a user panning slowly enough to fetch fewer than
 * `TILE_TRIM_BATCH` tiles per lifetime never reached the threshold, so the cache
 * grew without limit — quota pressure, which is what makes the shell's
 * all-or-nothing `addAll` fail. `trimmedThisLifetime` makes the first tile
 * written by any instance pay for a full check.
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
  // `Cache.keys()` yields insertion order, so the head is the oldest. FIFO
  // rather than LRU: tracking access times would mean a write per read.
  await Promise.all(keys.slice(0, excess).map((k) => cache.delete(k)));
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

self.addEventListener("install", () => {
  // Deliberately empty. Precaching here would let a flaky network fail the
  // installation, and `skipWaiting()` would swap the controller under a running
  // page. `checkForUpdate()` on activate installs the shell; the swap waits for
  // the reload prompt (see the `message` handler).
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    (async () => {
      // Claim the registering page so the update channel works on the first
      // visit rather than only after a reload.
      await self.clients.claim();
      await trimTiles().catch(() => {});
      try {
        await checkForUpdate({ force: true });
      } catch {
        // First visit while offline. The next navigation tries again.
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
      // Order matters: `respondWith` runs `serveNavigation`, which reads the
      // meta pointer and pins the new client to it, before the probe below can
      // move that pointer underneath it.
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
      // "network": no `respondWith`, so this worker is not in the request path
      // at all. Every weather-data request ends here.
      return;
  }
});

self.addEventListener("message", (event) => {
  const type = event.data && event.data.type;
  if (type === "rustdar:skip-waiting") {
    // The reload prompt was accepted, so the page is about to be torn down and
    // replacing the controller is safe.
    self.skipWaiting();
  } else if (type === "rustdar:check-update") {
    // A long-lived tab returning to the foreground never navigates, so this is
    // its only chance to notice a deploy.
    event.waitUntil(checkForUpdate({ force: true }).catch(() => {}));
  } else if (type === "rustdar:force-update") {
    event.waitUntil(
      forceReinstall().catch((e) => console.warn("rustdar sw: forced reinstall failed:", e)),
    );
  }
});

/*
 * Test hook. `tests/sw_routing.test.mjs` loads these shipped bytes into a scope
 * modelling a ServiceWorkerGlobalScope and drives the handlers directly;
 * `tests/sw_behaviour.rs` runs that suite under `cargo test`.
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
