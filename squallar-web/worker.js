/*
 * The rasterization worker's bootstrap.
 *
 * Radar rasterization is ~160-190 ms per Level II frame at the browser's
 * 1024x1024 (see `squallar-web/src/lib.rs`), and it used to run on the main
 * thread because wasm32-unknown-unknown has no threads. It runs here instead;
 * `squallar_worker::offload` posts the work and falls back to running it
 * inline whenever this file, or the module it loads, does not come up.
 *
 * This is a MODULE worker (`new Worker(url, { type: "module" })`), because
 * `wasm-pack build --target web` emits an ES module and there is no other way
 * to `import` it. Classic workers would need `--target no-modules` and a second
 * build of the same crate.
 *
 * It loads the SAME `pkg/squallar_web.js` the page does, instantiated a second
 * time. That is deliberate: `sw.js` pins each client to one shell generation
 * precisely because a mismatched (glue, wasm) pair fails with a `LinkError`,
 * and a second wasm artifact would double the surface that machinery has to
 * keep atomic. The cost is one more compile of the module and one more linear
 * memory, both off the main thread.
 *
 * THE IMPORTS ARE DYNAMIC, AND CARRY THIS WORKER'S QUERY STRING. This worker
 * is its own service-worker client, and so is every rayon thread it starts
 * and the tile lane it starts; a page pinned to one shell generation used to
 * get a worker tree from another when a deploy landed mid-boot, because the
 * service worker could pin the page's client id and nothing below it (the
 * rayon threads' glue imports carry no client id at all in Chromium). The
 * page therefore starts this file at `worker.js?pin=<key>`, and every shell
 * asset this file loads is asked for with the same query: `heap.js`, the
 * glue, and the module -- fetched here and handed to `initWithHeap`, because
 * left to itself it resolves `MODULE_PATH` against `heap.js`'s own URL with
 * the query dropped. The glue's `import.meta.url` is then the keyed URL,
 * which is what wasm-bindgen-rayon's helper hands each thread to import, so
 * the threads carry it without knowing. `sw.js` (`SHELL_PIN_PARAM`) answers
 * every request carrying the key from the generation it recorded for it at
 * first sight. A static `import` cannot spell a query it does not know at
 * parse time, which is why these are `import()`. The build-token handshake
 * in `worker_protocol.rs` stays as the last line: it refuses a pair the pin
 * somehow missed.
 *
 * Every path is relative. The site is served from a project-Pages subpath, so a
 * root-absolute URL resolves under a local server and 404s in production;
 * `tests/pwa_assets.rs` fails over one.
 */

/* `?pin=<key>` as the page spelled it; empty for a worker opened by hand. */
const shellPin = self.location.search;

/*
 * No top-level await: a rejected TLA leaves the worker alive but inert, and the
 * page would sit waiting for a `hello` that is never coming. Reporting the
 * failure lets it give up and rasterize on the main thread immediately.
 */
(async function () {
  const heap = await import("./heap.js" + shellPin);
  const glue = await import("./pkg/squallar_web.js" + shellPin);

  /*
   * Where this worker's ladder STARTS: the rung the page's own ladder
   * constructed, handed over on `self.name` because that is the only channel
   * a worker can read synchronously at the top of its own script. The worker
   * states no figure of its own; `heap.js` owns the rungs.
   *
   * The heap is still this instance's own: it walks the ladder separately
   * and may stop lower than the page did, because it constructs in the same
   * process after the page's memory already holds its reservation.
   *
   * `null` -- a worker opened directly, or started by a page from a build
   * before this -- starts from the top rung.
   */
  const ladderStartBytes = heap.heapFromName(self.name) ?? heap.LADDER_BYTES[0];

  /*
   * What the instance ACTUALLY got -- its ladder answer, or the module's
   * declared bound when every rung refused and the glue built its own. It
   * rides back to the page on the hello (`worker_protocol::MEMMAX`), because
   * the page only knows where this ladder started.
   */
  const heapMaxInForce = await heap.initWithHeap(glue.default, ladderStartBytes, undefined, () =>
    fetch(new URL("./pkg/squallar_web_bg.wasm" + shellPin, self.location.href)),
  );

  /*
   * Rayon's threads (WS3b). `initThreadPool` spawns `squallarRayonThreads()`
   * NESTED Workers over this worker's own shared linear memory, which needs
   * cross-origin isolation -- the CloudFront Response Headers Policy on
   * squallar.app emits COOP `same-origin` + COEP `require-corp`.
   *
   * The failure arm is a fallback and not a `fatal`. `squallar_radar::par` is
   * rayon on every target now, so a worker with NO global pool panics on the
   * first job rather than serving it slowly; `squallarRayonSerialPool` makes
   * the calling thread its own one-thread pool, which is exactly the speed
   * this worker ran at before WS3b. A browser without `SharedArrayBuffer`,
   * without nested Workers, or served without the isolation headers keeps
   * rasterizing off the main thread -- it just stops going faster.
   */
  await glue.initThreadPool(glue.squallarRayonThreads()).catch(function (e) {
    console.warn(
      "squallar: no rayon thread pool (" +
        String(e) +
        "); rasterizing on one thread",
    );
    glue.squallarRayonSerialPool();
  });

  glue.squallar_worker_main(heapMaxInForce);
})().catch(function (e) {
  self.postMessage({ kind: "fatal", error: String(e) });
});
