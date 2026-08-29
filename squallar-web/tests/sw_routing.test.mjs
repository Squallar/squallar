/*
 * Behavioural gates on the shipped service worker.
 *
 * ============================================================================
 * What these tests are for
 * ============================================================================
 *
 * `squallar-web/tests/pwa_assets.rs` reads `sw.js` as text and checks that the
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
  publishIndexOnlyDeploy,
  publishUnversionedDeploy,
  restartWorker,
  startWorker,
} from "./sw_harness.mjs";

const ORIGIN = "https://squallar.example/squallar/";
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
    await worker.fetch(new Request(shellUrl("pkg/squallar_web.js")), { clientId: client.id }),
  );
  const wasm = await read(
    await worker.fetch(new Request(shellUrl("pkg/squallar_web_bg.wasm")), { clientId: client.id }),
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
      // Schemes the Cache API would reject anyway.
      "data:image/png;base64,iVBORw0KGgo=",
      "blob:https://squallar.example/9f0b",
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

  it("keeps the deny list load-bearing even when squallar is served from a data origin", async () => {
    // The point of this test.
    //
    // `routeFor` is default-deny, so on squallar's real deploy the
    // NEVER_CACHE_HOSTS check is unobservable: nothing reaches it that the
    // default would not also refuse. Deleting it changes no behaviour, which
    // makes it look like decoration and makes it deletable by someone tidying.
    //
    // It is not decoration. It is the only thing standing between the two rules
    // that *do* cache — a same-origin navigation and a same-origin shell asset
    // — and a weather origin, if squallar is ever served from one: behind a
    // reverse proxy, on a shared host, or from a mirror. Serving the worker
    // from `api.weather.gov` is how that is made observable.
    const worker = await bootWorker({
      swUrl: "https://api.weather.gov/squallar/sw.js",
      origin: "https://api.weather.gov/squallar/",
    });
    const { routeFor } = worker.internals;

    for (const url of [
      "https://api.weather.gov/alerts/active",
      "https://api.weather.gov/squallar/",
      "https://api.weather.gov/squallar/pkg/squallar_web_bg.wasm",
      "https://api.weather.gov/squallar/manifest.webmanifest",
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
describe("routing: the PMTiles basemap archive is never cached", () => {
  // =========================================================================
  //
  // WHY THIS IS ITS OWN BLOCK, AND WHERE ITS NON-VACUITY COMES FROM.
  //
  // `routeFor` is default-deny, so on squallar's real deploy `isBasemapArchive`
  // is unobservable through `routeFor` alone: deleting the rule changes no
  // answer, because nothing reaches it that the default would not also refuse.
  // That is the shape the `isWeatherDataHost` block above already names — the
  // default would mask a regression — so the assertions that can actually fail
  // are the ones on `isBasemapArchive` itself, plus the served-from-the-archive-
  // host boot, where the same-origin shell and navigate rules ARE reachable and
  // the rule is the only thing standing in front of them.
  //
  // The archive cannot be cached even in principle: every read carries a
  // `Range`, `Cache.put()` throws a TypeError on the resulting 206, and
  // `Cache.match()` is range-blind, so a stored range would be handed back for
  // every other offset.

  it("recognises the archive by host and extension, not by either alone", async () => {
    const worker = await bootWorker();
    const { isBasemapArchive } = worker.internals;
    const u = (s) => new URL(s);

    for (const url of [
      "https://tiles.squallar.app/basemap/omt-20260828.pmtiles",
      // Case, and a fully-qualified host: `normalizeHost` is what both go
      // through, and this is the layer that has to be sound on its own.
      "https://TILES.SQUALLAR.APP/basemap/omt-20260828.PMTILES",
      "https://tiles.squallar.app./basemap/omt-20260828.pmtiles",
    ]) {
      assert.equal(isBasemapArchive(u(url)), true, `${url} is the basemap archive`);
    }

    for (const url of [
      // Suffix confusion in both directions — the whole reason this is an
      // equality against a normalised host rather than an `endsWith`.
      "https://tiles.squallar.app.evil.example/basemap/omt.pmtiles",
      "https://evil.example/tiles.squallar.app/basemap/omt.pmtiles",
      "https://nottiles.squallar.app/basemap/omt.pmtiles",
      // The same host's OTHER content. The extension is load-bearing: without
      // it this rule would also refuse a navigation, were squallar ever served
      // from here.
      "https://tiles.squallar.app/status/latest.json",
      "https://tiles.squallar.app/",
      // The extension somewhere that is not the path's end.
      "https://tiles.squallar.app/basemap/omt.pmtiles/tile",
    ]) {
      assert.equal(isBasemapArchive(u(url)), false, `${url} is not the basemap archive`);
    }
  });

  it("routes the archive to the network even when squallar is served from its host", async () => {
    // The non-vacuous half. Booting the worker at the archive's own origin, at
    // the site root, puts the archive under `ROOT.pathname` — so the
    // same-origin branch of `routeFor` is reached and `isBasemapArchive` is the
    // only rule in front of it. Delete the rule and this test still passes on
    // `isShellAsset`/`isDataAsset` missing; what it pins is that no future
    // caching rule can be added above without this one refusing first, and the
    // navigation below is what proves the refusal is scoped rather than total.
    const worker = await bootWorker({
      swUrl: "https://tiles.squallar.app/sw.js",
      origin: "https://tiles.squallar.app/",
    });
    const { routeFor } = worker.internals;

    assert.equal(
      routeFor({ url: "https://tiles.squallar.app/basemap/omt-20260828.pmtiles" }),
      "network",
      "the archive must reach the network untouched: a 206 cannot be put in a Cache",
    );
    assert.equal(
      routeFor({ url: "https://tiles.squallar.app/basemap/omt-20260828.pmtiles", mode: "navigate" }),
      "network",
      "not cacheable even when the browser calls the request a navigation",
    );
    // Scoped, not total: the rule must not swallow the deploy it shares a host
    // with. This is what the `.pmtiles` half of the predicate buys.
    assert.equal(
      routeFor({ url: "https://tiles.squallar.app/", mode: "navigate" }),
      "navigate",
      "a navigation on the archive host is still a navigation",
    );
  });

  it("keeps the worker out of the archive's request path entirely", async () => {
    // Stronger than "routes to the network": there must be no code path that
    // could hand a 206 to `Cache.put()`, so the worker must not call
    // respondWith at all.
    const worker = await bootWorker();
    const event = await worker.fetch(
      new Request("https://tiles.squallar.app/basemap/omt-20260828.pmtiles"),
    );
    assert.equal(
      event.handled,
      false,
      "the worker called respondWith for the archive; it must stay out of the request path",
    );
  });

  it("cannot be given a cache without `cachesToKeep` also naming it", async () => {
    // The hazard, demonstrated rather than asserted in prose: a future author
    // who adds an archive cache and stops there gets a cache that is emptied on
    // the next deploy, and the symptom is a slow map rather than an error.
    //
    // NOTE THE TRIGGER, because it is not "every activate" and the difference
    // is what makes the test honest. `checkForUpdate` returns early when the
    // validator token has not moved (`if (meta && meta.token === token)
    // return;`), so `purgeCaches` never runs on an activate that finds the same
    // deploy — seeding a cache and re-activating leaves it alone. The purge is
    // per DEPLOY. That is why this publishes one.
    let worker = await bootWorker({ tag: "A" });
    const speculative = "squallar-basemap-archive-v1";
    const cache = await worker.caches.open(speculative);
    await cache.put(
      new Request("https://tiles.squallar.app/basemap/omt-20260828.pmtiles"),
      new Response("PMTiles"),
    );
    assert.ok(
      (await worker.cacheNames()).includes(speculative),
      "the harness did not create the cache; the rest of this test would be vacuous",
    );

    publishDeploy(worker.network, ORIGIN, "B");
    await worker.message({ type: "squallar:check-update" });
    worker = await restartWorker(worker);

    assert.ok(
      !(await worker.cacheNames()).includes(speculative),
      `${speculative} survived a deploy, so purgeCaches is no longer exhaustive; \
an archive cache added without a cachesToKeep entry would look like it worked`,
    );
    // The control: a cache `cachesToKeep` DOES name survives the same deploy,
    // so the assertion above is about the keep set and not about the purge
    // running. The meta cache is the one such cache every boot creates.
    assert.ok(
      (await worker.cacheNames()).includes(worker.internals.META_CACHE),
      "the deploy purged a cache cachesToKeep names; this test is measuring the wrong thing",
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
    // squallar's index.html would replace those sites with this one.
    for (const url of [
      "https://squallar.example/somewhere/else",
      "https://squallar.example/",
      "https://squallar.example/squallar-old/index.html",
      "https://squallar.example/squallarn't/",
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
    assert.equal(routeFor({ url: shellUrl("pkg/squallar_web.js") }), "shell");
    assert.equal(routeFor({ url: `${shellUrl("pkg/squallar_web.js")}?v=2` }), "shell");
    assert.equal(routeFor({ url: shellUrl("pkg/squallar_web_bg.wasm") }), "shell");
    assert.equal(routeFor({ url: shellUrl("manifest.webmanifest") }), "shell");
    // Not precached, so not a shell asset: it must reach the network.
    assert.equal(routeFor({ url: shellUrl("pkg/squallar_web.d.ts") }), "network");
  });

  it("never treats a non-GET as cacheable", async () => {
    const worker = await bootWorker();
    const { routeFor } = worker.internals;
    for (const method of ["POST", "PUT", "DELETE", "HEAD", "OPTIONS"]) {
      assert.equal(routeFor({ url: ORIGIN, method, mode: "navigate" }), "network");
      assert.equal(routeFor({ url: shellUrl("zones.pack"), method }), "network");
      assert.equal(routeFor({ url: shellUrl("pkg/squallar_web.js"), method }), "network");
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
    const event = await worker.fetch(new Request(shellUrl("pkg/squallar_web.js")));
    assert.equal(event.handled, true);
    assert.equal(await (await event.response).text(), "pkg/squallar_web.js::A");
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

    const event = await worker.fetch(new Request(shellUrl("pkg/squallar_web.js")));
    assert.equal(event.handled, true);
    assert.equal(
      await (await event.response).text(),
      "pkg/squallar_web.js::A",
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
    const glue = await worker.fetch(new Request(shellUrl("pkg/squallar_web.js")), {
      clientId: client.id,
    });

    // Deploy B lands, via the message index.html sends on visibilitychange.
    publishDeploy(worker.network, ORIGIN, "B");
    await worker.message({ type: "squallar:check-update" });

    const wasm = await worker.fetch(new Request(shellUrl("pkg/squallar_web_bg.wasm")), {
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
    const release = worker.network.hold(shellUrl("pkg/squallar_web_bg.wasm"));
    const update = worker.message({ type: "squallar:check-update" });
    await new Promise((resolve) => setTimeout(resolve, 5));
    release();
    await update;

    const wasm = await worker.fetch(new Request(shellUrl("pkg/squallar_web_bg.wasm")), {
      clientId: client.id,
    });
    assert.equal(await (await wasm.response).text(), "pkg/squallar_web_bg.wasm::A");
  });

  it("gives a page that loads after the update the new deploy, whole", async () => {
    // The pin must not become a way of pinning everyone to the old bundle
    // forever. A fresh navigation is a fresh decision.
    const worker = await bootWorker({ tag: "A" });
    const first = worker.addClient();
    await worker.fetch(worker.navigation(ORIGIN), { resultingClientId: first.id });

    publishDeploy(worker.network, ORIGIN, "B");
    await worker.message({ type: "squallar:check-update" });

    const page = await loadPage(worker);
    assert.deepEqual(generationOf(page), new Set(["B"]));
  });

  it("keeps a client on its generation across a worker restart", async () => {
    // A service worker is killed after about thirty seconds idle, and the pin
    // is module state. If it only lived in memory, a restart between a page's
    // navigation and its wasm request would put that page back onto whatever
    // is current — the same mixed shell, reached by a different route.
    //
    // Retaining "the generation just superseded" instead does not fix this. It
    // keeps deploy A alive, but the restarted worker has no idea this client
    // belongs to A and serves it B regardless. Verified before the pin was made
    // durable: the page navigated under A and was handed B's glue.
    const worker = await bootWorker({ tag: "A" });
    const client = worker.addClient();
    await worker.fetch(worker.navigation(ORIGIN), { resultingClientId: client.id });

    const restarted = await restartWorker(worker);
    publishDeploy(restarted.network, ORIGIN, "B");
    await restarted.message({ type: "squallar:check-update" });

    const glue = await restarted.fetch(new Request(shellUrl("pkg/squallar_web.js")), {
      clientId: client.id,
    });
    assert.equal(
      await (await glue.response).text(),
      "pkg/squallar_web.js::A",
      "a page that navigated under deploy A was handed deploy B's glue after the \
worker restarted; its index.html and its wasm now disagree",
    );
  });

  it("still moves a page that loads after the restart onto the new deploy", async () => {
    // The durable pin must not become a way of pinning the whole browser to an
    // old bundle. Only the client that navigated under it is held.
    const worker = await bootWorker({ tag: "A" });
    const first = worker.addClient();
    await worker.fetch(worker.navigation(ORIGIN), { resultingClientId: first.id });

    const restarted = await restartWorker(worker);
    publishDeploy(restarted.network, ORIGIN, "B");
    await restarted.message({ type: "squallar:check-update" });

    assert.deepEqual(generationOf(await loadPage(restarted)), new Set(["B"]));
  });

  it("does not accumulate shell generations without limit", async () => {
    const worker = await bootWorker({ tag: "A" });
    for (const tag of ["B", "C", "D", "E"]) {
      publishDeploy(worker.network, ORIGIN, tag);
      await worker.message({ type: "squallar:check-update" });
    }
    const shells = (await worker.cacheNames()).filter((n) => n.startsWith("squallar-shell-"));
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
    await worker.message({ type: "squallar:check-update" });

    const second = worker.addClient();
    await worker.fetch(worker.navigation(ORIGIN), { resultingClientId: second.id });
    publishDeploy(worker.network, ORIGIN, "C");
    await worker.message({ type: "squallar:check-update" });

    const shells = (await worker.cacheNames()).filter((n) => n.startsWith("squallar-shell-"));
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
      names.some((n) => n.startsWith("squallar-shell-")),
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

    await worker.message({ type: "squallar:check-update" });

    const page = await loadPage(worker);
    assert.deepEqual(
      generationOf(page),
      new Set(["A"]),
      "a failed update must leave the previous complete shell serving",
    );
  });

  it("keeps a pinned generation intact when a rollback's install fails", async () => {
    // A rollback re-issues a token seen before, so the install opens the very
    // cache a live client is pinned to — the one every purge spared because of
    // that pin. If the download then fails, deleting "the" cache by name would
    // destroy the in-use generation: the installer manufacturing the mixed
    // shell the pinning exists to prevent.
    const worker = await bootWorker({ tag: "A" });
    const client = worker.addClient();
    await worker.fetch(worker.navigation(ORIGIN), { resultingClientId: client.id });

    // Deploy B lands and installs; the client stays pinned to generation A.
    publishDeploy(worker.network, ORIGIN, "B");
    await worker.message({ type: "squallar:check-update" });

    // The deploy is rolled back to A, but the re-download fails halfway.
    publishDeploy(worker.network, ORIGIN, "A");
    worker.network.serve(shellUrl("icons/icon-512.png"), new Response("gone", { status: 500 }));
    await worker.message({ type: "squallar:check-update" });

    // The pinned page's wasm request must still be answerable from A.
    const wasm = await worker.fetch(new Request(shellUrl("pkg/squallar_web_bg.wasm")), {
      clientId: client.id,
    });
    assert.equal(
      await (await wasm.response).text(),
      "pkg/squallar_web_bg.wasm::A",
      "the failed rollback install deleted the generation this page was loading from",
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
    await worker.message({ type: "squallar:check-update" });

    for (const client of [one, two]) {
      assert.equal(
        client.messages.some((m) => m.type === "squallar:shell-updated"),
        true,
        "an open window was not told a new version is ready",
      );
    }
  });

  it("re-downloads nothing when the deploy has not changed", async () => {
    const worker = await bootWorker({ tag: "A" });
    const before = worker.network.log.length;
    await worker.message({ type: "squallar:check-update" });
    const after = worker.network.log.slice(before);
    assert.deepEqual(
      after.map((e) => e.method),
      ["HEAD", "HEAD"],
      `an unchanged deploy cost more than the two probe HEADs: ${JSON.stringify(after)}`,
    );
  });
});

// ===========================================================================
describe("updates: the version probe watches both halves of a deploy", () => {
  // =========================================================================

  it("detects a deploy that changed only the index, not the wasm", async () => {
    // The regression this pins. A shell-side deploy edits index.html and ships
    // the same wasm bytes, so a probe that watched only the wasm produced an
    // identical token and never installed it — and navigations are cache-first,
    // so the stale index was served indefinitely, with only squallarForceUpdate()
    // or a later wasm-changing deploy as the way out.
    const worker = await bootWorker({ tag: "A" });
    publishIndexOnlyDeploy(worker.network, ORIGIN, "A", "A2");
    await worker.message({ type: "squallar:check-update" });

    const page = await loadPage(worker);
    assert.equal(
      page.document,
      "::A2",
      "an index-only deploy was not detected; the old shell is still being served",
    );
    assert.equal(
      page.wasm,
      "pkg/squallar_web_bg.wasm::A",
      "the wasm module did not change in this deploy and must be served as-is",
    );
  });

  it("announces an index-only deploy to open windows", async () => {
    // The reload prompt is how a mid-session tab hears about a deploy at all;
    // an update the probe can now see must reach it like any other.
    const worker = await bootWorker({ tag: "A" });
    const client = worker.addClient();
    publishIndexOnlyDeploy(worker.network, ORIGIN, "A", "A2");
    await worker.message({ type: "squallar:check-update" });

    assert.equal(
      client.messages.some((m) => m.type === "squallar:shell-updated"),
      true,
      "an open window was not told the index-only deploy is ready",
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
      await worker.message({ type: "squallar:check-update" });
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
    await worker.message({ type: "squallar:check-update" });

    assert.equal(
      worker.warnings.some((w) => w.includes("neither ETag nor Last-Modified")),
      true,
      `an unversioned server pinned the shell with no warning: ${JSON.stringify(worker.warnings)}`,
    );
  });

  it("reinstalls the shell on demand when the probe cannot tell it to", async () => {
    // The escape hatch. `squallar:check-update` is useless here by construction:
    // it compares a token the probe cannot supply.
    const network = new Network();
    publishUnversionedDeploy(network, ORIGIN, "A");
    const worker = await startWorker({ swUrl: SW_URL, network });
    await worker.activate();
    assert.deepEqual(generationOf(await loadPage(worker)), new Set(["A"]));

    publishUnversionedDeploy(worker.network, ORIGIN, "B");
    await worker.message({ type: "squallar:check-update" });
    assert.deepEqual(
      generationOf(await loadPage(worker)),
      new Set(["A"]),
      "a check that cannot detect a change must not pretend it did",
    );

    await worker.message({ type: "squallar:force-update" });
    assert.deepEqual(
      generationOf(await loadPage(worker)),
      new Set(["B"]),
      "the forced reinstall did not pick up the new deploy",
    );
  });

  it("reinstalls on demand when the probe is refusing HEAD", async () => {
    const worker = await bootWorker({ tag: "A" });
    publishDeploy(worker.network, ORIGIN, "B", { headStatus: 405 });

    await worker.message({ type: "squallar:check-update" });
    assert.deepEqual(generationOf(await loadPage(worker)), new Set(["A"]));

    await worker.message({ type: "squallar:force-update" });
    assert.deepEqual(
      generationOf(await loadPage(worker)),
      new Set(["B"]),
      "a forced reinstall must not depend on the probe it exists to work around",
    );
  });

  it("does not rewrite a generation a live client is pinned to", async () => {
    // A forced reinstall on an unversioned server produces the same token as
    // the shell already installed. If it downloaded into the cache that token
    // names, it would rewrite that shell's entries — and a page mid-load in it
    // would find its wasm module replaced between the navigation and the
    // instantiate. Which is the mixed shell again, this time caused by the
    // mechanism meant to be the safe way out.
    const network = new Network();
    publishUnversionedDeploy(network, ORIGIN, "A");
    const worker = await startWorker({ swUrl: SW_URL, network });
    await worker.activate();

    const client = worker.addClient();
    const nav = await worker.fetch(worker.navigation(ORIGIN), { resultingClientId: client.id });
    assert.equal(await (await nav.response).text(), "::A");

    publishUnversionedDeploy(worker.network, ORIGIN, "B");
    await worker.message({ type: "squallar:force-update" });

    const wasm = await worker.fetch(new Request(shellUrl("pkg/squallar_web_bg.wasm")), {
      clientId: client.id,
    });
    assert.equal(
      await (await wasm.response).text(),
      "pkg/squallar_web_bg.wasm::A",
      "the forced reinstall overwrote the shell this page was loading from",
    );
  });

  it("does not reinstall twice when the forced install was already current", async () => {
    const worker = await bootWorker({ tag: "A" });
    await worker.message({ type: "squallar:force-update" });

    const before = worker.network.log.length;
    await worker.message({ type: "squallar:check-update" });
    const after = worker.network.log.slice(before);
    assert.deepEqual(
      after.map((e) => e.method),
      ["HEAD", "HEAD"],
      `the ordinary check reinstalled after a force: ${JSON.stringify(after)}`,
    );
  });

  it("keeps serving the shell when the probe fails and there is one installed", async () => {
    const worker = await bootWorker({ tag: "A" });
    worker.network.offline = true;
    await worker.message({ type: "squallar:check-update" });
    assert.deepEqual(generationOf(await loadPage(worker)), new Set(["A"]));
  });
});

// ===========================================================================
describe("assets: the zone pack is cached, and never at the shell's expense", () => {
  // =========================================================================

  const PACK_URL = `${ORIGIN}zones.pack`;

  /** Serve a pack of `body` and fetch it once through the worker. */
  async function fetchPack(worker, body = "NWSZPK...") {
    worker.network.serve(
      PACK_URL,
      () => new Response(body, { status: 200, headers: { "content-type": "application/octet-stream" } }),
    );
    return worker.fetch(new Request(PACK_URL));
  }

  it("routes the pack to its own cache and not to the shell", async () => {
    const worker = await bootWorker();
    const { routeFor, SHELL_URLS, ASSET_PATHS } = worker.internals;

    assert.equal(routeFor({ url: PACK_URL }), "asset");
    assert.ok(ASSET_PATHS.includes("zones.pack"), "the pack must be a declared asset");

    // The whole reason it is a separate route: `SHELL_PATHS` is installed with
    // `cache.addAll`, which is all-or-nothing, so a multi-megabyte entry that
    // failed to fetch would take offline support for the entire app with it.
    assert.ok(
      !SHELL_URLS.includes(PACK_URL),
      "the pack is in the all-or-nothing shell install; one bad fetch would " +
        "cost the app its offline support",
    );

    // The rule is confined the same way the shell rule is.
    assert.equal(routeFor({ url: "https://squallar.example/zones.pack" }), "network");
    assert.equal(routeFor({ url: `${ORIGIN}zones.pack`, method: "POST" }), "network");
  });

  it("serves the second request from cache without touching the network", async () => {
    const worker = await bootWorker();
    const first = await fetchPack(worker);
    assert.equal(first.handled, true, "the worker must be in the pack's request path");
    assert.equal(await (await first.response).text(), "NWSZPK...");

    const before = worker.network.log.length;
    const second = await worker.fetch(new Request(PACK_URL));
    assert.equal(await (await second.response).text(), "NWSZPK...");
    assert.equal(
      worker.network.log.length,
      before,
      "the pack was refetched; one download per session is the entire point",
    );
  });

  it("keeps the pack across a deploy, which the shell deliberately does not", async () => {
    let worker = await bootWorker({ tag: "A" });
    await fetchPack(worker);
    assert.ok(worker.caches.countEntries((n) => n.startsWith("squallar-assets-")) > 0);

    // A new deploy: the validator token moves, the shell is refetched whole,
    // and `purgeCaches` deletes every `squallar-` cache not named in
    // `cachesToKeep`. The pack changes when the NWS publishes a new zone
    // edition, not when squallar ships, so it must survive.
    publishDeploy(worker.network, ORIGIN, "B");
    await worker.message({ type: "squallar:check-update" });
    worker = await restartWorker(worker);

    const before = worker.network.log.length;
    const after = await worker.fetch(new Request(PACK_URL));
    assert.equal(await (await after.response).text(), "NWSZPK...");
    assert.equal(
      worker.network.log.length,
      before,
      "the deploy purged the pack; every push to main would re-download it",
    );
  });

  it("does not cache a failed fetch, so a bad pack is not made permanent", async () => {
    const worker = await bootWorker();
    worker.network.serve(PACK_URL, () => new Response("nope", { status: 404 }));
    const missed = await worker.fetch(new Request(PACK_URL));
    assert.equal((await missed.response).status, 404);
    assert.equal(
      worker.caches.countEntries((n) => n.startsWith("squallar-assets-")),
      0,
      "a 404 was written to the asset cache and would be served forever",
    );

    // And the app recovers on the next session, because nothing was stored.
    const found = await fetchPack(worker);
    assert.equal(await (await found.response).text(), "NWSZPK...");
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
    await worker.message({ type: "squallar:check-update" });
    assert.equal(
      worker.scope.skipWaitingCalled,
      false,
      "a new shell must not take over a running page unasked",
    );

    await worker.message({ type: "squallar:skip-waiting" });
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

// ===========================================================================
describe("archive blocks: ranged reads survive in a persistent block cache", () => {
  // =========================================================================
  //
  // The PMTiles archives are read by `Range`, `Cache.put()` throws on the 206
  // those reads produce, and browsers do not reliably cache partial responses
  // — so before this cache, browsed areas survived nothing. The worker now
  // quantizes each single-range read to 64 KiB blocks, stores each block as a
  // synthetic-URL 200 (the 206 prohibition is about what is STORED, not what
  // is SERVED), and reassembles an exact 206. Exactness is load-bearing: the
  // wasm reader errors `RangeRequestsUnsupported` on any status but 206 and
  // `ResponseBodyTooLong` on an over-long body.
  //
  // The negatives here follow the suite's own rule: `routeFor` is default-deny,
  // so a deleted predicate can hide behind the default. The assertions that can
  // actually fail are on the predicates themselves plus the behavioural runs.

  /** A deterministic archive body: byte i is (i * 31 + seed) % 256. */
  function archiveBytes(length, seed = 7) {
    const bytes = new Uint8Array(length);
    for (let i = 0; i < length; i += 1) bytes[i] = (i * 31 + seed) % 256;
    return bytes;
  }

  /** The Range header of a request, whichever shape the harness's fetch saw. */
  function rangeHeaderOf(input, init) {
    if (typeof input !== "string" && input && input.headers && typeof input.headers.get === "function") {
      return input.headers.get("range");
    }
    const headers = init && init.headers;
    if (!headers) return null;
    if (typeof headers.get === "function") return headers.get("range");
    return headers.range ?? headers.Range ?? null;
  }

  /** Serve `bytes` at `url` as an origin that honours single `bytes=N-M` ranges. */
  function publishArchive(network, url, bytes, { ignoreRange = false } = {}) {
    network.serve(url, (input, init) => {
      const range = ignoreRange ? null : rangeHeaderOf(input, init);
      const match = range === null ? null : /^bytes=(\d+)-(\d+)$/.exec(range);
      if (!match) return new Response(bytes, { status: 200 });
      const start = Number(match[1]);
      if (start >= bytes.length) {
        return new Response("unsatisfiable", {
          status: 416,
          headers: { "content-range": `bytes */${bytes.length}` },
        });
      }
      const end = Math.min(Number(match[2]), bytes.length - 1);
      return new Response(bytes.slice(start, end + 1), {
        status: 206,
        headers: {
          "content-range": `bytes ${start}-${end}/${bytes.length}`,
          "content-type": "application/octet-stream",
        },
      });
    });
  }

  function fetchRange(worker, url, start, end) {
    return worker.fetch(new Request(url, { headers: { range: `bytes=${start}-${end}` } }));
  }

  async function bodyOf(event) {
    return Buffer.from(await (await event.response).arrayBuffer());
  }

  function blockCacheOf(worker, url) {
    const { archiveGeneration, blockCacheName } = worker.internals;
    return blockCacheName(archiveGeneration(new URL(url).pathname));
  }

  function archiveRequests(network, url) {
    return network.log.filter((e) => e.url === url).length;
  }

  /** Test-only reach into the harness's storage: the entries of one cache. */
  function blockEntries(worker, name) {
    const cache = worker.caches.caches.get(name);
    return cache ? [...cache.entries.entries()] : [];
  }

  it("quantizes a span to 64 KiB blocks, inclusive at both edges", async () => {
    const worker = await bootWorker();
    const { blockSpan, BLOCK_BYTES } = worker.internals;
    const B = BLOCK_BYTES;
    assert.equal(B, 64 * 1024, "the block quantum moved; every pin below assumed 64 KiB");

    assert.deepEqual(blockSpan({ start: 0, end: B - 1 }), { first: 0, last: 0 });
    assert.deepEqual(blockSpan({ start: 0, end: B }), { first: 0, last: 1 });
    assert.deepEqual(blockSpan({ start: B - 1, end: B }), { first: 0, last: 1 });
    assert.deepEqual(blockSpan({ start: B, end: 2 * B - 1 }), { first: 1, last: 1 });
    assert.deepEqual(blockSpan({ start: 2 * B, end: 2 * B }), { first: 2, last: 2 });
  });

  it("accepts exactly the single-range form the reader emits, nothing else", async () => {
    const worker = await bootWorker();
    const { parseSingleRange } = worker.internals;

    assert.deepEqual(parseSingleRange("bytes=0-15"), { start: 0, end: 15 });
    assert.deepEqual(parseSingleRange("bytes=127-16383"), { start: 127, end: 16383 });

    for (const value of [
      null,
      "",
      "bytes=0-", // open-ended
      "bytes=-500", // suffix
      "bytes=0-1,5-9", // multi-range: reassembly would need a multipart body
      "octets=0-1",
      "bytes=5-2", // inverted
      "bytes = 0-1",
      "Bytes=0-1", // not the reader's spelling; passes through, which is safe
    ]) {
      assert.equal(parseSingleRange(value), null, `${JSON.stringify(value)} must not be owned`);
    }
  });

  it("derives one generation per publish, shared by a part and its monolith", async () => {
    const worker = await bootWorker();
    const { archiveGeneration, blockCacheName, BLOCK_CACHE_PREFIX } = worker.internals;

    const monolith = "/terrain/4ca64469750e-20260829/squallar-terrain-hillshade.pmtiles";
    assert.equal(archiveGeneration(monolith), monolith);
    assert.equal(archiveGeneration(`${monolith}.part000`), monolith);
    assert.equal(archiveGeneration(`${monolith}.part123`), monolith);

    // A regenerated archive is a NEW generation: this is the assertion that a
    // wrong key would silently serve one publish's bytes as another's.
    assert.notEqual(
      blockCacheName(archiveGeneration("/basemap/omt-20260828.pmtiles")),
      blockCacheName(archiveGeneration("/basemap/omt-20260901.pmtiles")),
    );

    // The purge machinery only sees `squallar-` caches; a name outside that
    // prefix would be invisible to it and grow forever.
    const name = blockCacheName(archiveGeneration(monolith));
    assert.ok(name.startsWith(BLOCK_CACHE_PREFIX) && name.startsWith("squallar-"), name);
  });

  it("routes exactly the reader's ranged archive reads to the block cache", async () => {
    const worker = await bootWorker();
    const { routeFor, isArchivePart, isArchiveBlockSource, ARCHIVE_URLS } = worker.internals;
    const [basemap, terrain] = ARCHIVE_URLS;
    const range = "bytes=127-16383";

    assert.equal(routeFor({ url: basemap, range }), "archive-block");
    assert.equal(routeFor({ url: terrain, range }), "archive-block");
    // The terrain archive is PUBLISHED as parts; the reader fetches them
    // directly, and a part does not end in `.pmtiles`.
    assert.equal(routeFor({ url: `${terrain}.part000`, range }), "archive-block");

    // Not owned: everything that is not a single-range GET of an archive file.
    assert.equal(routeFor({ url: basemap }), "network");
    assert.equal(routeFor({ url: basemap, range: "bytes=0-1,5-9" }), "network");
    assert.equal(routeFor({ url: basemap, range: "bytes=-500" }), "network");
    assert.equal(routeFor({ url: basemap, method: "POST", range }), "network");
    assert.equal(routeFor({ url: "https://example.test/data.bin", range }), "network");

    // The predicates themselves, because default-deny masks their deletion.
    const u = (s) => new URL(s);
    assert.equal(isArchivePart(u(`${terrain}.part000`)), true);
    assert.equal(isArchiveBlockSource(u(basemap)), true);
    for (const url of [
      "https://tiles.squallar.app.evil.example/x.pmtiles.part000",
      "https://tiles.squallar.app/status/latest.json",
      "https://tiles.squallar.app/basemap/omt.pmtiles.part", // no digits
      "https://tiles.squallar.app/basemap/omt.pmtiles.part00", // part_url pads to 3
      "https://tiles.squallar.app/basemap/omt.pmtiles/tile",
    ]) {
      assert.equal(isArchiveBlockSource(u(url)), false, `${url} is not an archive file`);
    }
  });

  it("stays out of the request path for archive requests it does not own", async () => {
    const worker = await bootWorker();
    for (const request of [
      new Request(worker.internals.ARCHIVE_URLS[0]),
      new Request(worker.internals.ARCHIVE_URLS[0], { headers: { range: "bytes=0-1,5-9" } }),
      new Request("https://example.test/data.bin", { headers: { range: "bytes=0-99" } }),
    ]) {
      const event = await worker.fetch(request);
      assert.equal(event.handled, false, `the worker called respondWith for ${request.url}`);
    }
  });

  it("reassembles the exact span with an exact Content-Range", async () => {
    const worker = await bootWorker();
    const url = worker.internals.ARCHIVE_URLS[0];
    const B = worker.internals.BLOCK_BYTES;
    const bytes = archiveBytes(3 * B + 4928);
    publishArchive(worker.network, url, bytes);

    // Unaligned at both edges, spanning three blocks.
    const event = await fetchRange(worker, url, 100, 2 * B + 99);
    assert.equal(event.handled, true, "the worker must own the read");
    const response = await event.response;
    assert.equal(response.status, 206, "the reader errors RangeRequestsUnsupported on anything else");
    assert.equal(
      response.headers.get("content-range"),
      `bytes 100-${2 * B + 99}/${bytes.length}`,
      "the Content-Range must describe exactly the bytes returned",
    );
    const body = Buffer.from(await response.arrayBuffer());
    assert.equal(body.length, 2 * B, "the reader errors ResponseBodyTooLong on an over-long body");
    assert.ok(body.equals(Buffer.from(bytes.slice(100, 2 * B + 100))), "the bytes are wrong");

    // A span inside one block, and one aligned to both block edges.
    for (const [start, end] of [
      [B + 4464, B + 4564],
      [B, 2 * B - 1],
    ]) {
      const inner = await fetchRange(worker, url, start, end);
      const innerBody = await bodyOf(inner);
      assert.equal(innerBody.length, end - start + 1);
      assert.ok(innerBody.equals(Buffer.from(bytes.slice(start, end + 1))), `bytes=${start}-${end}`);
      assert.equal(
        (await inner.response).headers.get("content-range"),
        `bytes ${start}-${end}/${bytes.length}`,
      );
    }
  });

  it("stores blocks only as synthetic 200s under app-origin keys", async () => {
    const worker = await bootWorker();
    const url = worker.internals.ARCHIVE_URLS[0];
    const bytes = archiveBytes(3 * worker.internals.BLOCK_BYTES);
    publishArchive(worker.network, url, bytes);

    await fetchRange(worker, url, 100, worker.internals.BLOCK_BYTES + 100);

    const name = blockCacheOf(worker, url);
    const entries = blockEntries(worker, name);
    assert.ok(entries.length > 0, "nothing was stored; the rest of this test would be vacuous");
    for (const [key, { response }] of entries) {
      // The harness's Cache.put throws on a 206, as the spec requires, so the
      // fetch above completing already proves no 206 reached a put. This pins
      // the stronger property: what is stored is a plain synthetic 200.
      assert.equal(response.status, 200, `${key} is stored as ${response.status}`);
      assert.ok(
        key.startsWith(`${ORIGIN}__squallar_blk__/`),
        `${key} is not a synthetic app-origin key`,
      );
    }
    for (const { url: stored } of worker.cachedEntries()) {
      assert.ok(
        !stored.includes("tiles.squallar.app"),
        `${stored}: the archive's own URL must never be a cache key — Cache.match is range-blind`,
      );
    }
  });

  it("serves a browsed span again without touching the network", async () => {
    const worker = await bootWorker();
    const url = worker.internals.ARCHIVE_URLS[0];
    const B = worker.internals.BLOCK_BYTES;
    const bytes = archiveBytes(3 * B);
    publishArchive(worker.network, url, bytes);

    await fetchRange(worker, url, 0, B + 5000);
    const before = archiveRequests(worker.network, url);

    // A different span over the same blocks: served from block blobs alone.
    const again = await fetchRange(worker, url, 50, B - 1);
    assert.ok((await bodyOf(again)).equals(Buffer.from(bytes.slice(50, B))));
    assert.equal(
      archiveRequests(worker.network, url),
      before,
      "a span already held in blocks was re-fetched",
    );
  });

  it("keeps generations apart: each archive answers with its own bytes", async () => {
    const worker = await bootWorker();
    const [basemap, terrain] = worker.internals.ARCHIVE_URLS;
    const basemapBytes = archiveBytes(200_000, 7);
    const terrainBytes = archiveBytes(200_000, 3);
    publishArchive(worker.network, basemap, basemapBytes);
    publishArchive(worker.network, terrain, terrainBytes);

    const one = await fetchRange(worker, basemap, 0, 100);
    const two = await fetchRange(worker, terrain, 0, 100);
    assert.ok((await bodyOf(one)).equals(Buffer.from(basemapBytes.slice(0, 101))));
    assert.ok((await bodyOf(two)).equals(Buffer.from(terrainBytes.slice(0, 101))));

    const names = (await worker.cacheNames()).filter((n) =>
      n.startsWith(worker.internals.BLOCK_CACHE_PREFIX),
    );
    assert.equal(new Set(names).size, 2, `two archives, ${names.length} block caches: ${names}`);
  });

  it("keys a part and its monolith apart inside one generation", async () => {
    const worker = await bootWorker();
    const terrain = worker.internals.ARCHIVE_URLS[1];
    publishArchive(worker.network, terrain, archiveBytes(100_000, 1));
    publishArchive(worker.network, `${terrain}.part000`, archiveBytes(100_000, 2));

    await fetchRange(worker, terrain, 0, 15);
    await fetchRange(worker, `${terrain}.part000`, 0, 15);

    const entries = blockEntries(worker, blockCacheOf(worker, terrain));
    assert.equal(entries.length, 2, "the part overwrote the monolith's block, or vice versa");
    const keys = entries.map(([key]) => key);
    assert.ok(
      keys.some((k) => k.includes("part000")) && keys.some((k) => !k.includes("part000")),
      `block 0 of .part000 is not block 0 of the monolith: ${keys}`,
    );
  });

  it("survives a deploy: cachesToKeep names the current generations", async () => {
    // THE hazard this feature lives or dies on: every `squallar-` cache
    // `cachesToKeep` does not name is purged on every deploy, and the symptom
    // of forgetting the entry is a slow map, never an error. Same trigger
    // discipline as the speculative-cache test above: the purge runs on a
    // DEPLOY, so one is published.
    let worker = await bootWorker({ tag: "A" });
    const url = worker.internals.ARCHIVE_URLS[0];
    const B = worker.internals.BLOCK_BYTES;
    const bytes = archiveBytes(3 * B);
    publishArchive(worker.network, url, bytes);
    await fetchRange(worker, url, 0, B + 100);

    const name = blockCacheOf(worker, url);
    assert.ok((await worker.cacheNames()).includes(name), "no block cache before the deploy");

    // A stale generation, seeded exactly as a previous publish would leave it.
    const stale = worker.internals.blockCacheName("/basemap/omt-19990101.pmtiles");
    await (await worker.caches.open(stale)).put(
      new Request(`${ORIGIN}__squallar_blk__/old/x/0`),
      new Response("old"),
    );

    publishDeploy(worker.network, ORIGIN, "B");
    await worker.message({ type: "squallar:check-update" });
    worker = await restartWorker(worker);

    const names = await worker.cacheNames();
    assert.ok(
      names.includes(name),
      `the deploy purged ${name}; browsed areas surviving a redeploy is the point of the block cache`,
    );
    assert.ok(
      !names.includes(stale),
      `${stale} survived the deploy; stale generations must be retired by the same purge`,
    );

    // And the blocks still serve, without one archive request.
    const before = archiveRequests(worker.network, url);
    const event = await fetchRange(worker, url, 0, B - 1);
    assert.ok((await bodyOf(event)).equals(Buffer.from(bytes.slice(0, B))));
    assert.equal(archiveRequests(worker.network, url), before, "the blocks were re-downloaded");
  });

  it("retires a stale generation on activate, without waiting for a deploy", async () => {
    // A deploy that changes ARCHIVE_URLS changes this worker's bytes, so the
    // browser reinstalls and ACTIVATES it — but the validator token may not
    // have moved when the purge matters. Unlike the deploy purge above,
    // `purgeStaleBlockCaches` runs on every activate.
    let worker = await bootWorker({ tag: "A" });
    const url = worker.internals.ARCHIVE_URLS[0];
    publishArchive(worker.network, url, archiveBytes(100_000));
    await fetchRange(worker, url, 0, 100);
    const current = blockCacheOf(worker, url);

    const stale = worker.internals.blockCacheName("/terrain/00000000-20250101/old.pmtiles");
    await (await worker.caches.open(stale)).put(
      new Request(`${ORIGIN}__squallar_blk__/old/y/0`),
      new Response("old"),
    );

    worker = await restartWorker(worker);
    await worker.activate();

    const names = await worker.cacheNames();
    assert.ok(!names.includes(stale), `${stale} survived an activate`);
    assert.ok(names.includes(current), "the activate purged a CURRENT generation");
    assert.ok(blockEntries(worker, current).length > 0, "the current generation's blocks were emptied");
  });

  it("falls through to the network when the cache itself breaks", async () => {
    // The outermost layer of the block path is the fallback: a broken cache
    // must never break the map. The origin's own 206 is the control that the
    // response the page sees is the network's, not a half-assembled one.
    const worker = await bootWorker();
    const url = worker.internals.ARCHIVE_URLS[0];
    const bytes = archiveBytes(100_000);
    publishArchive(worker.network, url, bytes);

    const realOpen = worker.caches.open.bind(worker.caches);
    worker.caches.open = async (name) => {
      if (name.startsWith(worker.internals.BLOCK_CACHE_PREFIX)) {
        throw new Error("cache storage exploded");
      }
      return realOpen(name);
    };

    const event = await fetchRange(worker, url, 100, 300);
    assert.equal(event.handled, true);
    const response = await event.response;
    assert.equal(response.status, 206, "the fallback must yield the network's own 206");
    assert.equal(response.headers.get("content-range"), `bytes 100-300/${bytes.length}`);
    assert.ok(Buffer.from(await response.arrayBuffer()).equals(Buffer.from(bytes.slice(100, 301))));
  });

  it("passes an origin that ignores Range through, buffering and storing nothing", async () => {
    // A `200` here carries the WHOLE archive — the planet basemap is ~125 GB —
    // so the worker must hand the origin's answer over untouched and let the
    // reader run its own not-ranged retry discipline.
    const worker = await bootWorker();
    const url = worker.internals.ARCHIVE_URLS[0];
    const bytes = archiveBytes(50_000);
    publishArchive(worker.network, url, bytes, { ignoreRange: true });

    const event = await fetchRange(worker, url, 0, 15);
    const response = await event.response;
    assert.equal(response.status, 200, "the origin's non-ranged answer must arrive unmodified");
    assert.equal((await response.arrayBuffer()).byteLength, bytes.length);
    assert.equal(
      worker.caches.countEntries((n) => n.startsWith(worker.internals.BLOCK_CACHE_PREFIX)),
      0,
      "something from a non-206 answer was stored",
    );
  });

  it("clamps a span past the end of the file exactly as an origin would", async () => {
    const worker = await bootWorker();
    const url = worker.internals.ARCHIVE_URLS[0];
    const size = 100_000; // one full block, one short final block
    const bytes = archiveBytes(size);
    publishArchive(worker.network, url, bytes);

    const event = await fetchRange(worker, url, size - 10, size + 65_000);
    const response = await event.response;
    assert.equal(response.status, 206);
    assert.equal(
      response.headers.get("content-range"),
      `bytes ${size - 10}-${size - 1}/${size}`,
      "the Content-Range must describe the clamped bytes actually returned",
    );
    assert.ok(Buffer.from(await response.arrayBuffer()).equals(Buffer.from(bytes.slice(size - 10))));

    // Entirely past EOF: the fallback lets the origin answer 416 itself.
    const B = worker.internals.BLOCK_BYTES;
    const past = await fetchRange(worker, url, 4 * B, 4 * B + 100);
    assert.equal((await past.response).status, 416);
  });

  it("degrades to pass-through when storage is tight, and still serves the tile", async () => {
    const worker = await bootWorker();
    const url = worker.internals.ARCHIVE_URLS[0];
    const bytes = archiveBytes(200_000);
    publishArchive(worker.network, url, bytes);

    worker.scope.navigator = {
      storage: { estimate: async () => ({ usage: 0, quota: worker.internals.BLOCK_BYTES }) },
    };
    const tight = await fetchRange(worker, url, 0, 99);
    assert.ok((await bodyOf(tight)).equals(Buffer.from(bytes.slice(0, 100))), "the tile must never fail");
    assert.equal(
      worker.caches.countEntries((n) => n.startsWith(worker.internals.BLOCK_CACHE_PREFIX)),
      0,
      "a block was written into a nearly-full origin",
    );

    // The control that the guard reads the estimate rather than never storing.
    worker.scope.navigator = {
      storage: { estimate: async () => ({ usage: 0, quota: 8 * 1024 * 1024 * 1024 }) },
    };
    await fetchRange(worker, url, 0, 99);
    assert.ok(
      worker.caches.countEntries((n) => n.startsWith(worker.internals.BLOCK_CACHE_PREFIX)) > 0,
      "a roomy origin stored nothing; the tight case above is measuring the wrong thing",
    );
  });

  it("evicts oldest-touched blocks over the budget, and only those", async () => {
    const worker = await bootWorker();
    const url = worker.internals.ARCHIVE_URLS[0];
    const B = worker.internals.BLOCK_BYTES;
    const bytes = archiveBytes(10 * B);
    publishArchive(worker.network, url, bytes);

    // Six blocks written in order 0..5, then block 0 touched again by a read.
    for (let i = 0; i < 6; i += 1) await fetchRange(worker, url, i * B, i * B + 9);
    await fetchRange(worker, url, 0, 9);

    await worker.internals.enforceBlockBudget(3 * B);

    // Blocks 1, 2, 3 are the three oldest-touched; a re-read of one pays the
    // network again, while the freshly-touched block 0 does not.
    let before = archiveRequests(worker.network, url);
    await fetchRange(worker, url, 0, 9);
    assert.equal(archiveRequests(worker.network, url), before, "the freshly-touched block was evicted");

    before = archiveRequests(worker.network, url);
    const refetched = await fetchRange(worker, url, B, B + 9);
    assert.ok((await bodyOf(refetched)).equals(Buffer.from(bytes.slice(B, B + 10))));
    assert.equal(
      archiveRequests(worker.network, url),
      before + 1,
      "the oldest-touched block survived while the budget was exceeded",
    );
  });
});
