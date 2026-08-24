/*
 * Behavioural gates on the inline bootstrap script in `index.html`.
 *
 * ============================================================================
 * Why this is not covered by reading the file
 * ============================================================================
 *
 * `pwa_assets.rs` asserts that `index.html` contains the string
 * `addEventListener("offline"` and an element with `id="squallar-offline"`. Both
 * of those survive the change that matters. Turn one assignment around —
 *
 *     offlineBanner.hidden = navigator.onLine !== false;   // shows the banner
 *     offlineBanner.hidden = true;                         // never shows it
 *
 * — and the listener is still registered, the element still exists, the text
 * gate still passes, and the banner never appears again. For an application
 * that deliberately caches no weather data, that banner is the only thing
 * distinguishing "the sky is clear" from "this feed died twenty minutes ago".
 * It needs a test that runs the script.
 *
 * ============================================================================
 * The harness
 * ============================================================================
 *
 * The bootstrap is a classic-script IIFE, so it can be compiled with its four
 * free globals — `window`, `document`, `navigator`, `console` — passed as
 * parameters, the same technique `sw_harness.mjs` uses on the worker. What runs
 * is the shipped `index.html`'s own bytes.
 *
 * `getElementById` deliberately returns `null` for an id that is not in the
 * markup, rather than inventing an element. That makes the DOM stub a gate in
 * its own right: rename the banner's id in the HTML without updating the
 * script and these tests throw, which is the failure a browser would give.
 */

import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { describe, it } from "node:test";

const INDEX_URL = new URL("../index.html", import.meta.url);

let htmlCache = null;
async function indexHtml() {
  if (htmlCache === null) htmlCache = await readFile(INDEX_URL, "utf8");
  return htmlCache;
}

/**
 * The bootstrap script: the first `<script>` with no attributes.
 *
 * The module script that boots wasm is `<script type="module">` and is not this
 * one. The distinction is load-bearing in the page itself — the bootstrap runs
 * even when the wasm module fails to parse, which is exactly when a user most
 * needs the offline banner — so it is worth the test being precise about which
 * script it is running.
 */
function extractBootstrap(html) {
  const match = /<script>([\s\S]*?)<\/script>/.exec(html);
  assert.ok(match, "index.html has no attribute-free <script> block");
  return match[1];
}

/** Every element id in the markup, with whether it carries the `hidden` attribute. */
function elementsInMarkup(html) {
  const found = new Map();
  const re = /<([a-zA-Z]+)\b([^>]*)\bid="([^"]+)"([^>]*)>/g;
  let m;
  while ((m = re.exec(html)) !== null) {
    const attrs = `${m[2]} ${m[4]}`;
    found.set(m[3], { tag: m[1], hidden: /(^|\s)hidden(\s|=|$)/.test(attrs) });
  }
  return found;
}

class StubElement {
  constructor(id, hidden) {
    this.id = id;
    this.hidden = hidden;
    this.listeners = new Map();
  }

  addEventListener(type, handler) {
    if (!this.listeners.has(type)) this.listeners.set(type, []);
    this.listeners.get(type).push(handler);
  }

  /** Fire a DOM event at this element. */
  click() {
    for (const handler of this.listeners.get("click") ?? []) handler({ type: "click" });
  }
}

class EventTargetStub {
  constructor() {
    this.listeners = new Map();
  }

  addEventListener(type, handler) {
    if (!this.listeners.has(type)) this.listeners.set(type, []);
    this.listeners.get(type).push(handler);
  }

  dispatch(type, event = {}) {
    for (const handler of this.listeners.get(type) ?? []) handler({ type, ...event });
  }

  has(type) {
    return (this.listeners.get(type) ?? []).length > 0;
  }
}

/** Compile and run the shipped bootstrap against a controllable page. */
async function runBootstrap({ onLine = true, serviceWorker = true } = {}) {
  const html = await indexHtml();
  const markup = elementsInMarkup(html);

  const elements = new Map();
  for (const [id, { hidden }] of markup) elements.set(id, new StubElement(id, hidden));

  const documentStub = Object.assign(new EventTargetStub(), {
    visibilityState: "visible",
    getElementById: (id) => elements.get(id) ?? null,
  });

  const windowStub = new EventTargetStub();
  const reloads = [];
  windowStub.location = { reload: () => reloads.push(Date.now()) };

  const registration = Object.assign(new EventTargetStub(), {
    waiting: null,
    installing: null,
    updates: 0,
    update() {
      registration.updates += 1;
      return Promise.resolve();
    },
  });

  const posted = [];
  const swContainer = Object.assign(new EventTargetStub(), {
    controller: null,
    registered: [],
    registerResult: Promise.resolve(registration),
    register(url) {
      swContainer.registered.push(url);
      return swContainer.registerResult;
    },
  });

  const navigatorStub = { onLine };
  if (serviceWorker) navigatorStub.serviceWorker = swContainer;

  const warnings = [];
  const infos = [];
  const consoleStub = {
    ...console,
    warn: (...a) => warnings.push(a.map(String).join(" ")),
    info: (...a) => infos.push(a.map(String).join(" ")),
  };

  const run = new Function("window", "document", "navigator", "console", extractBootstrap(html));
  run(windowStub, documentStub, navigatorStub, consoleStub);

  return {
    window: windowStub,
    document: documentStub,
    navigator: navigatorStub,
    serviceWorker: swContainer,
    registration,
    element: (id) => elements.get(id),
    markup,
    reloads,
    posted,
    warnings,
    infos,
    /** Give the browser a turn, so a `.then` on registration settles. */
    settle: () => new Promise((resolve) => setImmediate(resolve)),
    /**
     * Fire `load` and let registration settle.
     *
     * The page holds its `registration` only from the `.then` on
     * `register()`, which runs on `load`. A test that skips this has a null
     * registration, takes the "no waiting worker" branch of every decision
     * below, and passes for a reason it did not intend.
     */
    async load() {
      windowStub.dispatch("load");
      await new Promise((resolve) => setImmediate(resolve));
    },
    /** Make a worker that records what the page posts to it. */
    worker: () => ({ postMessage: (message) => posted.push(message) }),
  };
}

// ===========================================================================
describe("offline banner", () => {
  // =========================================================================

  it("is hidden in the markup, so it cannot flash on a healthy load", async () => {
    const page = await runBootstrap();
    assert.equal(
      page.markup.get("squallar-offline")?.hidden,
      true,
      "the offline banner must ship hidden and be revealed by script",
    );
  });

  it("is showing immediately when the page loads with no network", async () => {
    const page = await runBootstrap({ onLine: false });
    assert.equal(
      page.element("squallar-offline").hidden,
      false,
      "the page loaded offline and said nothing about it",
    );
  });

  it("stays hidden when the page loads online", async () => {
    const page = await runBootstrap({ onLine: true });
    assert.equal(page.element("squallar-offline").hidden, true);
  });

  it("appears when the connection drops and clears when it returns", async () => {
    const page = await runBootstrap({ onLine: true });
    const banner = page.element("squallar-offline");

    page.navigator.onLine = false;
    page.window.dispatch("offline");
    assert.equal(banner.hidden, false, "the connection dropped and the banner did not appear");

    page.navigator.onLine = true;
    page.window.dispatch("online");
    assert.equal(banner.hidden, true, "the connection returned and the banner did not clear");
  });

  it("listens for both connectivity events", async () => {
    const page = await runBootstrap();
    assert.equal(page.window.has("offline"), true);
    assert.equal(page.window.has("online"), true);
  });
});

// ===========================================================================
describe("update prompt", () => {
  // =========================================================================

  it("appears only when the worker says a new shell is cached", async () => {
    const page = await runBootstrap();
    const banner = page.element("squallar-update");
    assert.equal(banner.hidden, true, "the update prompt must start hidden");

    page.serviceWorker.dispatch("message", { data: { type: "squallar:shell-updated" } });
    assert.equal(banner.hidden, false);
  });

  it("ignores messages it does not recognise", async () => {
    const page = await runBootstrap();
    page.serviceWorker.dispatch("message", { data: { type: "something-else" } });
    page.serviceWorker.dispatch("message", { data: null });
    assert.equal(page.element("squallar-update").hidden, true);
  });

  it("can be dismissed", async () => {
    const page = await runBootstrap();
    page.serviceWorker.dispatch("message", { data: { type: "squallar:shell-updated" } });
    page.element("squallar-update-dismiss").click();
    assert.equal(page.element("squallar-update").hidden, true);
  });

  it("reloads when accepted and no new worker is parked", async () => {
    const page = await runBootstrap();
    await page.load();
    page.serviceWorker.dispatch("message", { data: { type: "squallar:shell-updated" } });
    page.element("squallar-update-reload").click();
    assert.equal(page.reloads.length, 1, "accepting the update did not load the new shell");
  });

  it("lets a parked worker through and reloads only once it has taken over", async () => {
    const page = await runBootstrap();
    await page.load();

    const waiting = page.worker();
    page.registration.waiting = waiting;
    page.serviceWorker.dispatch("message", { data: { type: "squallar:shell-updated" } });
    page.element("squallar-update-reload").click();

    assert.deepEqual(
      page.posted,
      [{ type: "squallar:skip-waiting" }],
      "the waiting worker was never told to take over",
    );
    assert.equal(page.reloads.length, 0, "the page reloaded before the new worker was in control");

    page.serviceWorker.dispatch("controllerchange");
    assert.equal(page.reloads.length, 1);
  });
});

// ===========================================================================
describe("controllerchange", () => {
  // =========================================================================

  it("does not reload on the first visit's clients.claim()", async () => {
    // `controllerchange` fires both when a waiting worker takes over and when
    // the worker's own `clients.claim()` promotes a page that had no
    // controller. Reloading on the second means every first visit downloads the
    // ~10 MB module, throws it away, and downloads it again.
    const page = await runBootstrap();
    await page.settle();
    page.serviceWorker.dispatch("controllerchange");
    assert.equal(
      page.reloads.length,
      0,
      "a first-visit claim triggered a reload; the whole bundle is fetched twice",
    );
  });

  it("reloads at most once even if it fires repeatedly", async () => {
    const page = await runBootstrap();
    await page.load();
    page.registration.waiting = page.worker();
    page.element("squallar-update-reload").click();

    page.serviceWorker.dispatch("controllerchange");
    page.serviceWorker.dispatch("controllerchange");
    page.serviceWorker.dispatch("controllerchange");
    assert.equal(page.reloads.length, 1);
  });
});

// ===========================================================================
describe("registration", () => {
  // =========================================================================

  it("registers ./sw.js after load, relatively", async () => {
    const page = await runBootstrap();
    assert.deepEqual(page.serviceWorker.registered, [], "registration must wait for load");

    await page.load();
    assert.deepEqual(
      page.serviceWorker.registered,
      ["./sw.js"],
      "the leading ./ is what scopes the worker to the deploy directory",
    );
  });

  it("survives a registration failure without taking the page with it", async () => {
    const page = await runBootstrap();
    page.serviceWorker.registerResult = Promise.reject(new Error("insecure origin"));
    await page.load();
    assert.equal(
      page.warnings.some((w) => w.includes("service worker registration failed")),
      true,
    );
  });

  it("still paints connectivity in a browser with no service workers at all", async () => {
    const page = await runBootstrap({ onLine: false, serviceWorker: false });
    assert.equal(
      page.element("squallar-offline").hidden,
      false,
      "the offline banner must not depend on service worker support",
    );
  });
});

// ===========================================================================
describe("foreground update check", () => {
  // =========================================================================

  it("asks the worker to look for a deploy when the tab comes back", async () => {
    const page = await runBootstrap();
    await page.load();

    const controller = page.worker();
    page.serviceWorker.controller = controller;
    page.document.dispatch("visibilitychange");

    assert.deepEqual(page.posted, [{ type: "squallar:check-update" }]);
    assert.equal(page.registration.updates, 1, "the worker script itself was never re-checked");
  });

  it("throttles, so flipping between tabs is not a poll", async () => {
    const page = await runBootstrap();
    await page.load();
    page.serviceWorker.controller = page.worker();

    for (let i = 0; i < 5; i += 1) page.document.dispatch("visibilitychange");
    assert.equal(page.posted.length, 1, "the foreground check is not throttled");
  });

  it("does nothing when the tab is going away rather than arriving", async () => {
    const page = await runBootstrap();
    await page.load();
    page.serviceWorker.controller = page.worker();

    page.document.visibilityState = "hidden";
    page.document.dispatch("visibilitychange");
    assert.deepEqual(page.posted, []);
  });
});

// ===========================================================================
describe("forced update escape hatch", () => {
  // =========================================================================

  it("posts the force message to the controlling worker", async () => {
    // The way out of a version probe that has stopped working. Without it, a
    // server that starts refusing HEAD pins this app to the deploy that was
    // current when it broke, indefinitely and silently.
    const page = await runBootstrap();
    page.serviceWorker.controller = page.worker();

    assert.equal(typeof page.window.squallarForceUpdate, "function", "no forced-update entry point");
    assert.equal(page.window.squallarForceUpdate(), true);
    assert.deepEqual(page.posted, [{ type: "squallar:force-update" }]);
  });

  it("says so rather than throwing when no worker is in control", async () => {
    const page = await runBootstrap();
    page.serviceWorker.controller = null;
    assert.equal(page.window.squallarForceUpdate(), false);
    assert.equal(
      page.warnings.some((w) => w.includes("no service worker")),
      true,
    );
  });
});
