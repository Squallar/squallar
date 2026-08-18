#!/usr/bin/env python3
"""
serve.py -- static file server for the rustdar web bundle (rig edition).

Serves /home/reddragon/projects/rustdar/rustdar-web (or --dir) with correct
MIME types (.wasm -> application/wasm, .js -> text/javascript; module scripts
and instantiateStreaming both care). Never modifies the repo: all
instrumentation happens at response time.

Rig endpoints (byte transforms of repo files, applied per-response):

  /index-rig.html   index.html with a <script> prelude injected as the FIRST
                    script in <head>. The prelude:
                      * collects window.__rig_errors (window.onerror,
                        unhandledrejection, console.error) and
                        window.__rig_console (all console levels, ring buffer)
                      * listens on BroadcastChannel "__rig" so the worker
                        prelude (below) can relay worker-side errors
                      * blocks navigator.serviceWorker.register() with a
                        rejected promise (the app's own handled degradation
                        path: index.html catches and console.warn's). Disable
                        with --no-block-sw.
                      * bumps the resource-timing buffer so boot fetches are
                        observable.
  /worker.js        (only when instrumenting, default on) the rasterization
                    worker with a prelude that relays self errors,
                    unhandledrejections and console.error/warn over
                    BroadcastChannel "__rig" to the page. NOTE: static
                    `import` declarations hoist above the prelude, so module
                    evaluation of pkg/rustdar_web.js is NOT covered; init()
                    and everything later is. Disable with --no-instrument-worker.

  /index.html, /sw.js and everything else are served byte-identical to disk.

Programmatic use:
    import serve
    httpd, thread = serve.start_server("/path/to/rustdar-web", port=0)
    url = "http://127.0.0.1:%d/index-rig.html" % httpd.server_address[1]
    ...
    serve.stop_server(httpd, thread)

CLI use (prints exactly one stdout line when ready, then serves until
SIGTERM/SIGINT):
    python3 serve.py --dir /home/reddragon/projects/rustdar/rustdar-web \
        --port 0 --log out/serve.log
    # stdout: RIG-SERVE-READY <port> http://127.0.0.1:<port>/
"""

import argparse
import functools
import http.server
import os
import signal
import sys
import threading

DEFAULT_DIR = "/home/reddragon/projects/rustdar/rustdar-web"

MIME = {
    ".html": "text/html; charset=utf-8",
    ".js": "text/javascript; charset=utf-8",
    ".mjs": "text/javascript; charset=utf-8",
    ".wasm": "application/wasm",
    ".json": "application/json; charset=utf-8",
    ".webmanifest": "application/manifest+json; charset=utf-8",
    ".map": "application/json",
    ".css": "text/css; charset=utf-8",
    ".png": "image/png",
    ".svg": "image/svg+xml",
    ".ico": "image/x-icon",
    ".txt": "text/plain; charset=utf-8",
}

# Injected right after <head> so it is the first script that runs in the
# document -- before the app's classic script and (trivially) before the
# deferred module script. __RIG_BLOCK_SW__ is replaced with true/false.
PAGE_PRELUDE = b"""<script>/* rustdar rig prelude (injected by serve.py, repo untouched) */
(function () {
  "use strict";
  var E = (window.__rig_errors = []);
  var C = (window.__rig_console = []);
  window.__rig = { t0: Date.now(), block_sw: __RIG_BLOCK_SW__ };
  function push(arr, o) {
    try { arr.push(o); if (arr.length > 1200) arr.splice(0, arr.length - 1200); } catch (_) {}
  }
  function fmt(args) {
    try {
      return Array.prototype.map.call(args, function (x) {
        if (typeof x === "string") return x;
        try { return JSON.stringify(x); } catch (_) { return String(x); }
      }).join(" ").slice(0, 2000);
    } catch (_) { return "<unformattable>"; }
  }
  window.addEventListener("error", function (e) {
    push(E, { t: Date.now(), kind: "window.onerror", msg: String(e.message || e),
              src: (e.filename || "") + ":" + (e.lineno || 0) });
  }, true);
  window.addEventListener("unhandledrejection", function (e) {
    var r = e.reason;
    push(E, { t: Date.now(), kind: "unhandledrejection",
              msg: String((r && (r.stack || r.message)) || r).slice(0, 2000) });
  });
  ["error", "warn", "info", "log", "debug"].forEach(function (lvl) {
    var orig = console[lvl] ? console[lvl].bind(console) : null;
    console[lvl] = function () {
      var m = fmt(arguments);
      push(C, { t: Date.now(), lvl: lvl, msg: m });
      if (lvl === "error") push(E, { t: Date.now(), kind: "console.error", msg: m });
      if (orig) return orig.apply(null, arguments);
    };
  });
  try {
    var bc = new BroadcastChannel("__rig");
    bc.onmessage = function (m) {
      var d = m.data || {};
      if (d.lvl) push(C, d); else push(E, d);
    };
  } catch (_) {}
  try { performance.setResourceTimingBufferSize(4000); } catch (_) {}
  if (window.__rig.block_sw && navigator.serviceWorker) {
    try {
      Object.defineProperty(navigator.serviceWorker, "register", {
        configurable: true,
        value: function () {
          return Promise.reject(new Error("rig: service worker registration blocked for measurement"));
        },
      });
    } catch (_) {
      try {
        navigator.serviceWorker.register = function () {
          return Promise.reject(new Error("rig: service worker registration blocked for measurement"));
        };
      } catch (_2) {}
    }
  }
})();
</script>
"""

# Prepended to worker.js. Static imports in the module hoist above this code,
# so it runs after pkg/rustdar_web.js module evaluation but before init()
# resolves -- runtime errors and console.error/warn in the worker are covered.
WORKER_PRELUDE = b"""/* rustdar rig worker prelude (injected by serve.py, repo untouched).
   Static `import` declarations below are hoisted and evaluate BEFORE this
   code; everything from init() onward is covered. */
try {
  (function () {
    "use strict";
    var bc = null;
    try { bc = new BroadcastChannel("__rig"); } catch (_) {}
    function send(o) { try { if (bc) bc.postMessage(o); } catch (_) {} }
    self.addEventListener("error", function (e) {
      send({ t: Date.now(), kind: "worker.error", msg: String(e.message || e),
             src: (e.filename || "") + ":" + (e.lineno || 0) });
    });
    self.addEventListener("unhandledrejection", function (e) {
      var r = e.reason;
      send({ t: Date.now(), kind: "worker.unhandledrejection",
             msg: String((r && (r.stack || r.message)) || r).slice(0, 2000) });
    });
    ["error", "warn"].forEach(function (lvl) {
      var orig = console[lvl] ? console[lvl].bind(console) : null;
      console[lvl] = function () {
        var s;
        try { s = Array.prototype.map.call(arguments, String).join(" ").slice(0, 2000); }
        catch (_) { s = "<unformattable>"; }
        if (lvl === "error") send({ t: Date.now(), kind: "worker.console.error", msg: s });
        else send({ t: Date.now(), lvl: "warn", msg: "[worker] " + s });
        if (orig) return orig.apply(null, arguments);
      };
    });
  })();
} catch (_) {}
"""


def transform_index(raw, block_sw=True):
    """index.html bytes -> instrumented page bytes."""
    prelude = PAGE_PRELUDE.replace(
        b"__RIG_BLOCK_SW__", b"true" if block_sw else b"false")
    marker = b"<head>"
    idx = raw.find(marker)
    if idx >= 0:
        cut = idx + len(marker)
        return raw[:cut] + b"\n" + prelude + raw[cut:]
    return prelude + raw  # no <head>: prepend (still first script)


def transform_worker(raw):
    """worker.js bytes -> instrumented worker bytes."""
    return WORKER_PRELUDE + b"\n" + raw


class RigHandler(http.server.SimpleHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "rustdar-rig/1"

    def guess_type(self, path):
        _, ext = os.path.splitext(path)
        return MIME.get(ext.lower(), "application/octet-stream")

    def end_headers(self):
        # Fresh bytes every navigation; screenshots/timings must never come
        # from a disk cache entry of a previous run.
        self.send_header("Cache-Control", "no-store")
        if getattr(self.server, "rig_coep", False):
            self.send_header("Cross-Origin-Opener-Policy", "same-origin")
            self.send_header("Cross-Origin-Embedder-Policy", "require-corp")
        super().end_headers()

    def log_message(self, fmt, *args):
        log = getattr(self.server, "rig_log", None)
        if log is None:
            return
        line = "%s %s %s\n" % (self.log_date_time_string(),
                               self.address_string(), fmt % args)
        with self.server.rig_log_lock:
            try:
                log.write(line)
                log.flush()
            except ValueError:  # closed during shutdown
                pass

    def do_GET(self):
        path = self.path.split("?", 1)[0].split("#", 1)[0]
        if path == "/index-rig.html":
            return self._send_transformed(
                "index.html",
                lambda raw: transform_index(raw, self.server.rig_block_sw),
                "text/html; charset=utf-8")
        if path == "/worker.js" and self.server.rig_instrument_worker:
            return self._send_transformed(
                "worker.js", transform_worker, "text/javascript; charset=utf-8")
        return super().do_GET()

    def _send_transformed(self, relname, transform, ctype):
        fpath = os.path.join(self.server.rig_dir, relname)
        try:
            with open(fpath, "rb") as f:
                raw = f.read()
        except OSError as e:
            self.send_error(404, "rig: cannot read %s: %s" % (relname, e))
            return
        body = transform(raw)
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


class RigServer(http.server.ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True


def start_server(directory=DEFAULT_DIR, port=0, host="127.0.0.1",
                 log=None, block_sw=True, instrument_worker=True, coep=False):
    """Start serving in a daemon thread. Returns (httpd, thread).
    Stop with stop_server(httpd, thread). Port 0 picks a free port;
    read it from httpd.server_address[1]."""
    handler = functools.partial(RigHandler, directory=directory)
    httpd = RigServer((host, port), handler)
    httpd.rig_dir = directory
    httpd.rig_log = log
    httpd.rig_log_lock = threading.Lock()
    httpd.rig_block_sw = block_sw
    httpd.rig_instrument_worker = instrument_worker
    httpd.rig_coep = coep
    thread = threading.Thread(target=httpd.serve_forever,
                              name="rig-serve", daemon=True)
    thread.start()
    return httpd, thread


def stop_server(httpd, thread=None):
    httpd.shutdown()
    httpd.server_close()
    if thread is not None:
        thread.join(timeout=5)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[1])
    ap.add_argument("--dir", default=DEFAULT_DIR,
                    help="directory to serve (default: %(default)s)")
    ap.add_argument("--port", type=int, default=0,
                    help="port; 0 = pick a free one (default)")
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--log", default="-",
                    help="request log file path, '-' = stderr (default)")
    ap.add_argument("--no-block-sw", action="store_true",
                    help="let /index-rig.html register the real service worker")
    ap.add_argument("--no-instrument-worker", action="store_true",
                    help="serve worker.js byte-identical to disk")
    ap.add_argument("--coep", action="store_true",
                    help="send COOP/COEP headers (cross-origin isolation; only "
                         "needed if the app ever wants SharedArrayBuffer)")
    args = ap.parse_args(argv)

    if not os.path.isfile(os.path.join(args.dir, "index.html")):
        print("FATAL: no index.html under %s" % args.dir, file=sys.stderr)
        return 1

    log = sys.stderr if args.log == "-" else open(args.log, "a", buffering=1)
    httpd, thread = start_server(
        directory=args.dir, port=args.port, host=args.host, log=log,
        block_sw=not args.no_block_sw,
        instrument_worker=not args.no_instrument_worker, coep=args.coep)
    port = httpd.server_address[1]
    # Exactly one machine-parseable stdout line.
    print("RIG-SERVE-READY %d http://%s:%d/" % (port, args.host, port),
          flush=True)

    stop = threading.Event()
    for sig in (signal.SIGTERM, signal.SIGINT):
        signal.signal(sig, lambda *_: stop.set())
    try:
        stop.wait()
    finally:
        stop_server(httpd, thread)
        if log is not sys.stderr:
            log.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
