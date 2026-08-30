/*
 * squallar's service worker: shell cached, weather data never cached.
 *
 * "Never" is literal — the worker does not call `respondWith()` for weather
 * data, so no code here *can* write it to a cache. Those fetches fail offline
 * and index.html shows a banner. Routing is default-deny, so a new entry in
 * `squallar_radar::sources::DataSources` is uncached with no change here.
 *
 * The PMTiles archives (basemap + terrain) are the only basemap now -- the
 * CartoDB raster tiles and their cache were deleted. They are read by `Range`,
 * and the Cache API can neither store a 206 nor match on a range. So their
 * ranged reads are served from a BLOCK cache instead: fixed 64 KiB blocks,
 * each stored as a synthetic-URL 200 -- the 206 prohibition applies to what is
 * STORED, never to what is SERVED -- and the requested span is reassembled as
 * an exact 206. Any archive request that is not a single `bytes=N-M` range
 * still routes "network" untouched, and any error anywhere in the block path
 * falls through to a plain network fetch: a broken cache must never break the
 * map.
 *
 * `__squallar_offline__/` is the offline-area store: the download engine PUTs
 * whole sub-archive segments to app-origin URLs the worker itself is the origin
 * for, and reads them back by `Range`. Deliberate user downloads: nothing here
 * ever evicts one — only a DELETE from the engine removes a segment.
 *
 * Every URL resolves against `ROOT`, so the same bytes work at `/squallar/`
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
 * A client mid-session is never swapped; it gets `squallar:shell-updated` and
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

const META_CACHE = `squallar-meta-v${SW_VERSION}`;
const SHELL_PREFIX = `squallar-shell-v${SW_VERSION}-`;
const ASSET_CACHE = `squallar-assets-v${SW_VERSION}`;

/* Synthetic keys for the meta and client-pin records; nothing is served from them. */
const META_KEY = new URL("__squallar_sw_meta__", ROOT).href;
const PINS_KEY = new URL("__squallar_sw_pins__", ROOT).href;

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
  "pkg/squallar_web.js",
  "pkg/squallar_web_bg.wasm",
  "icons/icon-192.png",
  "icons/icon-512.png",
  "icons/icon-maskable-512.png",
  "icons/apple-touch-icon.png",
  "icons/favicon-32.png",
];

const SHELL_URLS = SHELL_PATHS.map((p) => new URL(p, ROOT).href);
const SHELL_URL_SET = new Set(SHELL_URLS);

/*
 * Same-origin data assets: cached, but deliberately NOT part of the shell.
 *
 * `SHELL_PATHS` is precached with `cache.addAll`, which is all-or-nothing, so
 * one multi-megabyte entry that fails to fetch would take offline support for
 * the whole app down with it. These are fetched on demand by the app, cached on
 * the way past, and served from cache afterwards. A miss costs the app requests
 * to api.weather.gov and nothing else -- `nws::zone_pack_source` treats an
 * absent pack as a supported state.
 *
 * They also live in a cache of their own rather than the shell's, so a deploy
 * does not re-download them: the shell is refetched whenever the validator
 * token moves, which is every push to main, and the pack changes only when the
 * NWS publishes a new zone edition.
 */
const ASSET_PATHS = ["zones.pack"];
const ASSET_URL_SET = new Set(ASSET_PATHS.map((p) => new URL(p, ROOT).href));

/*
 * The assets whose HTTP validators stand in for "which deploy is this". Two,
 * because a deploy has two independent halves: the wasm bundle, and the shell
 * around it. A probe that watched only the wasm never saw a deploy that changed
 * index.html without rebuilding the module — and navigations are cache-first,
 * so the old shell was served indefinitely. The directory index is the asset a
 * shell-side deploy is most certain to touch (it inlines the CSS and the
 * bootstrap); an icon-only deploy still slips through, which is what
 * `squallarForceUpdate()` is for — closing it would cost nine HEADs per check.
 */
const SHELL_VERSION_PROBES = [
  new URL("pkg/squallar_web_bg.wasm", ROOT).href,
  new URL("", ROOT).href,
];

/*
 * Every origin `squallar_source::origins::DataSources::production()` reads from,
 * and nothing else. Belt and braces on top of default-deny, but
 * `tests/pwa_assets.rs` pins the set against the Rust declaration in BOTH
 * directions -- every origin appears here, and every entry here is an origin --
 * so a new data source cannot be added without someone reading this file, and a
 * retired one cannot linger in the list once it is gone from `DataSources`.
 *
 * The basemap archive's host is not here either: `isBasemapArchive` routes it
 * "network" by its own explicit rule below, so listing it here would restate
 * a refusal that already has a named owner.
 */
const NEVER_CACHE_HOSTS = new Set([
  "unidata-nexrad-level2.s3.amazonaws.com",
  "unidata-nexrad-level2-chunks.s3.amazonaws.com",
  "unidata-nexrad-level3.s3.amazonaws.com",
  "noaa-hrrr-bdp-pds.s3.amazonaws.com",
  "noaa-goes19.s3.amazonaws.com",
  "noaa-goes18.s3.amazonaws.com",
  "noaa-mrms-pds.s3.amazonaws.com",
  "noaa-gmgsi-pds.s3.amazonaws.com",
  "api.weather.gov",
  "www.spc.noaa.gov",
  "mesonet.agron.iastate.edu",
  "api.open-meteo.com",
]);

/*
 * The host serving the self-hosted PMTiles v3 basemap archive, as
 * `squallar_egui::tiles::BASEMAP_ARCHIVE_URL` names it. `tests/pwa_assets.rs`
 * pins this string against that const, so the two cannot drift.
 */
const BASEMAP_ARCHIVE_HOST = "tiles.squallar.app";

/*
 * The archives whose ranged reads the block cache serves, exactly as the Rust
 * consts name them: `squallar_egui::tiles::BASEMAP_ARCHIVE_URL`,
 * `::TERRAIN_ARCHIVE_URL`, `::HEIGHT_ARCHIVE_URL` and
 * `::CONUS_HEIGHT_ARCHIVE_URL`. `tests/pwa_assets.rs` pins this list against
 * all four consts in BOTH directions, so a regenerated archive cannot ship
 * without this file moving with it — which is what lets `cachesToKeep` below
 * treat these four generations as current and purge every other one.
 *
 * The two terrain-RGB entries carry `UNPUBLISHED-GENERATION` because no
 * terrain-RGB archive has been built yet. They are listed anyway: the list is
 * what `cachesToKeep` KEEPS, so an archive that arrives before this file does
 * is an archive whose blocks are purged on every deploy.
 */
const ARCHIVE_URLS = [
  "https://tiles.squallar.app/basemap/omt-20260828.pmtiles",
  "https://tiles.squallar.app/terrain/7c94bc6966ab-20260829/squallar-terrain-hillshade.pmtiles",
  "https://tiles.squallar.app/terrain-rgb/UNPUBLISHED-GENERATION/squallar-terrain-terrain-rgb.pmtiles",
  "https://tiles.squallar.app/terrain-rgb-conus/UNPUBLISHED-GENERATION/squallar-terrain-terrain-rgb.pmtiles",
];

/*
 * The block cache. Deliberately NOT versioned by SW_VERSION: the whole point
 * is that browsed areas survive redeploys, and the generation in the name —
 * derived from the archive's own URL path, which moves whenever an archive is
 * regenerated — is the only invalidation an immutable archive needs.
 */
const BLOCK_CACHE_PREFIX = "squallar-blk-";

/* Fixed quantum for archive reads. A tile read is typically 1-3 blocks. */
const BLOCK_BYTES = 64 * 1024;

/* Total block-cache budget across both archives; oldest-touched evict first. */
const BLOCK_CACHE_MAX_BYTES = 256 * 1024 * 1024;

/*
 * Headroom `navigator.storage.estimate()` must show before a block is written.
 * Under this, writes degrade to pass-through — the tile still serves from the
 * bytes in hand, nothing is stored, and nothing can fail for lack of quota.
 */
const BLOCK_STORAGE_FLOOR_BYTES = 64 * 1024 * 1024;

/* The block manifest's synthetic key, the `PINS_KEY` way. */
const BLOCK_MANIFEST_KEY = new URL("__squallar_blk_manifest__", ROOT).href;

/*
 * The offline-area store: whole sub-archive segments the user deliberately
 * downloaded, PUT by the download engine under `__squallar_offline__/`.
 * Deliberately NOT generation-keyed, unlike `squallar-blk-<gen>`: a segment is
 * a self-contained sub-archive that outlives archive generations by design.
 * And deliberately outside every trim path: the block cache's budget and
 * eviction never touch it — a deliberate download is removed by a DELETE and
 * by nothing else.
 */
const OFFLINE_CACHE = "squallar-offline-v1";

/* The store's app-origin directory. Nothing navigates here; the worker is the
 * origin for every URL under it. */
const OFFLINE_DIR = new URL("__squallar_offline__/", ROOT);

/* Reserved GET-only endpoints under the store directory; never stored. */
const OFFLINE_LIST_URL = new URL("__list__", OFFLINE_DIR).href;
const OFFLINE_QUOTA_URL = new URL("__quota__", OFFLINE_DIR).href;

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

/**
 * A PMTiles archive file (monolith form). Every request the reader issues
 * carries a `Range`, so every raw response is a `206`, and `Cache.put()` is
 * specified to reject a `Response` whose status is not 200-299 *and* explicitly
 * throws a `TypeError` on 206 — which is why the block cache stores aligned
 * blocks as synthetic 200s and never the archive response itself. Even if a 206
 * stored, `Cache.match()` ignores `Range` entirely: the first stored range
 * would be handed back for every subsequent offset, a corrupt archive rather
 * than a slow one.
 *
 * The extension is load-bearing rather than cosmetic: the same host serves
 * `/status/latest.json`, and a rule matching the bare host would also refuse a
 * navigation if squallar were ever served from it.
 */
function isBasemapArchive(url) {
  return normalizeHost(url.hostname) === BASEMAP_ARCHIVE_HOST && /\.pmtiles$/i.test(url.pathname);
}

/**
 * A published part of an archive: `<archive>.pmtiles.partNNN`, as
 * `squallar_egui::basemap_archive::HttpRangeSource::part_url` spells it. A
 * distinct predicate because a part does NOT end in `.pmtiles`, so
 * `isBasemapArchive` never matches one.
 */
function isArchivePart(url) {
  return (
    normalizeHost(url.hostname) === BASEMAP_ARCHIVE_HOST &&
    /\.pmtiles\.part\d{3,}$/i.test(url.pathname)
  );
}

/** Any file the archive reader range-reads: the monolith, or one of its parts. */
function isArchiveBlockSource(url) {
  return isBasemapArchive(url) || isArchivePart(url);
}

/**
 * Parse a `Range` header that is a single `bytes=N-M` — the one form the
 * reader emits (`basemap_archive.rs`: inclusive both ends, chosen because it
 * is the CORS-safelisted shape). Anything else — absent, multi-range, suffix
 * (`bytes=-500`), open-ended (`bytes=0-`), inverted — answers `null`, and the
 * request passes to the network untouched.
 */
function parseSingleRange(value) {
  if (typeof value !== "string") return null;
  const match = /^bytes=(\d+)-(\d+)$/.exec(value);
  if (!match) return null;
  const start = Number(match[1]);
  const end = Number(match[2]);
  if (!Number.isSafeInteger(start) || !Number.isSafeInteger(end) || start > end) return null;
  return { start, end };
}

/**
 * The generation one archive file belongs to: its URL path with any `.partNNN`
 * suffix stripped, so a part and its monolith share a generation. The path
 * carries the publish date (`omt-20260828`, `7c94bc6966ab-20260829`), so a
 * regenerated archive IS a new generation — a wrong key here would silently
 * serve one generation's bytes as another's.
 */
function archiveGeneration(pathname) {
  return pathname.replace(/\.part\d{3,}$/i, "");
}

/** The Cache Storage name holding `generation`'s blocks. */
function blockCacheName(generation) {
  return BLOCK_CACHE_PREFIX + encodeURIComponent(generation);
}

/** The block caches for the archives this deploy reads — the ones kept. */
function currentBlockCacheNames() {
  return ARCHIVE_URLS.map((url) => blockCacheName(archiveGeneration(new URL(url).pathname)));
}

/**
 * The synthetic key one stored block is put under — the `PINS_KEY` precedent:
 * a URL under the app origin that nothing ever navigates to. Keyed by
 * generation AND file basename because parts are byte-addressed per file: block
 * 0 of `.part001` is not block 0 of `.part000`.
 */
function blockKey(generation, basename, index) {
  return new URL(
    `__squallar_blk__/${encodeURIComponent(generation)}/${encodeURIComponent(basename)}/${index}`,
    ROOT,
  ).href;
}

function isShellAsset(url) {
  // Query and fragment dropped so a cache-busted `?v=2` still resolves.
  return SHELL_URL_SET.has(url.origin + url.pathname);
}

function isDataAsset(url) {
  return ASSET_URL_SET.has(url.origin + url.pathname);
}

/**
 * The offline-area store's directory. An explicit predicate, the
 * `isBasemapArchive` way, because default-deny masks a deleted rule: the
 * suite asserts on this function directly, not just on routes that happen to
 * come out "network". The trailing slash in `OFFLINE_DIR.pathname` makes this
 * directory containment — `__squallar_offline__x` does not match.
 */
function isOfflineStore(url) {
  return url.origin === OFFLINE_DIR.origin && url.pathname.startsWith(OFFLINE_DIR.pathname);
}

/**
 * Classify one request. The whole caching policy is this function.
 *
 * Returns one of:
 *   "network"       - the worker must not touch it. No `respondWith`, no cache.
 *   "navigate"      - a top-level navigation; answer from the cached shell index.
 *   "shell"         - a named app-shell asset.
 *   "asset"         - a named same-origin data asset, cached outside the shell.
 *   "archive-block" - a single-range read of a PMTiles archive, served
 *                     block-wise from the block cache.
 *   "offline-store" - a download-engine request under `__squallar_offline__/`;
 *                     the worker is the origin, for GET, PUT and DELETE.
 *
 * The block caches are the reason `cachesToKeep` names every current
 * generation: every `squallar-`-prefixed cache it does not name is deleted by
 * `purgeCaches`, so a cache added without a keep entry is a cache emptied on
 * every deploy — browsed areas surviving a redeploy is the entire point.
 *
 * The purge trigger is a DEPLOY, not an activate, and the difference is worth
 * having written down because the obvious test gets it wrong. `checkForUpdate`
 * returns early when the validator token has not moved, so an activate that
 * finds the same deploy never reaches `purgeCaches` at all — seeding a stray
 * cache and re-activating leaves it untouched. `sw_routing.test.mjs` publishes
 * a real deploy for exactly this reason. (Stale block GENERATIONS are the
 * exception: `purgeStaleBlockCaches` runs on every activate as well.)
 *
 * Takes a plain `{url, method, mode, range}` rather than a `Request` so it is
 * callable from a test harness that has no `Request` constructor. `range` is
 * the request's `Range` header value, or null/undefined without one.
 */
function routeFor({ url, method = "GET", mode = "no-cors", range = null }) {
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

  // Nothing squallar issues carries credentials, so their presence means the
  // request was not built by this application — and a cache entry keyed by a URL
  // containing a password is a bad thing to own regardless of host.
  if (u.username || u.password) return "network";

  // First, so no later rule can reach a data origin. Not redundant with
  // default-deny: it stops the two rules that *do* say yes from matching a
  // weather origin if squallar is ever served from one, behind a proxy or on a
  // shared host.
  if (isWeatherDataHost(u.hostname)) return "network";

  // The offline-area store owns its directory for exactly the engine's three
  // verbs — PUT and DELETE are how segments arrive and leave, which is why
  // this precedes the GET-only rule. A navigation here is not the engine and
  // stays out of the store.
  if (isOfflineStore(u)) {
    if (mode === "navigate") return "network";
    if (method === "GET" || method === "PUT" || method === "DELETE") return "offline-store";
    return "network";
  }

  // Only GET is ever cacheable; beyond the store's verbs squallar issues
  // nothing else.
  if (method !== "GET") return "network";

  // Before the rules that say yes, for the same reason the deny list is: a raw
  // range response cannot be cached, so nothing downstream may try. An archive
  // read that is exactly the single-range form the reader emits is served
  // block-wise; anything else touching an archive file — no `Range`, a
  // multi-range, a navigation — reaches the network untouched. Written down
  // rather than left to default-deny because "the default happened to refuse
  // it" and "we decided it must never be cached raw" look identical from
  // outside and only the second survives someone adding a rule above.
  if (isArchiveBlockSource(u)) {
    return parseSingleRange(range) !== null ? "archive-block" : "network";
  }

  // `ROOT.pathname` ends in a slash, so this is directory containment, not a
  // prefix match: `/squallar/` does not match `/squallar-old/x`. Without it a
  // user-site deploy at `https://<user>.github.io/` would answer navigations for
  // every other project on that origin with squallar's index.html. Worker scope
  // confines this today; scope is not the thing to rely on.
  if (u.origin === ROOT.origin && u.pathname.startsWith(ROOT.pathname)) {
    if (mode === "navigate") return "navigate";
    if (isShellAsset(u)) return "shell";
    if (isDataAsset(u)) return "asset";
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
  // ASSET_CACHE is kept for the reason it exists: `purgeCaches` deletes every
  // `squallar-` cache not named here, so leaving it out would re-download the
  // zone pack on every deploy -- which is exactly the cost a cache outside the
  // shell was chosen to avoid. OFFLINE_CACHE is kept for the stronger form of
  // the same hazard: it holds DELIBERATE user downloads that nothing refetches
  // on its own, so an unlisted entry would not cost a re-download — it would
  // silently destroy the user's offline areas on the next deploy.
  const keep = new Set([META_CACHE, ASSET_CACHE, OFFLINE_CACHE, newShellName]);
  // The current archive generations' block caches. Without these entries every
  // deploy would silently empty the block cache — the symptom is a slow map,
  // never an error — because `purgeCaches` deletes every `squallar-` cache not
  // named here. Stale generations are exactly the ones NOT named, and the same
  // purge is what retires them.
  for (const name of currentBlockCacheNames()) keep.add(name);
  for (const name of clientShells.values()) keep.add(name);
  for (const name of Object.values(await readPins())) keep.add(name);
  return keep;
}

async function purgeCaches(keep) {
  const names = await caches.keys();
  await Promise.all(
    names
      .filter((n) => n.startsWith("squallar-") && !keep.has(n))
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
          `squallar sw: the shell version probe has failed ${probeFailures} ` +
            `times in a row (${e}). The cached shell cannot be checked for ` +
            `updates and will keep being served. If this is not simply an ` +
            `offline device, post {type:"squallar:force-update"} to this ` +
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
          'squallar sw: the server sends neither ETag nor Last-Modified for the ' +
            "wasm module or the directory index, so a new deploy cannot be " +
            "detected and the cached shell is pinned. Post " +
            '{type:"squallar:force-update"} to reinstall it.',
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
    if (meta) await notifyClients({ type: "squallar:shell-updated", token });
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
    console.warn("squallar sw: forced reinstall could not probe the version:", e);
  }
  if (token === null) token = meta ? meta.token : "unversioned";

  const name = `${shellCacheName(token)}-forced-${Date.now()}`;
  await installShell(token, name);
  await purgeCaches(await cachesToKeep(name));
  if (meta) await notifyClients({ type: "squallar:shell-updated", token });
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

/*
 * Cache-first, and that is the whole policy: these assets are content the NWS
 * republishes on a schedule of months, and the app asks for one once per
 * session. No revalidation, because a HEAD per session on a file that changes
 * quarterly costs more than it saves; a new edition arrives with a deploy that
 * bumps SW_VERSION, or by the app fetching a URL this cache has never seen.
 *
 * A failure rejects, exactly as an uncontrolled fetch would, so the app sees an
 * ordinary network error and falls back to resolving zones over HTTP.
 */
async function serveAsset(event) {
  const cache = await caches.open(ASSET_CACHE);
  const hit = await cache.match(event.request);
  if (hit) return hit;

  const response = await fetch(event.request);
  // `put` on a partial or opaque response would poison the cache with
  // something the app cannot parse, and a rejected pack is a silent
  // fallback to the fan-out this asset exists to remove.
  if (response && response.ok && response.status === 200) {
    await cache.put(event.request, response.clone());
  }
  return response;
}

// ---------------------------------------------------------------------------
// Archive block cache
// ---------------------------------------------------------------------------

/** The request's `Range` header, read defensively: a navigation `event.request`
 * is a plain object with no `headers` at all. */
function requestRange(request) {
  const headers = request && request.headers;
  if (!headers || typeof headers.get !== "function") return null;
  return headers.get("range");
}

/** Which blocks a span touches: inclusive indexes, both edges. */
function blockSpan(span) {
  return {
    first: Math.floor(span.start / BLOCK_BYTES),
    last: Math.floor(span.end / BLOCK_BYTES),
  };
}

/** The `total` of a `Content-Range: bytes N-M/total`, or null for `*`/absent. */
function archiveTotalBytes(value) {
  const match = typeof value === "string" && /^bytes \d+-\d+\/(\d+)$/.exec(value);
  return match ? Number(match[1]) : null;
}

/*
 * The manifest bounding the block caches: `{clock, blocks: {key: {cache,
 * bytes, touched}}}`, stored the `readPins` way. `touched` is a tick of
 * `clock`, not wall time, so eviction order cannot tie. Memoised like
 * `metaPromise`; read-hit touches mutate the memo only and are persisted by
 * the next block write — lost on a worker restart, which degrades
 * oldest-touched toward oldest-written, a bounded approximation rather than a
 * wrong answer.
 */
let blockManifestPromise = null;

function blockManifest() {
  if (!blockManifestPromise) blockManifestPromise = readBlockManifest();
  return blockManifestPromise;
}

async function readBlockManifest() {
  const cache = await caches.open(META_CACHE);
  const stored = await cache.match(BLOCK_MANIFEST_KEY);
  if (stored) {
    try {
      const manifest = await stored.json();
      if (manifest && typeof manifest.blocks === "object" && manifest.blocks !== null) {
        if (!Number.isSafeInteger(manifest.clock)) manifest.clock = 0;
        return manifest;
      }
    } catch {
      // Unreadable is the same as absent; rebuilt below if blocks exist.
    }
  }
  return { clock: 0, blocks: {} };
}

async function writeBlockManifest(manifest) {
  const cache = await caches.open(META_CACHE);
  await cache.put(
    BLOCK_MANIFEST_KEY,
    new Response(JSON.stringify(manifest), {
      headers: { "content-type": "application/json" },
    }),
  );
}

/** Mark a served block as freshly used. Memo-only; see `blockManifest`. */
async function touchBlock(key) {
  const manifest = await blockManifest();
  const entry = manifest.blocks[key];
  if (entry) entry.touched = ++manifest.clock;
}

/**
 * Evict oldest-touched blocks until the manifest's total is within `cap`.
 * Deletion failures are swallowed entry by entry: the budget is bookkeeping,
 * and bookkeeping must never fail a tile.
 */
async function enforceBlockBudget(cap, manifest = null) {
  const standalone = manifest === null;
  if (standalone) manifest = await blockManifest();

  let total = 0;
  for (const entry of Object.values(manifest.blocks)) total += entry.bytes;
  if (total <= cap) return;

  const oldestFirst = Object.entries(manifest.blocks).sort(
    (a, b) => a[1].touched - b[1].touched,
  );
  for (const [key, entry] of oldestFirst) {
    if (total <= cap) break;
    try {
      // `caches.open` creates on demand; never manufacture a purged cache.
      if (await caches.has(entry.cache)) await (await caches.open(entry.cache)).delete(key);
    } catch {
      // The entry still leaves the books; a re-fetch overwrites the orphan.
    }
    delete manifest.blocks[key];
    total -= entry.bytes;
  }
  if (standalone) await writeBlockManifest(manifest);
}

/** Whether writing `bytes` more would crowd the origin's storage quota. */
async function storageIsTight(bytes) {
  const storage = self.navigator && self.navigator.storage;
  if (!storage || typeof storage.estimate !== "function") return false;
  try {
    const { usage = 0, quota = 0 } = (await storage.estimate()) || {};
    if (!quota) return false;
    return quota - usage < bytes + BLOCK_STORAGE_FLOOR_BYTES;
  } catch {
    return false;
  }
}

/**
 * Store one fetched block as a synthetic-URL 200 and put it on the books.
 * Every failure is absorbed: the caller already holds the bytes it will serve,
 * and a broken cache must never break the map.
 */
async function storeBlock(cache, cacheName, key, blob, total) {
  try {
    if (await storageIsTight(blob.size)) return;
    const headers = {
      "content-type": "application/octet-stream",
      "x-squallar-block-bytes": String(blob.size),
    };
    if (total !== null) headers["x-squallar-archive-bytes"] = String(total);
    await cache.put(key, new Response(blob, { status: 200, headers }));

    const manifest = await blockManifest();
    manifest.blocks[key] = { cache: cacheName, bytes: blob.size, touched: ++manifest.clock };
    await enforceBlockBudget(BLOCK_CACHE_MAX_BYTES, manifest);
    await writeBlockManifest(manifest);
  } catch {
    // Pass-through degradation, not failure.
  }
}

/**
 * One aligned block of `url`: Cache Storage hit, else a real ranged fetch of
 * exactly that block. Only a `206` is ever stored — a `200` here carries the
 * whole archive (the planet basemap is ~125 GB) and is thrown without reading
 * its body so the outer fallback can hand the origin's own answer to the
 * reader untouched; a 206 cannot be opaque (an opaque response reads status 0),
 * so nothing CORS-unreadable can be stored either.
 */
async function archiveBlock(event, cache, cacheName, generation, basename, index) {
  const key = blockKey(generation, basename, index);

  const hit = await cache.match(key);
  if (hit) {
    event.waitUntil(touchBlock(key).catch(() => {}));
    const declared = hit.headers.get("x-squallar-archive-bytes");
    return { blob: await hit.blob(), total: declared === null ? null : Number(declared) };
  }

  const start = index * BLOCK_BYTES;
  const response = await fetch(event.request.url, {
    headers: { range: `bytes=${start}-${start + BLOCK_BYTES - 1}` },
  });
  if (response.status !== 206) {
    // Fire-and-forget: releasing the connection is advisory, and nothing may
    // wait on a body that could be the whole archive.
    if (response.body && typeof response.body.cancel === "function") {
      response.body.cancel().catch(() => {});
    }
    throw new Error(`${basename} block ${index}: HTTP ${response.status}, not 206`);
  }

  const total = archiveTotalBytes(response.headers.get("content-range"));
  const blob = await response.blob();
  event.waitUntil(storeBlock(cache, cacheName, key, blob, total));
  return { blob, total };
}

/**
 * Serve a single-range archive read from the block cache.
 *
 * The answer must be EXACT: the wasm reader errors `RangeRequestsUnsupported`
 * on any status but 206 and `ResponseBodyTooLong` on a body longer than it
 * asked for, so the status, the `Content-Range`, and the body length are all
 * load-bearing. Blocks are walked in order and the walk stops at a short block
 * — that is the file ending — so a span past EOF clamps exactly as an origin
 * would clamp it. A span starting past EOF throws, and the fallback lets the
 * origin answer it (a 416) itself.
 */
async function archiveBlockRange(event) {
  const url = new URL(event.request.url);
  const span = parseSingleRange(requestRange(event.request));
  if (span === null) throw new Error("archive-block routed without a single range");

  const generation = archiveGeneration(url.pathname);
  const cacheName = blockCacheName(generation);
  const basename = url.pathname.slice(url.pathname.lastIndexOf("/") + 1);
  const cache = await caches.open(cacheName);

  const { first, last } = blockSpan(span);
  const pieces = [];
  let total = null;
  let size = 0;

  for (let index = first; index <= last; index += 1) {
    const block = await archiveBlock(event, cache, cacheName, generation, basename, index);
    if (block.total !== null) total = block.total;

    const blockStart = index * BLOCK_BYTES;
    const from = Math.max(span.start - blockStart, 0);
    const to = Math.min(span.end + 1 - blockStart, block.blob.size);
    if (to > from) {
      // `Blob.slice` is a lazy view, not a copy, in both Firefox and Chromium.
      pieces.push(block.blob.slice(from, to));
      size += to - from;
    }
    if (block.blob.size < BLOCK_BYTES) break;
  }

  if (size === 0) throw new Error(`bytes=${span.start}-${span.end} starts past the end of ${basename}`);

  const end = span.start + size - 1;
  return new Response(new Blob(pieces), {
    status: 206,
    headers: {
      "content-type": "application/octet-stream",
      "content-length": String(size),
      "content-range": `bytes ${span.start}-${end}/${total === null ? "*" : total}`,
    },
  });
}

/**
 * The outermost layer of the block path, and deliberately so: ANY error —
 * a broken cache, a quota rejection, an origin refusing ranges — falls through
 * to a plain network fetch of the original request, which is exactly what the
 * worker not existing would have produced.
 */
async function serveArchiveBlock(event) {
  try {
    return await archiveBlockRange(event);
  } catch {
    return fetch(event.request);
  }
}

// ---------------------------------------------------------------------------
// Offline-area store
// ---------------------------------------------------------------------------

/*
 * Ask the page(s) to request durable storage, once per worker lifetime, on the
 * first stored segment. It has to be the page: the Storage Standard exposes
 * `StorageManager.persist()` as `[Exposed=Window]` — Window only — while
 * `persisted()` and `estimate()` are `Exposed=(Window,Worker)`. So the worker
 * checks `persisted()` here and index.html answers `squallar:request-persist`
 * by calling `persist()`.
 */
let persistRequested = false;

async function requestPersistence() {
  if (persistRequested) return;
  persistRequested = true;
  try {
    const storage = self.navigator && self.navigator.storage;
    if (storage && typeof storage.persisted === "function" && (await storage.persisted())) return;
  } catch {
    // Unknown reads as not-persisted: asking the page again is harmless.
  }
  await notifyClients({ type: "squallar:request-persist" });
}

/** `{usage, quota}` from `navigator.storage.estimate()`; null means UNKNOWN.
 * Never 0 — a zero quota would read as "nothing fits", which is a fabrication
 * the download engine would act on. */
async function offlineQuota() {
  let usage = null;
  let quota = null;
  try {
    const storage = self.navigator && self.navigator.storage;
    if (storage && typeof storage.estimate === "function") {
      const estimate = (await storage.estimate()) || {};
      if (Number.isFinite(estimate.usage)) usage = estimate.usage;
      if (Number.isFinite(estimate.quota)) quota = estimate.quota;
    }
  } catch {
    // Answered as unknown.
  }
  return new Response(JSON.stringify({ usage, quota }), {
    status: 200,
    headers: { "content-type": "application/json" },
  });
}

/**
 * `{url, bytes}` for every stored segment, enumerated from the cache itself —
 * never from a stored manifest, which could say "complete" over a cache a
 * half-landed PUT left short. On web this listing IS the filesystem fact the
 * launch-time completeness recomputation reads. The byte count is the header
 * `Cache.put` wrote atomically with the body; a missing or mangled header
 * falls back to reading the blob's own size rather than guessing.
 */
async function offlineList() {
  const cache = await caches.open(OFFLINE_CACHE);
  const segments = [];
  for (const request of await cache.keys()) {
    const url = typeof request === "string" ? request : request.url;
    const stored = await cache.match(url);
    if (!stored) continue;
    const declared = Number(stored.headers.get("x-squallar-segment-bytes"));
    const bytes =
      Number.isSafeInteger(declared) && declared >= 0 ? declared : (await stored.blob()).size;
    segments.push({ url, bytes });
  }
  return new Response(JSON.stringify(segments), {
    status: 200,
    headers: { "content-type": "application/json" },
  });
}

/**
 * The offline-area store's request handler.
 *
 *   PUT    <dir>/{area}/{seg}.pmtiles   store the whole body; 200.
 *   GET    same, no Range               the stored 200, whole.
 *   GET    same, `bytes=N-M`            exact 206 via lazy `Blob.slice` — the
 *                                       archive-block route's technique, and
 *                                       the same exactness contract: the wasm
 *                                       reader errors on any other status and
 *                                       on an over-long body.
 *   DELETE same                         remove one segment; 404 if absent.
 *   DELETE <dir>/{area}/                remove every segment of the area.
 *
 * Segments are stored as synthetic 200s because `Cache.put()` throws on a 206
 * by spec; the 206 is synthesized at serve time. A span past EOF clamps, and
 * one starting past EOF answers 416, exactly as an origin would.
 */
async function offlineStore(event) {
  const u = new URL(event.request.url);
  const key = u.origin + u.pathname;
  const method = event.request.method;

  if (key === OFFLINE_LIST_URL || key === OFFLINE_QUOTA_URL) {
    // GET-only endpoints, and never stored: a PUT accepted here would become a
    // phantom row in the very listing it landed in.
    if (method !== "GET") return new Response(null, { status: 405 });
    return key === OFFLINE_QUOTA_URL ? offlineQuota() : offlineList();
  }

  if (method === "PUT") {
    // A directory is not a segment; the whole-area form exists only for DELETE.
    if (u.pathname.endsWith("/")) return new Response(null, { status: 405 });
    const blob = await event.request.blob();
    const cache = await caches.open(OFFLINE_CACHE);
    await cache.put(
      key,
      new Response(blob, {
        status: 200,
        headers: {
          "content-type": "application/octet-stream",
          "content-length": String(blob.size),
          "x-squallar-segment-bytes": String(blob.size),
        },
      }),
    );
    event.waitUntil(requestPersistence().catch(() => {}));
    return new Response(null, { status: 200 });
  }

  if (method === "DELETE") {
    const cache = await caches.open(OFFLINE_CACHE);
    if (u.pathname.endsWith("/")) {
      // The bare directory would name every area at once; refuse it.
      if (key === OFFLINE_DIR.href) return new Response(null, { status: 405 });
      let removed = 0;
      for (const request of await cache.keys()) {
        const stored = typeof request === "string" ? request : request.url;
        if (stored.startsWith(key) && (await cache.delete(stored))) removed += 1;
      }
      // 200 even at zero: removing an absent area is the state it asks for.
      return new Response(JSON.stringify({ removed }), {
        status: 200,
        headers: { "content-type": "application/json" },
      });
    }
    const removed = await cache.delete(key);
    return new Response(null, { status: removed ? 200 : 404 });
  }

  const cache = await caches.open(OFFLINE_CACHE);
  const hit = await cache.match(key);
  if (!hit) return new Response(null, { status: 404 });

  const span = parseSingleRange(requestRange(event.request));
  // No Range — or not the single `bytes=N-M` form, which an origin is also
  // free to ignore with a full 200.
  if (span === null) return hit;

  const blob = await hit.blob();
  if (span.start >= blob.size) {
    return new Response(null, {
      status: 416,
      headers: { "content-range": `bytes */${blob.size}` },
    });
  }
  const end = Math.min(span.end, blob.size - 1);
  // `Blob.slice` is a lazy view, not a copy, in both Firefox and Chromium.
  const piece = blob.slice(span.start, end + 1);
  return new Response(piece, {
    status: 206,
    headers: {
      "content-type": "application/octet-stream",
      "content-length": String(piece.size),
      "content-range": `bytes ${span.start}-${end}/${blob.size}`,
    },
  });
}

/**
 * Unlike the archive path there is no fallback here: the worker IS the origin
 * for these URLs, so an error must read as one. A PUT answered 200 with
 * nothing stored is a segment the launch-time completeness recomputation would
 * count as downloaded — the silent partial success this store exists to
 * prevent.
 */
async function serveOfflineStore(event) {
  try {
    return await offlineStore(event);
  } catch (e) {
    return new Response(String(e), { status: 500 });
  }
}

/**
 * Retire the block caches of generations this deploy no longer reads, and
 * their manifest rows. Runs on EVERY activate — unlike `purgeCaches`, which
 * only a deploy triggers — because a stale generation is dead weight the
 * moment the worker's own bytes (which carry `ARCHIVE_URLS`) change.
 *
 * Also rebuilds an empty manifest from what the caches actually hold (an
 * SW_VERSION bump renames META_CACHE and orphans the books): without this the
 * budget would read zero over a full cache and never evict.
 */
async function purgeStaleBlockCaches() {
  const keep = new Set(currentBlockCacheNames());
  const names = await caches.keys();
  await Promise.all(
    names
      .filter((n) => n.startsWith(BLOCK_CACHE_PREFIX) && !keep.has(n))
      .map((n) => caches.delete(n)),
  );

  const manifest = await blockManifest();
  let dirty = false;
  for (const [key, entry] of Object.entries(manifest.blocks)) {
    if (!keep.has(entry.cache)) {
      delete manifest.blocks[key];
      dirty = true;
    }
  }

  if (Object.keys(manifest.blocks).length === 0) {
    for (const name of keep) {
      if (!(await caches.has(name))) continue;
      const cache = await caches.open(name);
      for (const request of await cache.keys()) {
        const key = typeof request === "string" ? request : request.url;
        const stored = await cache.match(key);
        if (!stored) continue;
        const bytes = Number(stored.headers.get("x-squallar-block-bytes"));
        manifest.blocks[key] = {
          cache: name,
          bytes: Number.isSafeInteger(bytes) ? bytes : BLOCK_BYTES,
          touched: ++manifest.clock,
        };
        dirty = true;
      }
    }
  }

  if (dirty) {
    await enforceBlockBudget(BLOCK_CACHE_MAX_BYTES, manifest);
    await writeBlockManifest(manifest);
  }
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
      await purgeStaleBlockCaches().catch(() => {});
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
    range: requestRange(event.request),
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
    case "asset":
      event.respondWith(serveAsset(event));
      return;
    case "archive-block":
      event.respondWith(serveArchiveBlock(event));
      return;
    case "offline-store":
      event.respondWith(serveOfflineStore(event));
      return;
    default:
      // "network": no `respondWith`, so this worker is not in the request path
      // at all. Every weather-data request ends here.
      return;
  }
});

self.addEventListener("message", (event) => {
  const type = event.data && event.data.type;
  if (type === "squallar:skip-waiting") {
    // The reload prompt was accepted, so the page is about to be torn down and
    // replacing the controller is safe.
    self.skipWaiting();
  } else if (type === "squallar:check-update") {
    // A long-lived tab returning to the foreground never navigates, so this is
    // its only chance to notice a deploy.
    event.waitUntil(checkForUpdate({ force: true }).catch(() => {}));
  } else if (type === "squallar:force-update") {
    event.waitUntil(
      forceReinstall().catch((e) => console.warn("squallar sw: forced reinstall failed:", e)),
    );
  }
});

/*
 * Test hook. `tests/sw_routing.test.mjs` loads these shipped bytes into a scope
 * modelling a ServiceWorkerGlobalScope and drives the handlers directly;
 * `tests/sw_behaviour.rs` runs that suite under `cargo test`.
 */
self.__squallarSwInternals = {
  ROOT,
  SHELL_URLS,
  ASSET_PATHS,
  ASSET_CACHE,
  META_CACHE,
  isDataAsset,
  NEVER_CACHE_HOSTS,
  SHELL_PREFIX,
  routeFor,
  isWeatherDataHost,
  BASEMAP_ARCHIVE_HOST,
  isBasemapArchive,
  ARCHIVE_URLS,
  BLOCK_BYTES,
  BLOCK_CACHE_PREFIX,
  BLOCK_CACHE_MAX_BYTES,
  isArchivePart,
  isArchiveBlockSource,
  parseSingleRange,
  archiveGeneration,
  blockCacheName,
  currentBlockCacheNames,
  blockKey,
  blockSpan,
  archiveTotalBytes,
  enforceBlockBudget,
  purgeStaleBlockCaches,
  isShellAsset,
  normalizeHost,
  validatorToken,
  OFFLINE_CACHE,
  OFFLINE_LIST_URL,
  OFFLINE_QUOTA_URL,
  isOfflineStore,
};
