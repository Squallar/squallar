/*
 * Behavioural gates on the shipped service worker.
 *
 * ============================================================================
 * What these tests are for
 * ============================================================================
 *
 * `rustdar-web/tests/pwa_assets.rs` reads `sw.js` as text and checks that the
 * things it *declares* are consistent with the Rust origin declaration and with
 * the files on disk. That catches drift. It cannot catch anything about what the
 * worker does, because it never runs it — and every rule in `sw.js` that matters
 * is a rule about behaviour:
 *
 *   * a weather-data response must never enter a cache;
 *   * a page load must draw its whole shell from one deploy;
 *   * a navigation must be answered from cache, because a network-first one
 *     pairs a new index.html with a cached wasm module and the page dies;
 *   * the basemap cache must stay bounded across worker restarts.
 *
 * Every test here runs the real `sw.js`, loaded from disk by `sw_harness.mjs`,
 * against a scope that models the browser's. Nothing is re-implemented: if a
 * rule is deleted from the worker, the assertion that depended on it fails.
 *
 * ============================================================================
 * Reading a failure
 * ============================================================================
 *
 * The tests are named for the property, not the mechanism, because the
 * mechanism is allowed to change. A failure in `weather_data` means a request
 * that must have gone to the network did not; a failure in `atomicity` means a
 * page could load half of one deploy and half of another. Both are shipping
 * defects, not test maintenance.
 */

import assert from "node:assert/strict";
import { describe, it } from "node:test";

import {
  Network,
  opaqueResponse,
  publishDeploy,
  publishUnversionedDeploy,
  restartWorker,
  startWorker,
} from "./sw_harness.mjs";

const ORIGIN = "https://rustdar.example/rustdar/";
const SW_URL = `${ORIGIN}sw.js`;

/** The five origins `DataSources::production()` reads from, as URLs in use. */
const PRODUCTION_DATA_URLS = [
  "https://unidata-nexrad-level2.s3.amazonaws.com/2026/07/25/KTLX/KTLX20260725_180000_V06",
  "https://unidata-nexrad-level2-chunks.s3.amazonaws.com/KTLX/397/20260725-180000-001-S",
  "https://unidata-nexrad-level3.s3.amazonaws.com/TLX_N0Q_2026_07_25_18_00",
  "https://noaa-hrrr-bdp-pds.s3.amazonaws.com/hrrr.20260725/conus/hrrr.t18z.wrfsfcf00.grib2",
  "https://noaa-goes19.s3.amazonaws.com/GLM-L2-LCFA/2026/206/18/OR_GLM-L2-LCFA_G19.nc",
  "https://noaa-goes18.s3.amazonaws.com/GLM-L2-LCFA/2026/206/18/OR_GLM-L2-LCFA_G18.nc",
  "https://api.weather.gov/alerts/active?area=OK",
  "https://www.spc.noaa.gov/products/outlook/day1otlk_cat.nolyr.geojson",
  "https://mesonet.agron.iastate.edu/cgi-bin/request/asos.py?station=OKC",
];

const TILE_URL = "https://cartodb-basemaps-a.global.ssl.fastly.net/dark_nolabels/7/29/52.png";

function shellUrl(asset) {
  return new URL(asset, ORIGIN).href;
}

/** A worker with deploy `tag` installed and its first activation complete. */
async function bootWorker({ tag = "A", swUrl = SW_URL, origin = ORIGIN } = {}) {
  const network = new Network();
  publishDeploy(network, origin, tag);
  const worker = await startWorker({ swUrl, network });
  await worker.activate();
  return worker;
}

/** Load a page: navigate, then request the two halves of the wasm bundle. */
async function loadPage(worker, origin = ORIGIN) {
  const client = worker.addClient();
  const nav = await worker.fetch(worker.navigation(origin), {
    clientId: "",
    resultingClientId: client.id,
  });
  const read = async (event) => (event.handled ? (await event.response).text() : null);
  const document = await read(nav);
  const glue = await read(
    await worker.fetch(new Request(shellUrl("pkg/rustdar_web.js")), { clientId: client.id }),
  );
  const wasm = await read(
    await worker.fetch(new Request(shellUrl("pkg/rustdar_web_bg.wasm")), { clientId: client.id }),
  );
  return { client, document, glue, wasm };
}

/** The deploy tag every part of a page load came from, or a description. */
function generationOf({ document, glue, wasm }) {
  const tags = [document, glue, wasm].map((body) => (body === null ? "network" : body.split("::")[1]));
  return new Set(tags);
}

// ===========================================================================
describe("routing: weather data is default-deny", () => {
  // =========================================================================

  it("routes every production data origin to the network, and never to a cache", async () => {
    const worker = await bootWorker();
    const { routeFor } = worker.internals;
    for (const url of PRODUCTION_DATA_URLS) {
      assert.equal(routeFor({ url }), "network", `GET ${url} must not be cacheable`);
      assert.equal(
        routeFor({ url, mode: "navigate" }),
        "network",
        `${url} must not be cacheable even when the browser calls it a navigation`,
      );
      assert.equal(routeFor({ url, method: "POST" }), "network");
    }
  });

  it("cannot be talked into caching a data origin by how the URL is spelled", async () => {
    const worker = await bootWorker();
    const { routeFor } = worker.internals;

    // Each of these has been someone's cache-poisoning bug in some other
    // codebase. The comment is what the spelling is trying to exploit.
    const adversarial = [
      // A fully-qualified name. `new URL` keeps the dot; DNS does not care.
      "https://api.weather.gov./alerts/active",
      "https://mesonet.agron.iastate.edu./cgi-bin/request/asos.py",
      "https://unidata-nexrad-level2.s3.amazonaws.com./KTLX",
      // Case. Hostnames are case-insensitive; string comparisons are not.
      "https://API.WEATHER.GOV/alerts/active",
      "https://Www.Spc.Noaa.Gov/products/outlook/day1otlk.geojson",
      // Suffix confusion in both directions.
      "https://api.weather.gov.evil.example/alerts/active",
      "https://amazonaws.com.evil.example/bucket/key",
      "https://notapi.weather.gov.attacker.test/x",
      // The tile rule's extension, on a data host.
      "https://api.weather.gov/radar/ktlx/reflectivity.png",
      "https://noaa-hrrr-bdp-pds.s3.amazonaws.com/hrrr/t18z.png",
      "https://www.spc.noaa.gov/outlook/day1.PNG",
      // Credentials in the URL.
      "https://user:secret@api.weather.gov/alerts/active",
      // A basemap-shaped path on a host that only looks like CartoDB.
      "https://cartodb-basemaps-a.global.ssl.fastly.net.evil.example/dark/7/29/52.png",
      "https://cartodb-basemaps-e.global.ssl.fastly.net/dark/7/29/52.png",
      "https://evil.example/cartodb-basemaps-a.global.ssl.fastly.net/7/29/52.png",
      // Schemes the Cache API would reject anyway.
      "data:image/png;base64,iVBORw0KGgo=",
      "blob:https://rustdar.example/9f0b",
      "chrome-extension://abcdefghijklmnop/inject.js",
    ];

    for (const url of adversarial) {
      assert.equal(routeFor({ url }), "network", `${url} must route to the network`);
    }
  });

  it("normalises a host before deciding, so the deny list is not spelling-sensitive", async () => {
    const worker = await bootWorker();
    const { isWeatherDataHost } = worker.internals;

    // The mechanism behind the spellings above. Kept separate because the
    // default-deny in `routeFor` would mask a regression here — this is the
    // layer a future rule would lean on, and it has to be sound on its own.
    for (const host of [
      "api.weather.gov.",
      "API.WEATHER.GOV",
      "Api.Weather.Gov.",
      "mesonet.agron.iastate.edu.",
      "unidata-nexrad-level2.s3.amazonaws.com.",
      "WWW.SPC.NOAA.GOV.",
    ]) {
      assert.equal(isWeatherDataHost(host), true, `${host} must be recognised as a data host`);
    }

    for (const host of ["api.weather.gov.evil.example", "weather.gov.attacker.test", "notweather.gov"]) {
      assert.equal(isWeatherDataHost(host), false, `${host} is not a data host`);
    }
  });

  it("keeps the deny list load-bearing even when rustdar is served from a data origin", async () => {
    // The point of this test.
    //
    // `routeFor` is default-deny, so on rustdar's real deploy the
    // NEVER_CACHE_HOSTS check is unobservable: nothing reaches it that the
    // default would not also refuse. Deleting it changes no behaviour, which
    // makes it look like decoration and makes it deletable by someone tidying.
    //
    // It is not decoration. It is the only thing standing between the two rules
    // that *do* cache — a same-origin navigation and a same-origin shell asset
    // — and a weather origin, if rustdar is ever served from one: behind a
    // reverse proxy, on a shared host, or from a mirror. Serving the worker
    // from `api.weather.gov` is how that is made observable.
    const worker = await bootWorker({
      swUrl: "https://api.weather.gov/rustdar/sw.js",
      origin: "https://api.weather.gov/rustdar/",
    });
    const { routeFor } = worker.internals;

    for (const url of [
      "https://api.weather.gov/alerts/active",
      "https://api.weather.gov/rustdar/",
      "https://api.weather.gov/rustdar/pkg/rustdar_web_bg.wasm",
      "https://api.weather.gov/rustdar/manifest.webmanifest",
    ]) {
      assert.equal(
        routeFor({ url, mode: "navigate" }),
        "network",
        `${url} is on a weather-data origin and must never be cached, even though \
it is same-origin with the worker`,
      );
      assert.equal(routeFor({ url }), "network", `${url} must never be cached`);
    }
  });

  it("leaves data requests entirely out of the worker's request path", async () => {
    // Stronger than "responds with a network fetch": the worker must not call
    // respondWith at all, so there is no code path that could write the
    // response anywhere.
    const worker = await bootWorker();
    for (const url of PRODUCTION_DATA_URLS) {
      const event = await worker.fetch(new Request(url));
      assert.equal(
        event.handled,
        false,
        `the worker called respondWith for ${url}; it must stay out of the request path`,
      );
    }
  });
});

// ===========================================================================
describe("routing: the basemap tile rule is confined to CartoDB", () => {
  // =========================================================================

  it("caches CartoDB's four subdomains and nothing else", async () => {
    const worker = await bootWorker();
    const { routeFor } = worker.internals;

    for (const sub of ["a", "b", "c", "d"]) {
      const url = `https://cartodb-basemaps-${sub}.global.ssl.fastly.net/dark_nolabels/7/29/52.png`;
      assert.equal(routeFor({ url }), "tile", `${url} is a basemap tile`);
    }

    // The rule must be the host, not the extension. A `.png` anywhere else is
    // not a tile, and treating it as one puts a fetched image into a cache that
    // is served stale for six months — which, on a weather origin, is the
    // precise harm this worker exists to prevent.
    for (const url of [
      "https://api.weather.gov/radar/ktlx/reflectivity.png",
      "https://tiles.example/dark_nolabels/7/29/52.png",
      "https://rustdar.example/rustdar/screenshot.png",
      "https://cartodb-basemaps-a.global.ssl.fastly.net.evil.example/7/29/52.png",
      "https://cartodb-basemaps-aa.global.ssl.fastly.net/7/29/52.png",
      "https://cartodb-basemaps-a.global.ssl.fastly.net.co/7/29/52.png",
    ]) {
      assert.notEqual(routeFor({ url }), "tile", `${url} must not be treated as a basemap tile`);
    }
  });

  it("refuses a tile URL carrying credentials", async () => {
    const worker = await bootWorker();
    assert.equal(
      worker.internals.routeFor({
        url: "https://user:secret@cartodb-basemaps-a.global.ssl.fastly.net/dark/7/29/52.png",
      }),
      "network",
      "a cache entry keyed by a URL containing a password must not be created",
    );
  });

  it("treats the extension case-insensitively on a host that is genuinely CartoDB", async () => {
    const worker = await bootWorker();
    assert.equal(
      worker.internals.routeFor({
        url: "https://cartodb-basemaps-b.global.ssl.fastly.net/dark_nolabels/7/29/52.PNG",
      }),
      "tile",
    );
  });
});

// ===========================================================================
describe("routing: the shell rule is confined to the deploy directory", () => {
  // =========================================================================

  it("answers navigations only under the directory the worker was served from", async () => {
    const worker = await bootWorker();
    const { routeFor } = worker.internals;

    assert.equal(routeFor({ url: ORIGIN, mode: "navigate" }), "navigate");
    assert.equal(routeFor({ url: `${ORIGIN}?station=KTLX`, mode: "navigate" }), "navigate");
    assert.equal(routeFor({ url: `${ORIGIN}index.html`, mode: "navigate" }), "navigate");

    // A user-site deploy at `https://<user>.github.io/` shares its origin with
    // every other project that user publishes. Answering their navigations with
    // rustdar's index.html would replace those sites with this one.
    for (const url of [
      "https://rustdar.example/somewhere/else",
      "https://rustdar.example/",
      "https://rustdar.example/rustdar-old/index.html",
      "https://rustdar.example/rustdarn't/",
    ]) {
      assert.equal(
        routeFor({ url, mode: "navigate" }),
        "network",
        `${url} is outside the deploy directory and must not be answered from the shell`,
      );
    }
  });

  it("recognises a shell asset regardless of a cache-busting query", async () => {
    const worker = await bootWorker();
    const { routeFor } = worker.internals;
    assert.equal(routeFor({ url: shellUrl("pkg/rustdar_web.js") }), "shell");
    assert.equal(routeFor({ url: `${shellUrl("pkg/rustdar_web.js")}?v=2` }), "shell");
    assert.equal(routeFor({ url: shellUrl("pkg/rustdar_web_bg.wasm") }), "shell");
    assert.equal(routeFor({ url: shellUrl("manifest.webmanifest") }), "shell");
    // Not precached, so not a shell asset: it must reach the network.
    assert.equal(routeFor({ url: shellUrl("pkg/rustdar_web.d.ts") }), "network");
  });

  it("never treats a non-GET as cacheable", async () => {
    const worker = await bootWorker();
    const { routeFor } = worker.internals;
    for (const method of ["POST", "PUT", "DELETE", "HEAD", "OPTIONS"]) {
      assert.equal(routeFor({ url: ORIGIN, method, mode: "navigate" }), "network");
      assert.equal(routeFor({ url: TILE_URL, method }), "network");
      assert.equal(routeFor({ url: shellUrl("pkg/rustdar_web.js"), method }), "network");
    }
  });
});

// ===========================================================================
describe("offline: the shell survives and weather data honestly fails", () => {
  // =========================================================================

  it("serves the whole shell from cache with the network down", async () => {
    const worker = await bootWorker({ tag: "A" });
    worker.network.offline = true;

    const page = await loadPage(worker);
    assert.deepEqual(
      generationOf(page),
      new Set(["A"]),
      "the whole page load must come from the cached shell while offline",
    );
  });

  it("fails every production data origin offline, with nothing left in any cache", async () => {
    const worker = await bootWorker({ tag: "A" });
    await loadPage(worker);
    worker.network.offline = true;

    for (const url of PRODUCTION_DATA_URLS) {
      const event = await worker.fetch(new Request(url));
      // Not handled means the browser does its own networking, which offline
      // means a network error the application surfaces. That is the honest
      // answer, and it is the one the offline banner exists to explain.
      assert.equal(event.handled, false, `${url} must not be answered by the worker`);
    }

    const weatherHosts = PRODUCTION_DATA_URLS.map((u) => new URL(u).hostname);
    for (const { cache, url } of worker.cachedEntries()) {
      const host = new URL(url).hostname;
      assert.equal(
        weatherHosts.includes(host),
        false,
        `${url} is a weather-data URL and it is sitting in ${cache}`,
      );
    }
  });

  it("still answers a shell asset offline for a client that never navigated", async () => {
    const worker = await bootWorker({ tag: "A" });
    worker.network.offline = true;
    const event = await worker.fetch(new Request(shellUrl("pkg/rustdar_web.js")));
    assert.equal(event.handled, true);
    assert.equal(await (await event.response).text(), "pkg/rustdar_web.js::A");
  });
});

// ===========================================================================
describe("shell: navigations and subresources are cache-first", () => {
  // =========================================================================

  it("answers a navigation from cache even when the network has newer bytes", async () => {
    // Network-first here is the specific bug the design exists to prevent: a
    // fresh index.html paired with the cached glue and wasm is a wasm-bindgen
    // version mismatch and a blank page. The shell moves as one, or not at all.
    const worker = await bootWorker({ tag: "A" });
    publishDeploy(worker.network, ORIGIN, "B");

    const client = worker.addClient();
    const event = await worker.fetch(worker.navigation(ORIGIN), { resultingClientId: client.id });

    assert.equal(event.handled, true, "the navigation must be answered by the worker");
    assert.equal(
      await (await event.response).text(),
      "::A",
      "the navigation was answered from the network; it must come from the cached shell",
    );
  });

  it("answers a shell subresource from cache even when the network has newer bytes", async () => {
    const worker = await bootWorker({ tag: "A" });
    publishDeploy(worker.network, ORIGIN, "B");

    const event = await worker.fetch(new Request(shellUrl("pkg/rustdar_web.js")));
    assert.equal(event.handled, true);
    assert.equal(
      await (await event.response).text(),
      "pkg/rustdar_web.js::A",
      "the glue was fetched from the network; a cached wasm module would then not match it",
    );
  });

  it("falls through to the network before any shell exists", async () => {
    const network = new Network();
    publishDeploy(network, ORIGIN, "A");
    const worker = await startWorker({ swUrl: SW_URL, network });
    // No activate: nothing has been installed.
    const event = await worker.fetch(worker.navigation(ORIGIN), { resultingClientId: "c1" });
    assert.equal(await (await event.response).text(), "::A");
  });
});

// ===========================================================================
describe("atomicity: one page load draws its shell from one deploy", () => {
  // =========================================================================

  it("keeps a client on its own generation when a deploy lands mid-load", async () => {
    // Reproduces the failure this pinning exists to fix. Before it, the
    // navigation came from deploy A, the meta pointer moved while the page was
    // still parsing, and the wasm module arrived from deploy B — a
    // wasm-bindgen mismatch, and deploy A had already been deleted so there was
    // no way back.
    const worker = await bootWorker({ tag: "A" });
    const client = worker.addClient();

    const nav = await worker.fetch(worker.navigation(ORIGIN), { resultingClientId: client.id });
    const glue = await worker.fetch(new Request(shellUrl("pkg/rustdar_web.js")), {
      clientId: client.id,
    });

    // Deploy B lands, via the message index.html sends on visibilitychange.
    publishDeploy(worker.network, ORIGIN, "B");
    await worker.message({ type: "rustdar:check-update" });

    const wasm = await worker.fetch(new Request(shellUrl("pkg/rustdar_web_bg.wasm")), {
      clientId: client.id,
    });

    const bodies = {
      document: await (await nav.response).text(),
      glue: await (await glue.response).text(),
      wasm: await (await wasm.response).text(),
    };
    assert.deepEqual(
      generationOf(bodies),
      new Set(["A"]),
      `this page load mixed deploys: ${JSON.stringify(bodies)}`,
    );
  });

  it("keeps a client on its generation when the deploy lands mid-request", async () => {
    // The same race, wound tighter: the install completes while the page's
    // wasm request is already in the worker.
    const worker = await bootWorker({ tag: "A" });
    const client = worker.addClient();
    await worker.fetch(worker.navigation(ORIGIN), { resultingClientId: client.id });

    publishDeploy(worker.network, ORIGIN, "B");
    const release = worker.network.hold(shellUrl("pkg/rustdar_web_bg.wasm"));
    const update = worker.message({ type: "rustdar:check-update" });
    await new Promise((resolve) => setTimeout(resolve, 5));
    release();
    await update;

    const wasm = await worker.fetch(new Request(shellUrl("pkg/rustdar_web_bg.wasm")), {
      clientId: client.id,
    });
    assert.equal(await (await wasm.response).text(), "pkg/rustdar_web_bg.wasm::A");
  });

  it("gives a page that loads after the update the new deploy, whole", async () => {
    // The pin must not become a way of pinning everyone to the old bundle
    // forever. A fresh navigation is a fresh decision.
    const worker = await bootWorker({ tag: "A" });
    const first = worker.addClient();
    await worker.fetch(worker.navigation(ORIGIN), { resultingClientId: first.id });

    publishDeploy(worker.network, ORIGIN, "B");
    await worker.message({ type: "rustdar:check-update" });

    const page = await loadPage(worker);
    assert.deepEqual(generationOf(page), new Set(["B"]));
  });

  it("retains the superseded generation so a worker restart mid-load is survivable", async () => {
    const worker = await bootWorker({ tag: "A" });
    const client = worker.addClient();
    await worker.fetch(worker.navigation(ORIGIN), { resultingClientId: client.id });

    publishDeploy(worker.network, ORIGIN, "B");
    await worker.message({ type: "rustdar:check-update" });

    // The worker is killed for idleness. `clientShells` goes with it; the caches
    // do not.
    const restarted = await restartWorker(worker);
    const names = await restarted.cacheNames();
    assert.equal(
      names.some((n) => n.includes("%22A%22")),
      true,
      `the superseded shell was deleted, so a page still loading it now 404s: ${names}`,
    );
  });

  it("does not accumulate shell generations without limit", async () => {
    const worker = await bootWorker({ tag: "A" });
    for (const tag of ["B", "C", "D", "E"]) {
      publishDeploy(worker.network, ORIGIN, tag);
      await worker.message({ type: "rustdar:check-update" });
    }
    const shells = (await worker.cacheNames()).filter((n) => n.startsWith("rustdar-shell-"));
    assert.ok(
      shells.length <= 2,
      `${shells.length} shell caches are being retained with no client pinned to them: ${shells}`,
    );
  });

  it("prunes the pin when the client goes away", async () => {
    const worker = await bootWorker({ tag: "A" });
    const client = worker.addClient();
    await worker.fetch(worker.navigation(ORIGIN), { resultingClientId: client.id });

    // The tab is closed, then a new one opens and a deploy lands.
    worker.removeClient(client);
    publishDeploy(worker.network, ORIGIN, "B");
    await worker.message({ type: "rustdar:check-update" });

    const second = worker.addClient();
    await worker.fetch(worker.navigation(ORIGIN), { resultingClientId: second.id });
    publishDeploy(worker.network, ORIGIN, "C");
    await worker.message({ type: "rustdar:check-update" });

    const shells = (await worker.cacheNames()).filter((n) => n.startsWith("rustdar-shell-"));
    assert.equal(
      shells.some((n) => n.includes("%22A%22")),
      false,
      `deploy A is still retained for a client that no longer exists: ${shells}`,
    );
  });
});

// ===========================================================================
describe("install: a shell is published whole or not at all", () => {
  // =========================================================================

  it("publishes nothing when one shell asset is missing", async () => {
    const network = new Network();
    publishDeploy(network, ORIGIN, "A");
    // The deploy forgot the manifest — precisely what a Pages workflow that
    // copies `sw.js` but not `manifest.webmanifest` produces.
    network.serve(shellUrl("manifest.webmanifest"), new Response("nope", { status: 404 }));

    const worker = await startWorker({ swUrl: SW_URL, network });
    await worker.activate();

    const names = await worker.cacheNames();
    assert.equal(
      names.some((n) => n.startsWith("rustdar-shell-")),
      false,
      `a shell cache was left behind by a failed install: ${names}`,
    );

    // And the page still works, straight from the network.
    const event = await worker.fetch(worker.navigation(ORIGIN), { resultingClientId: "c1" });
    assert.equal(await (await event.response).text(), "::A");
  });

  it("keeps the working shell when an update fails halfway", async () => {
    const worker = await bootWorker({ tag: "A" });
    publishDeploy(worker.network, ORIGIN, "B");
    worker.network.serve(shellUrl("icons/icon-512.png"), new Response("gone", { status: 500 }));

    await worker.message({ type: "rustdar:check-update" });

    const page = await loadPage(worker);
    assert.deepEqual(
      generationOf(page),
      new Set(["A"]),
      "a failed update must leave the previous complete shell serving",
    );
  });

  it("does not announce the first install as an update", async () => {
    const network = new Network();
    publishDeploy(network, ORIGIN, "A");
    const worker = await startWorker({ swUrl: SW_URL, network });
    const client = worker.addClient();
    await worker.activate();
    assert.deepEqual(
      client.messages,
      [],
      "the first install has nothing to replace; a reload prompt there is a lie",
    );
  });

  it("announces a replacement to every open window", async () => {
    const worker = await bootWorker({ tag: "A" });
    const one = worker.addClient();
    const two = worker.addClient();
    publishDeploy(worker.network, ORIGIN, "B");
    await worker.message({ type: "rustdar:check-update" });

    for (const client of [one, two]) {
      assert.equal(
        client.messages.some((m) => m.type === "rustdar:shell-updated"),
        true,
        "an open window was not told a new version is ready",
      );
    }
  });

  it("re-downloads nothing when the deploy has not changed", async () => {
    const worker = await bootWorker({ tag: "A" });
    const before = worker.network.log.length;
    await worker.message({ type: "rustdar:check-update" });
    const after = worker.network.log.slice(before);
    assert.deepEqual(
      after.map((e) => e.method),
      ["HEAD"],
      `an unchanged deploy cost more than one HEAD: ${JSON.stringify(after)}`,
    );
  });
});

// ===========================================================================
describe("updates: a degraded version probe is visible and escapable", () => {
  // =========================================================================

  it("warns when the probe keeps failing rather than pinning the shell silently", async () => {
    const worker = await bootWorker({ tag: "A" });
    // The server starts refusing HEAD. `checkForUpdate` can no longer tell one
    // deploy from another and correctly declines to act — forever.
    publishDeploy(worker.network, ORIGIN, "B", { headStatus: 405 });

    for (let i = 0; i < 4; i += 1) {
      await worker.message({ type: "rustdar:check-update" });
    }

    assert.equal(
      worker.warnings.some((w) => w.includes("shell version probe has failed")),
      true,
      `a permanently broken probe pinned the shell with no warning: ${JSON.stringify(worker.warnings)}`,
    );
  });

  it("warns when the server publishes no validators to compare", async () => {
    const network = new Network();
    publishUnversionedDeploy(network, ORIGIN, "A");
    const worker = await startWorker({ swUrl: SW_URL, network });
    await worker.activate();

    publishUnversionedDeploy(worker.network, ORIGIN, "B");
    await worker.message({ type: "rustdar:check-update" });

    assert.equal(
      worker.warnings.some((w) => w.includes("neither ETag nor Last-Modified")),
      true,
      `an unversioned server pinned the shell with no warning: ${JSON.stringify(worker.warnings)}`,
    );
  });

  it("reinstalls the shell on demand when the probe cannot tell it to", async () => {
    // The escape hatch. `rustdar:check-update` is useless here by construction:
    // it compares a token the probe cannot supply.
    const network = new Network();
    publishUnversionedDeploy(network, ORIGIN, "A");
    const worker = await startWorker({ swUrl: SW_URL, network });
    await worker.activate();
    assert.deepEqual(generationOf(await loadPage(worker)), new Set(["A"]));

    publishUnversionedDeploy(worker.network, ORIGIN, "B");
    await worker.message({ type: "rustdar:check-update" });
    assert.deepEqual(
      generationOf(await loadPage(worker)),
      new Set(["A"]),
      "a check that cannot detect a change must not pretend it did",
    );

    await worker.message({ type: "rustdar:force-update" });
    assert.deepEqual(
      generationOf(await loadPage(worker)),
      new Set(["B"]),
      "the forced reinstall did not pick up the new deploy",
    );
  });

  it("reinstalls on demand when the probe is refusing HEAD", async () => {
    const worker = await bootWorker({ tag: "A" });
    publishDeploy(worker.network, ORIGIN, "B", { headStatus: 405 });

    await worker.message({ type: "rustdar:check-update" });
    assert.deepEqual(generationOf(await loadPage(worker)), new Set(["A"]));

    await worker.message({ type: "rustdar:force-update" });
    assert.deepEqual(
      generationOf(await loadPage(worker)),
      new Set(["B"]),
      "a forced reinstall must not depend on the probe it exists to work around",
    );
  });

  it("does not reinstall twice when the forced install was already current", async () => {
    const worker = await bootWorker({ tag: "A" });
    await worker.message({ type: "rustdar:force-update" });

    const before = worker.network.log.length;
    await worker.message({ type: "rustdar:check-update" });
    const after = worker.network.log.slice(before);
    assert.deepEqual(
      after.map((e) => e.method),
      ["HEAD"],
      `the ordinary check reinstalled after a force: ${JSON.stringify(after)}`,
    );
  });

  it("keeps serving the shell when the probe fails and there is one installed", async () => {
    const worker = await bootWorker({ tag: "A" });
    worker.network.offline = true;
    await worker.message({ type: "rustdar:check-update" });
    assert.deepEqual(generationOf(await loadPage(worker)), new Set(["A"]));
  });
});

// ===========================================================================
describe("tiles: the basemap cache stays bounded", () => {
  // =========================================================================

  /** Fetch `count` distinct tiles through the worker. */
  async function fetchTiles(worker, count, offset = 0) {
    for (let i = 0; i < count; i += 1) {
      const url = `https://cartodb-basemaps-a.global.ssl.fastly.net/dark_nolabels/7/${offset + i}/52.png`;
      worker.network.serve(
        url,
        () =>
          new Response(`tile ${offset + i}`, {
            status: 200,
            headers: { date: new Date().toUTCString(), "cache-control": "public, max-age=15552000" },
          }),
      );
      await worker.fetch(new Request(url));
    }
  }

  it("caches a tile and serves it back without touching the network", async () => {
    const worker = await bootWorker();
    await fetchTiles(worker, 1);
    const before = worker.network.log.length;
    const event = await worker.fetch(
      new Request("https://cartodb-basemaps-a.global.ssl.fastly.net/dark_nolabels/7/0/52.png"),
    );
    assert.equal(event.handled, true);
    assert.equal(await (await event.response).text(), "tile 0");
    assert.equal(worker.network.log.length, before, "a cached tile was refetched");
  });

  it("enforces the bound across worker restarts, not just within one lifetime", async () => {
    // A service worker is killed after roughly thirty seconds idle. A user
    // panning slowly adds a handful of tiles per lifetime, so a bound that only
    // triggers after a fixed number of writes *within* a lifetime is never
    // reached, and the cache grows without limit until it takes the quota with
    // it — which is what makes the shell's all-or-nothing install fail.
    let worker = await bootWorker();
    const { TILE_CACHE_MAX, TILE_TRIM_BATCH } = worker.internals;

    const perLifetime = 10;
    assert.ok(perLifetime < TILE_TRIM_BATCH, "this test must stay under the batch threshold");

    const lifetimes = Math.ceil((TILE_CACHE_MAX + 200) / perLifetime);
    for (let i = 0; i < lifetimes; i += 1) {
      await fetchTiles(worker, perLifetime, i * perLifetime);
      worker = await restartWorker(worker);
    }

    const tiles = worker.caches.countEntries((n) => n.startsWith("rustdar-basemap-"));
    assert.ok(
      tiles <= TILE_CACHE_MAX + TILE_TRIM_BATCH,
      `the basemap cache holds ${tiles} tiles; the bound is ${TILE_CACHE_MAX} \
(+ at most one ${TILE_TRIM_BATCH}-entry batch of overshoot)`,
    );
  });

  it("evicts oldest-first so the tiles just fetched survive", async () => {
    let worker = await bootWorker();
    const { TILE_CACHE_MAX } = worker.internals;
    await fetchTiles(worker, TILE_CACHE_MAX + 100);
    worker = await restartWorker(worker);
    await fetchTiles(worker, 1, 10_000);

    const urls = worker.cachedUrls().filter((u) => u.includes("cartodb-basemaps"));
    assert.equal(
      urls.some((u) => u.endsWith("/7/10000/52.png")),
      true,
      "the most recently fetched tile was evicted",
    );
    assert.equal(
      urls.some((u) => u.endsWith("/7/0/52.png")),
      false,
      "the oldest tile survived while newer ones were evicted",
    );
  });

  it("revalidates a cached tile that carries no usable Date", async () => {
    const worker = await bootWorker();
    const { tileIsStale } = worker.internals;

    assert.equal(
      tileIsStale(new Response("t", { status: 200 })),
      true,
      "a readable response with no Date cannot be shown to be fresh, so it must revalidate",
    );
    assert.equal(
      tileIsStale(new Response("t", { status: 200, headers: { date: "not a date" } })),
      true,
    );
    assert.equal(
      tileIsStale(
        new Response("t", {
          status: 200,
          headers: { date: new Date().toUTCString(), "cache-control": "max-age=15552000" },
        }),
      ),
      false,
    );
    // An opaque response exposes no headers at all. There is nothing to reason
    // about and a coastline does not go dangerously wrong with age.
    assert.equal(tileIsStale(opaqueResponse()), false);
  });

  it("honours the origin's own max-age rather than inventing one", async () => {
    const worker = await bootWorker();
    const { tileFreshFor } = worker.internals;
    assert.equal(
      tileFreshFor(new Response("t", { headers: { "cache-control": "public, max-age=15552000" } })),
      15552000 * 1000,
    );
  });
});

// ===========================================================================
describe("lifecycle", () => {
  // =========================================================================

  it("claims its clients so the update channel works on the first visit", async () => {
    const worker = await bootWorker();
    assert.equal(worker.scope.clients.claimed, true);
  });

  it("does not swap the controller until the user accepts the reload", async () => {
    const worker = await bootWorker();
    publishDeploy(worker.network, ORIGIN, "B");
    await worker.message({ type: "rustdar:check-update" });
    assert.equal(
      worker.scope.skipWaitingCalled,
      false,
      "a new shell must not take over a running page unasked",
    );

    await worker.message({ type: "rustdar:skip-waiting" });
    assert.equal(worker.scope.skipWaitingCalled, true);
  });

  it("precaches nothing during install, so a flaky network cannot fail it", async () => {
    const network = new Network();
    publishDeploy(network, ORIGIN, "A");
    network.offline = true;
    const worker = await startWorker({ swUrl: SW_URL, network });
    await worker.install();
    assert.deepEqual(
      await worker.cacheNames(),
      [],
      "the install event wrote to a cache; a failure there fails the registration",
    );
  });

  it("survives a first activation with no network at all", async () => {
    const network = new Network();
    network.offline = true;
    const worker = await startWorker({ swUrl: SW_URL, network });
    await worker.activate();
    // Nothing to serve, so the navigation must simply fall through.
    const event = await worker.fetch(worker.navigation(ORIGIN), { resultingClientId: "c1" });
    await assert.rejects(async () => (await event.response).text());
  });
});
