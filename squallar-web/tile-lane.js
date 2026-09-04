/*
 * The tile lane's bootstrap: one more nested Worker on the rasterization
 * worker's own linear memory, running the basemap's vector tile batches on a
 * thread whose message loop no radar or model job can hold.
 *
 * Started by the rasterization worker (`squallar-web/src/worker.rs`,
 * `spawn_tile_lane`), never by the page. The one message it receives carries
 * the worker's `WebAssembly.Module` and shared `WebAssembly.Memory` and the
 * lane's end of a `MessageChannel`; `init({ module_or_path, memory })` is the
 * same call wasm-bindgen-rayon's helpers make for the pool's threads, so this
 * instantiation shares the worker's heap, statics and codec rows rather than
 * paying for a heap of its own. The page's end of the port rides the worker's
 * `hello`.
 *
 * A MODULE worker for the reason `worker.js` is one, and every path relative
 * for the same reason (`sw.js` precaches this file beside `worker.js`).
 *
 * A failed init is reported on the port rather than left silent: the page
 * would otherwise wait for a `lanehello` that never comes. It still costs
 * nothing but the lane -- the page keeps styling tiles on its own thread.
 */
import init, { squallar_tile_lane_main } from "./pkg/squallar_web.js";

self.onmessage = function (event) {
  var d = event.data || {};
  if (d.kind !== "laneinit") return;
  self.onmessage = null;
  init({ module_or_path: d.module, memory: d.memory })
    .then(function () {
      squallar_tile_lane_main(d.port);
    })
    .catch(function (e) {
      d.port.postMessage({ kind: "fatal", error: String(e) });
    });
};
