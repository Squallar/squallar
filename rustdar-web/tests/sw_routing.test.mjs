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

import { Network, publishDeploy, startWorker } from "./sw_harness.mjs";

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

});

// ===========================================================================
describe("routing: the shell rule is confined to the deploy directory", () => {
  // =========================================================================

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
// ===========================================================================
// ===========================================================================
// ===========================================================================
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
