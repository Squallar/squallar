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
 * The glue is imported dynamically, with this lane's own query string, for
 * the reason `worker.js` imports its own that way: the worker started this
 * file at `tile-lane.js?pin=<key>`, the page's shell-generation key, and the
 * glue this lane loads has to match the module it is handed. Its import is
 * one of the requests the browsers attribute to nobody (Chromium hands the
 * service worker an empty client id for it), so the key in the URL is the
 * only thing that can name its generation.
 *
 * A failed init is reported on the port rather than left silent: the page
 * would otherwise wait for a `lanehello` that never comes. It still costs
 * nothing but the lane -- the page keeps styling tiles on its own thread.
 */

/* `?pin=<key>` as the worker spelled it; empty when opened by hand. */
const shellPin = self.location.search;

self.onmessage = function (event) {
  var d = event.data || {};
  if (d.kind !== "laneinit") return;
  self.onmessage = null;
  import("./pkg/squallar_web.js" + shellPin)
    .then(function (glue) {
      return glue
        .default({ module_or_path: d.module, memory: d.memory })
        .then(function () {
          glue.squallar_tile_lane_main(d.port);
        });
    })
    .catch(function (e) {
      d.port.postMessage({ kind: "fatal", error: String(e) });
    });
};
