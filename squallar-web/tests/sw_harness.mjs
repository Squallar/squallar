/*
 * A ServiceWorkerGlobalScope faithful enough to run `sw.js` unmodified.
 *
 * ============================================================================
 * Why this exists rather than a re-implementation
 * ============================================================================
 *
 * The service worker's whole job is a policy decision — which requests may be
 * cached and which must never be. A test that restated that policy in its own
 * code would assert only that the test agrees with itself. So this harness
 * loads the *shipped bytes* of `../sw.js`, compiles them, and runs them against
 * a scope that behaves like a browser's. Every assertion in
 * `sw_routing.test.mjs` is therefore against the code that is deployed.
 *
 * The loader is `new Function`, not `vm.createContext`. A `vm` realm gets its
 * own `URL`, `Response` and `Request` constructors, so cross-realm values
 * compare and behave subtly differently from the ones the worker would see. A
 * function whose parameters shadow exactly the four globals a service worker
 * gets from its scope (`self`, `caches`, `fetch`, `clients`) leaves everything
 * else — `URL`, `Response`, `Headers`, `Set`, `Date` — as Node's real
 * WHATWG implementations, which are the same specifications the browser
 * implements.
 *
 * ============================================================================
 * What is modelled, and where the model stops
 * ============================================================================
 *
 * Modelled because the worker's correctness depends on it:
 *
 *   * `Cache.addAll` is all-or-nothing and rejects on any non-2xx, writing
 *     nothing. The shell's atomicity claim rests entirely on this.
 *   * `Cache.keys()` yields insertion order, which is what `trimTiles` calls
 *     FIFO eviction.
 *   * `Cache.match(request, {ignoreSearch})` strips the query string.
 *   * `caches.open` creates on demand; `caches.keys()` lists what exists.
 *   * A `FetchEvent` is answered by `respondWith` or not at all, and the
 *     difference is observable — `handled` below is how a test proves the
 *     worker stayed out of the request path.
 *   * The worker can be torn down and restarted with its caches intact, which
 *     is what makes module-level state (`tilePutsSinceTrim`, the memoised
 *     shell cache) testable rather than theoretical.
 *
 * Not modelled, deliberately:
 *
 *   * Real HTTP. `Network` below is a routing table the test writes.
 *   * `WebAssembly.instantiateStreaming` timing, and therefore the true wall
 *     width of the update race. The tests drive that race with explicit
 *     deferred responses instead, which makes it deterministic rather than
 *     probabilistic — a stronger gate, but not a substitute for having watched
 *     it happen in a browser.
 *   * Opaque responses are *simulated* by overriding `type` on a real
 *     `Response`. A genuine `no-cors` response also hides its headers; the
 *     `opaqueResponse` helper hides them too, so the worker cannot tell.
 *
 * A navigation request is a plain object rather than a `Request`. That is not
 * laziness: the Fetch specification forbids constructing a `Request` with mode
 * `"navigate"`, so no test in any harness can build one. `sw.js` reads exactly
 * `.url`, `.method` and `.mode` off `event.request` before routing, and passes
 * the object on to `cache.match` and `fetch`, both of which are this file's.
 */

import { readFileSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

const SW_SOURCE_URL = new URL("../sw.js", import.meta.url);

export const SW_PATH = fileURLToPath(SW_SOURCE_URL);

/** The shipped worker source, read once per test process. */
let swSourceCache = null;
export async function swSource() {
  if (swSourceCache === null) swSourceCache = await readFile(SW_SOURCE_URL, "utf8");
  return swSourceCache;
}

// ---------------------------------------------------------------------------
// Cache API
// ---------------------------------------------------------------------------

/** Strip the query and fragment, which is what `{ignoreSearch: true}` means. */
function withoutSearch(href) {
  const u = new URL(href);
  u.search = "";
  u.hash = "";
  return u.href;
}

function requestUrl(input) {
  return typeof input === "string" ? input : input.url;
}

class MemoryCache {
  /**
   * Holds the storage rather than the network, so that a restarted worker —
   * which is handed the same storage and may be handed a different network —
   * cannot end up with caches fetching from the previous test's server.
   */
  constructor(storage) {
    this.storage = storage;
    /** Insertion-ordered, which is what `Cache.keys()` guarantees. */
    this.entries = new Map();
  }

  async put(request, response) {
    // The real Cache API rejects these, and the worker relies on it: a 206 or a
    // non-2xx must never become a cache entry that is later served as if it
    // were the asset.
    if (response.status === 206) throw new TypeError("Cache.put: 206 not allowed");
    if (response.status !== 0 && !(response.status >= 200 && response.status <= 299)) {
      throw new TypeError(`Cache.put: response status ${response.status} not allowed`);
    }
    this.entries.set(requestUrl(request), { request, response });
  }

  async match(request, options = {}) {
    const wanted = requestUrl(request);
    const entry = this.entries.get(wanted);
    if (entry) return entry.response.clone();
    if (!options.ignoreSearch) return undefined;
    const bare = withoutSearch(wanted);
    for (const stored of this.entries.values()) {
      if (withoutSearch(requestUrl(stored.request)) === bare) return stored.response.clone();
    }
    return undefined;
  }

  async keys() {
    return [...this.entries.values()].map((e) => e.request);
  }

  async delete(request) {
    return this.entries.delete(requestUrl(request));
  }

  /**
   * All-or-nothing, exactly as the specification requires: every request is
   * fetched, and if any of them fails or answers non-2xx the whole call rejects
   * having written nothing.
   */
  async addAll(requests) {
    const responses = await Promise.all(
      requests.map(async (request) => {
        const response = await this.storage.network.fetch(request);
        if (!response.ok) {
          throw new TypeError(`Cache.addAll: ${requestUrl(request)} answered ${response.status}`);
        }
        return { request, response };
      }),
    );
    for (const { request, response } of responses) {
      this.entries.set(requestUrl(request), { request, response });
    }
  }
}

class MemoryCacheStorage {
  constructor(network) {
    this.network = network;
    /** Creation-ordered, which is what `caches.keys()` yields. */
    this.caches = new Map();
  }

  async open(name) {
    let cache = this.caches.get(name);
    if (!cache) {
      cache = new MemoryCache(this);
      this.caches.set(name, cache);
    }
    return cache;
  }

  async has(name) {
    return this.caches.has(name);
  }

  async keys() {
    return [...this.caches.keys()];
  }

  async delete(name) {
    return this.caches.delete(name);
  }

  async match(request, options) {
    for (const cache of this.caches.values()) {
      const hit = await cache.match(request, options);
      if (hit) return hit;
    }
    return undefined;
  }

  /** Test-only: total entries across every cache whose name matches. */
  countEntries(predicate = () => true) {
    let n = 0;
    for (const [name, cache] of this.caches) if (predicate(name)) n += cache.entries.size;
    return n;
  }

  /**
   * Test-only: every URL stored anywhere, with the cache that holds it.
   *
   * This is what lets a test assert the negative that matters — that no weather
   * origin appears in *any* cache — rather than the much weaker claim that the
   * one cache it thought to look in is clean.
   */
  allEntries() {
    const out = [];
    for (const [name, cache] of this.caches) {
      for (const url of cache.entries.keys()) out.push({ cache: name, url });
    }
    return out;
  }
}

// ---------------------------------------------------------------------------
// Network
// ---------------------------------------------------------------------------

/** A `Response` that hides its headers, the way a `no-cors` fetch does. */
export function opaqueResponse(body = "tile") {
  const response = new Response(body, { status: 200 });
  Object.defineProperty(response, "type", { value: "opaque" });
  Object.defineProperty(response, "status", { value: 0 });
  Object.defineProperty(response, "ok", { value: false });
  Object.defineProperty(response, "headers", { value: new Headers() });
  return response;
}

/**
 * A scriptable origin server.
 *
 * Handlers are keyed by URL without the query string. `offline` fails every
 * request the way a dead uplink does. `log` records what the worker actually
 * asked for, which is how a test proves a request never left the machine.
 */
export class Network {
  constructor() {
    this.handlers = new Map();
    this.log = [];
    this.offline = false;
    /** Per-URL gate: resolve the returned function to release the response. */
    this.deferred = new Map();
  }

  /** `handler` is a function of the Request, or a plain Response-ish value. */
  serve(url, handler) {
    this.handlers.set(withoutSearch(url), handler);
    return this;
  }

  /**
   * Hold responses for `url` until the returned `release()` is called.
   *
   * This is how the update race is made deterministic: the test decides exactly
   * when the new shell's wasm module arrives, so the interleaving under test is
   * the one that ran, not the one that happened to run.
   */
  hold(url) {
    let release;
    const gate = new Promise((resolve) => {
      release = resolve;
    });
    this.deferred.set(withoutSearch(url), gate);
    return release;
  }

  async fetch(input, init = {}) {
    const url = requestUrl(input);
    const method = (typeof input === "string" ? init.method : input.method) || "GET";
    this.log.push({ url, method });

    if (this.offline) throw new TypeError(`NetworkError: failed to fetch ${url}`);

    const key = withoutSearch(url);
    const gate = this.deferred.get(key);
    if (gate) await gate;

    const handler = this.handlers.get(key);
    if (handler === undefined) return new Response("not found", { status: 404 });

    const value = typeof handler === "function" ? await handler(input, init, method) : handler;
    if (value instanceof Error) throw value;
    // Every caller gets its own body, as a real network would give it.
    return value.clone();
  }

  /** URLs the worker asked for, deduplicated, in first-request order. */
  requested() {
    return [...new Set(this.log.map((e) => e.url))];
  }
}

// ---------------------------------------------------------------------------
// Events and clients
// ---------------------------------------------------------------------------

class ExtendableEvent {
  constructor() {
    this.pending = [];
  }

  waitUntil(promise) {
    this.pending.push(Promise.resolve(promise));
  }

  /** Settle everything `waitUntil` was handed, the way the browser does. */
  async settle() {
    // Handlers registered during a `waitUntil` are themselves awaited, which is
    // what lets a probe kick off an install that the test can then observe.
    while (this.pending.length) {
      const batch = this.pending;
      this.pending = [];
      await Promise.allSettled(batch);
    }
  }
}

export class FetchEvent extends ExtendableEvent {
  constructor(request, { clientId = "", resultingClientId = "" } = {}) {
    super();
    this.request = request;
    this.clientId = clientId;
    this.resultingClientId = resultingClientId;
    /** Undefined until `respondWith` is called: the worker stayed out of it. */
    this.response = undefined;
    this.handled = false;
  }

  respondWith(promise) {
    if (this.handled) throw new DOMException("respondWith called twice", "InvalidStateError");
    this.handled = true;
    this.response = Promise.resolve(promise);
  }

  /**
   * Settle the response as well as the `waitUntil` work.
   *
   * The browser does not deliver a response until the promise given to
   * `respondWith` resolves, so anything that promise does — such as recording
   * which shell generation a navigation pinned — has completed by the time the
   * page sees the response. Awaiting only `waitUntil` here let a test observe
   * the worker mid-`respondWith`, which is a state no page can be in, and made
   * assertions depend on how many microtask turns the test happened to take.
   *
   * Rejections are swallowed: a fetch that fails is an ordinary outcome the
   * tests assert on directly.
   */
  async settle() {
    if (this.response) await this.response.catch(() => {});
    await super.settle();
  }
}

class WindowClient {
  constructor(id, url) {
    this.id = id;
    this.url = url;
    this.type = "window";
    this.messages = [];
  }

  postMessage(message) {
    this.messages.push(message);
  }
}

// ---------------------------------------------------------------------------
// The scope
// ---------------------------------------------------------------------------

/**
 * Build a scope and run the shipped worker in it.
 *
 * `swUrl` is the URL the worker script itself was served from, which is the
 * only thing `ROOT` is derived from. Passing a weather-data origin here is how
 * the deny list is shown to be load-bearing rather than decorative.
 *
 * Pass `storage` from a previous scope to model a worker that was killed for
 * idleness and restarted: the caches survive, every module-level variable in
 * `sw.js` does not.
 */
export async function startWorker({
  swUrl = "https://squallar.example/squallar/sw.js",
  network = new Network(),
  storage = null,
  source = null,
} = {}) {
  const caches = storage ?? new MemoryCacheStorage(network);
  caches.network = network;

  const listeners = new Map();
  const clients = [];
  let nextClientId = 1;

  const scope = {
    location: { href: swUrl },
    registration: { scope: new URL("./", swUrl).href, waiting: null, installing: null },
    skipWaitingCalled: false,
    skipWaiting() {
      scope.skipWaitingCalled = true;
      return Promise.resolve();
    },
    addEventListener(type, handler) {
      if (!listeners.has(type)) listeners.set(type, []);
      listeners.get(type).push(handler);
    },
    clients: {
      claimed: false,
      async claim() {
        scope.clients.claimed = true;
      },
      async matchAll() {
        return clients;
      },
      async get(id) {
        return clients.find((c) => c.id === id);
      },
    },
  };

  const warnings = [];
  const consoleShim = {
    ...console,
    warn: (...args) => {
      warnings.push(args.map(String).join(" "));
    },
  };

  const src = source ?? (await swSource());
  // The four bindings a service worker gets from its global scope, and nothing
  // else. `URL`, `Response`, `Headers`, `Set`, `Date` and the rest resolve to
  // Node's real implementations, so the worker sees the same semantics a
  // browser would give it.
  const load = new Function(
    "self",
    "caches",
    "fetch",
    "clients",
    "console",
    `${src}\n;return self.__squallarSwInternals;`,
  );

  const boundFetch = (input, init) => network.fetch(input, init);
  const internals = load(scope, caches, boundFetch, scope.clients, consoleShim);

  async function dispatch(type, event) {
    for (const handler of listeners.get(type) ?? []) await handler(event);
    await event.settle();
    return event;
  }

  return {
    scope,
    caches,
    network,
    internals,
    warnings,
    listenerTypes: () => [...listeners.keys()],

    addClient(url = new URL("./", swUrl).href) {
      const client = new WindowClient(`client-${nextClientId++}`, url);
      clients.push(client);
      return client;
    },
    removeClient(client) {
      const i = clients.indexOf(client);
      if (i >= 0) clients.splice(i, 1);
    },
    clients: () => clients,

    activate: () => dispatch("activate", new ExtendableEvent()),
    install: () => dispatch("install", new ExtendableEvent()),

    /** Dispatch a fetch and return the event, settled. */
    fetch(request, options) {
      return dispatch("fetch", new FetchEvent(request, options));
    },

    message(data) {
      const event = new ExtendableEvent();
      event.data = data;
      return dispatch("message", event);
    },

    /**
     * A top-level navigation. Not a `Request`: mode `"navigate"` cannot be
     * constructed, so this is the shape the worker actually reads.
     */
    navigation(url, options = {}) {
      return { url, method: "GET", mode: "navigate", ...options };
    },

    /** The cache names the worker has created, in creation order. */
    cacheNames: () => caches.keys(),

    /** Every URL stored in every cache. */
    cachedUrls: () => caches.allEntries().map((e) => e.url),
    cachedEntries: () => caches.allEntries(),
  };
}

// ---------------------------------------------------------------------------
// A deployment to serve
// ---------------------------------------------------------------------------

/**
 * The shell `sw.js` precaches, relative to the deploy directory — read out of
 * `sw.js` itself.
 *
 * This list used to be written out here as well. That is a deploy the tests
 * cannot see: `installShell` calls `cache.addAll`, which is all-or-nothing, so
 * a shell entry the harness does not publish makes every install fail — 18
 * suites at once, all of them pointing at caching rather than at the list. And
 * the inverse is worse and silent: an entry dropped from `sw.js` but left here
 * still gets served, so the suite keeps passing for an asset that is no longer
 * precached.
 *
 * The extraction is the same shape as the one `build.yaml` uses to check the
 * staged tree, and `pwa_assets.rs`'s: a flat list of double-quoted strings.
 * Anything else throws here rather than yielding a plausible short list.
 */
export const SHELL_ASSETS = readShellPaths(readFileSync(SW_SOURCE_URL, "utf8"));

function readShellPaths(src) {
  const list = /const SHELL_PATHS = \[([^\]]*)\]/.exec(src);
  if (list === null) {
    throw new Error("sw.js no longer declares `const SHELL_PATHS = [...]`");
  }
  const paths = [...list[1].matchAll(/"([^"]*)"/g)].map((m) => m[1]);
  if (paths.length < 2) {
    throw new Error(
      `SHELL_PATHS in sw.js parsed as ${paths.length} entries; the extraction is wrong`,
    );
  }
  return paths;
}

/**
 * Publish a deploy tagged `tag` at `origin`.
 *
 * Every asset answers with its own path and the tag, so a test can read a
 * response body and say which deploy it came from — which is the whole of how
 * shell atomicity is asserted. `HEAD` answers an `ETag` of the tag, which is
 * what `probeValidator` turns into the shell's version token.
 */
export function publishDeploy(network, origin, tag, { headStatus = 200 } = {}) {
  for (const asset of SHELL_ASSETS) {
    const url = new URL(asset, origin).href;
    network.serve(url, (request, init, method) => {
      if (method === "HEAD") {
        if (headStatus !== 200) return new Response(null, { status: headStatus });
        return new Response(null, { status: 200, headers: { etag: `"${tag}"` } });
      }
      return new Response(`${asset}::${tag}`, {
        status: 200,
        headers: { etag: `"${tag}"`, "content-type": "text/plain" },
      });
    });
  }
  return network;
}

/**
 * Publish a deploy that changed only the directory index, leaving the wasm
 * module — and every other asset — exactly as `baseTag` served them. This is
 * the shape of a shell-side deploy: index.html edited, the `wasm-pack` output
 * untouched, so the module's validator does not move. A probe watching only
 * the wasm sees nothing here; detecting this deploy is what the second probe
 * asset exists for.
 */
export function publishIndexOnlyDeploy(network, origin, baseTag, indexTag) {
  publishDeploy(network, origin, baseTag);
  network.serve(new URL("", origin).href, (request, init, method) =>
    method === "HEAD"
      ? new Response(null, { status: 200, headers: { etag: `"${indexTag}"` } })
      : new Response(`::${indexTag}`, {
          status: 200,
          headers: { etag: `"${indexTag}"`, "content-type": "text/plain" },
        }),
  );
  return network;
}

/** A deploy that publishes no HTTP validators at all. */
export function publishUnversionedDeploy(network, origin, tag) {
  for (const asset of SHELL_ASSETS) {
    network.serve(new URL(asset, origin).href, (request, init, method) =>
      method === "HEAD"
        ? new Response(null, { status: 200 })
        : new Response(`${asset}::${tag}`, { status: 200 }),
    );
  }
  return network;
}

/** Restart the worker against caches that survived, as an idle kill does. */
export function restartWorker(worker, overrides = {}) {
  return startWorker({
    swUrl: worker.scope.location.href,
    network: worker.network,
    storage: worker.caches,
    ...overrides,
  });
}
