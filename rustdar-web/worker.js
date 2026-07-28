/*
 * The rasterization worker's bootstrap.
 *
 * Radar rasterization is ~160-190 ms per Level II frame at the browser's
 * 1024x1024 (see `rustdar-web/src/lib.rs`), and it used to run on the main
 * thread because wasm32-unknown-unknown has no threads. It runs here instead;
 * `rustdar_frontend::offload` posts the work and falls back to running it
 * inline whenever this file, or the module it loads, does not come up.
 *
 * This is a MODULE worker (`new Worker(url, { type: "module" })`), because
 * `wasm-pack build --target web` emits an ES module and there is no other way
 * to `import` it. Classic workers would need `--target no-modules` and a second
 * build of the same crate.
 *
 * It loads the SAME `pkg/rustdar_web.js` the page does, instantiated a second
 * time. That is deliberate: `sw.js` pins each client to one shell generation
 * precisely because a mismatched (glue, wasm) pair fails with a `LinkError`,
 * and a second wasm artifact would double the surface that machinery has to
 * keep atomic. The cost is one more compile of the module and one more linear
 * memory, both off the main thread. The residual risk — a worker, being its own
 * service-worker client, fetching a different generation than the page — is
 * caught by the build-token handshake in `worker_protocol.rs`.
 *
 * Every path is relative. The site is served from a project-Pages subpath, so a
 * root-absolute URL resolves under a local server and 404s in production;
 * `.github/scripts/check-relative-paths.py` fails the build over one.
 */
import init, { rustdar_worker_main } from "./pkg/rustdar_web.js";

/*
 * No top-level await: a rejected TLA leaves the worker alive but inert, and the
 * page would sit waiting for a `hello` that is never coming. Reporting the
 * failure lets it give up and rasterize on the main thread immediately.
 */
init()
  .then(function () {
    rustdar_worker_main();
  })
  .catch(function (e) {
    self.postMessage({ kind: "fatal", error: String(e) });
  });
