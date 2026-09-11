#!/usr/bin/env python3
"""
serve.py -- static file server for the squallar web bundle (rig edition).

Serves the repo's squallar-web/ (or --dir) with correct
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
                      * with --seed-local-storage '<json>', writes the given
                        localStorage keys BEFORE any app script runs -- the
                        Tier-2 gate pins its scene with
                        {"squallar.ui": "{\"site\":\"KTLX\"}"} so both browsers
                        boot the same site instead of their own defaults.
  /worker.js        (only when instrumenting, default on) the rasterization
                    worker with a prelude that relays self errors,
                    unhandledrejections and console.error/warn over
                    BroadcastChannel "__rig" to the page. NOTE: static
                    `import` declarations hoist above the prelude, so module
                    evaluation of pkg/squallar_web.js is NOT covered; init()
                    and everything later is. Disable with --no-instrument-worker.

                    With --doctor-first-worker, the FIRST /worker.js request
                    instead returns a stub module that posts
                    {kind: "hello", token: "doctored/0/deadbeef"} -- a token
                    no real build can produce -- and every later request
                    returns the real file. This is the Tier-2 doctored-token
                    leg: the page must detect the mismatch, terminate the
                    stub, and respawn onto the real worker. The respawn
                    REFETCHES /worker.js, and every response here carries
                    Cache-Control: no-store (see end_headers), so the refetch
                    can never be cache-served the stub -- without that the
                    backoff ladder would deadlock on the stub forever.

  --pin-clock 2026-09-07T07:22:00Z pins both preludes' wall clock: the page
                    and the worker read that instant plus real elapsed time
                    from Date.now() / new Date(), so a loop seeded on a site
                    lists one fixed archive window every run. The Tier-2
                    `long` leg pins the KTLX VCP 212 window that first hit
                    the page's 1 GiB linear-memory wall (run_tier2.sh).

  /index.html, /sw.js and everything else are served byte-identical to disk.

Programmatic use:
    import serve
    httpd, thread = serve.start_server("/path/to/squallar-web", port=0)
    url = "http://127.0.0.1:%d/index-rig.html" % httpd.server_address[1]
    ...
    serve.stop_server(httpd, thread)

CLI use (prints exactly one stdout line when ready, then serves until
SIGTERM/SIGINT):
    python3 serve.py --dir squallar-web \
        --port 0 --log out/serve.log
    # stdout: RIG-SERVE-READY <port> http://127.0.0.1:<port>/

TLS (WO-5, the mobile-row blocker): a phone on plain LAN HTTP is not a
secure context, so no crossOriginIsolated, no SharedArrayBuffer, a dead
rasterization worker, and `run_here` on the page thread -- the mobile legs
would silently measure a threading configuration the app never ships in.
Three ways to serve https:

  --tls-cert C --tls-key K   a cert you provisioned (mkcert is the intended
                             tool -- its CA is installable on phones)
  --tls                      generate a throwaway self-signed pair via the
                             openssl CLI (SANs: localhost, 127.0.0.1, this
                             box's LAN IP). Desktop drivers accept it via
                             acceptInsecureCerts; PHONES DO NOT -- see below.
  (neither)                  plain http, unchanged

Phone provisioning (do this once per test device; safaridriver rejects
acceptInsecureCerts, and Android Chrome has no such switch for a manual
run):
  1. host:   mkcert -install && mkcert <LAN-IP> localhost 127.0.0.1
  2. host:   serve with --tls-cert <LAN-IP>+2.pem --tls-key <LAN-IP>+2-key.pem
             --host 0.0.0.0
  3. phone:  copy the CA from `mkcert -CAROOT` (rootCA.pem) to the device and
             install it -- iOS: AirDrop/mail the file, Settings > General >
             VPN & Device Management > install profile, THEN Settings >
             General > About > Certificate Trust Settings > enable full
             trust. Android: Settings > Security > More > Install from
             device storage > CA certificate (accept the warning).
  4. phone:  browse to https://<LAN-IP>:<port>/index-rig.html -- the page
             must report crossOriginIsolated=true (drive.py prints it as a
             denominator on every leg; a row without it is invalid).
"""

import argparse
import functools
import http.server
import json
import os
import signal
import socket
import ssl
import subprocess
import sys
import threading

# Resolved from THIS FILE (`<root>/.github/browser-rig/serve.py`), never from an
# absolute path: the one that stood here named `projects/squallar`, which the
# `rustdar` -> `squallar` rename moved in the wrong direction, so it had not
# existed on this box since. Every rig caller passes --dir, so the rot only
# ever bit a hand-run serve.py -- loudly (`FATAL: no index.html under ...`),
# which is why it survived unnoticed rather than unreported.
DEFAULT_DIR = os.path.join(
    os.path.dirname(os.path.dirname(os.path.dirname(
        os.path.abspath(__file__)))),
    "squallar-web")


def lan_ip():
    """This box's LAN address, for the self-signed SAN list. A UDP connect
    never sends a packet; it only asks the kernel which source address the
    default route would use."""
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        try:
            s.connect(("192.0.2.1", 80))  # TEST-NET-1; nothing is sent
            return s.getsockname()[0]
        finally:
            s.close()
    except OSError:
        return None


def selfsigned_pair(out_dir):
    """A throwaway self-signed cert/key via the openssl CLI (mkcert-less
    fallback). Fresh every call -- 30 days of validity on a file nobody
    keeps. Desktop drivers accept it through acceptInsecureCerts; phones
    need the mkcert route in the module doc instead."""
    os.makedirs(out_dir, exist_ok=True)
    cert = os.path.join(out_dir, "rig-selfsigned.crt")
    key = os.path.join(out_dir, "rig-selfsigned.key")
    sans = ["DNS:localhost", "IP:127.0.0.1"]
    ip = lan_ip()
    if ip:
        sans.append("IP:%s" % ip)
    r = subprocess.run(
        ["openssl", "req", "-x509", "-newkey", "rsa:2048", "-sha256",
         "-nodes", "-days", "30", "-keyout", key, "-out", cert,
         "-subj", "/CN=squallar-rig",
         "-addext", "subjectAltName=%s" % ",".join(sans)],
        capture_output=True, text=True)
    if r.returncode != 0:
        raise RuntimeError("openssl self-signed generation failed rc=%d: %s"
                           % (r.returncode, r.stderr.strip()[-400:]))
    return cert, key

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
# deferred module script. __RIG_BLOCK_SW__ is replaced with true/false;
# __RIG_SEED_LS__ with a JSON object of localStorage seeds (or null). Seeding
# happens here, in the first script, which is what guarantees "before any app
# script": the app reads its config during boot, so a seed written any later
# would race it.
PAGE_PRELUDE = b"""<script>/* squallar rig prelude (injected by serve.py, repo untouched) */
(function () {
  "use strict";
  // __RIG_PIN_CLOCK_MS__ (see --pin-clock): when not null, this page's wall
  // clock is pinned. Every `Date.now()` and argument-less `new Date()` --
  // which is what the app's chrono `Utc::now()` compiles to on wasm, and so
  // what a radar loop's listing window hangs off -- reads the pinned instant
  // plus the real time elapsed since this ran. Installed BEFORE this
  // prelude's own first `Date.now()`, so the rig's timestamps and the app's
  // clock are one clock: every page-side comparison (frame-progress, stale
  // readings, t0) stays valid and only the epoch moves. `new Date(x)` and
  // `Date.parse`/`Date.UTC` are untouched, so a stamp the app READS is
  // interpreted as before; only what it asks for as "now" moves.
  var PIN = __RIG_PIN_CLOCK_MS__;
  if (PIN !== null) {
    var RealDate = Date;
    var off = PIN - RealDate.now();
    var PinnedDate = function Date(...args) {
      if (!new.target) return String(new PinnedDate());
      return args.length ? new RealDate(...args) : new RealDate(RealDate.now() + off);
    };
    PinnedDate.prototype = RealDate.prototype;
    PinnedDate.now = function () { return RealDate.now() + off; };
    PinnedDate.parse = RealDate.parse;
    PinnedDate.UTC = RealDate.UTC;
    window.Date = PinnedDate;
    window.__rig_pin_clock = { pin_ms: PIN, offset_ms: off };
  }
  // THE PAGE'S CLOCK, REACHABLE FROM A WEBDRIVER SCRIPT. A driver's
  // `execute` does not always run where this prelude ran: geckodriver
  // evaluates in a Marionette sandbox whose `Date` is the untouched
  // intrinsic, so `Date.now()` there reads the REAL clock while every
  // timestamp this prelude stamps is on the pinned one. Subtracting the two
  // is how a live frame loop was reported as stopped for ~23 h on every
  // pinned firefox leg. An expando IS reachable from that sandbox (measured
  // on geckodriver 0.37.1 / firefox 154: `window.__rig_now()` pinned,
  // bare `Date.now()` real, `window.Date === Date` false), so a driver that
  // wants THIS page's clock calls this and never its own `Date`.
  // Defined whether or not a pin is installed, so the caller has one route
  // rather than two and an unpinned leg exercises the same one.
  window.__rig_now = function () { return Date.now(); };
  var E = (window.__rig_errors = []);
  // 1200 entries, and it STAYS 1200. The cap is a page-memory bound, not a
  // display window: `msg` is truncated at 2000 chars, so the ring's worst
  // case is already ~2.4 MB of live page heap, and the scenes this rig runs
  // hardest (`huge`) are the ones that trap the wasm allocator at a 1 GiB
  // linear-memory ceiling. Growing the ring spends the budget of the leg
  // being measured. What was actually broken was the EXPORT -- the driver
  // shipped 60 of these -- and that costs nothing on the page.
  var C = (window.__rig_console = []);
  // The gesture player's markers, kept OUT of the console ring buffer.
  //
  // `C` holds the last 1200 console entries, and the app logs frame telemetry
  // every frame: at 54-175 Hz that buffer turns over in well under ten
  // seconds. A `loop complete` marker arrives once per 20 s, so by the time
  // the driver next polls, earlier markers have been evicted and the scraped
  // `loops_completed` is not a count of loops that happened -- it is a count
  // of loops still visible in a window that scrolls faster than they arrive.
  //
  // MEASURED: a 90 s leg on linux firefox and a 140 s leg on macOS firefox
  // both reported 2 loops, as did a 46 s leg; raising the window bought
  // nothing, because the limit was never time. Worse, the loss rate is a
  // function of how fast the app logs, which differs per browser -- so the
  // undercount was browser-correlated, which is the shape that costs validity
  // rather than precision.
  //
  // Markers are rare (one per 20 s) so this array is bounded generously
  // rather than by turnover; a leg would have to run eleven hours to reach
  // the cap, and the cap exists only so a runaway cannot exhaust memory.
  var M = (window.__rig_marks = []);
  window.__rig = { t0: Date.now(), block_sw: __RIG_BLOCK_SW__ };
  var seed = __RIG_SEED_LS__;
  if (seed) {
    try {
      for (var k in seed) window.localStorage.setItem(k, String(seed[k]));
      window.__rig.seeded = Object.keys(seed);
    } catch (e) {
      E.push({ t: Date.now(), kind: "rig.seed", msg: String(e) });
    }
  }
  // `arr.evicted` is the count this ring has DROPPED, kept as a plain
  // property on the array. `JSON.stringify` and the WebDriver value
  // serializer both ignore non-index properties on an array, so nothing that
  // reads the ring as a list sees it -- but a probe that asks for it by name
  // can, and that is what lets the exporter say "1200 entries, and 3140 more
  // were logged and evicted" instead of leaving the reader to infer a full
  // ring is a complete one. Without it `arr.length` SATURATES at the cap, so
  // "how many existed" is unanswerable from the artifact.
  function push(arr, o) {
    try {
      arr.push(o);
      if (arr.length > 1200) {
        var drop = arr.length - 1200;
        arr.splice(0, drop);
        arr.evicted = (arr.evicted || 0) + drop;
      }
    } catch (_) {}
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
  // Cheap substring test first; the marker sentences are the two the gesture
  // player emits, and `mark` runs on EVERY console line so it must not regex.
  function mark(t, m) {
    if (m.indexOf("gesture script ") !== 0) return;
    if (m.indexOf(" loop complete: ") < 0 && m.indexOf(" begin") < 0) return;
    try { if (M.length < 20000) M.push({ t: t, msg: m }); } catch (_) {}
  }
  ["error", "warn", "info", "log", "debug"].forEach(function (lvl) {
    var orig = console[lvl] ? console[lvl].bind(console) : null;
    console[lvl] = function () {
      var m = fmt(arguments);
      var t = Date.now();
      push(C, { t: t, lvl: lvl, msg: m });
      mark(t, m);
      if (lvl === "error") push(E, { t: t, kind: "console.error", msg: m });
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
# so it runs after pkg/squallar_web.js module evaluation but before init()
# resolves -- runtime errors and console.error/warn in the worker are covered.
WORKER_PRELUDE = b"""/* squallar rig worker prelude (injected by serve.py, repo untouched).
   Static `import` declarations below are hoisted and evaluate BEFORE this
   code; everything from init() onward is covered. */
try {
  (function () {
    "use strict";
    // __RIG_PIN_CLOCK_MS__ (see --pin-clock): when not null, this page's wall
    // clock is pinned. Every `Date.now()` and argument-less `new Date()` --
    // which is what the app's chrono `Utc::now()` compiles to on wasm, and so
    // what a radar loop's listing window hangs off -- reads the pinned instant
    // plus the real time elapsed since this ran. Installed BEFORE this
    // prelude's own first `Date.now()`, so the rig's timestamps and the app's
    // clock are one clock: every page-side comparison (frame-progress, stale
    // readings, t0) stays valid and only the epoch moves. `new Date(x)` and
    // `Date.parse`/`Date.UTC` are untouched, so a stamp the app READS is
    // interpreted as before; only what it asks for as "now" moves.
    var PIN = __RIG_PIN_CLOCK_MS__;
    if (PIN !== null) {
      var RealDate = Date;
      var off = PIN - RealDate.now();
      var PinnedDate = function Date(...args) {
        if (!new.target) return String(new PinnedDate());
        return args.length ? new RealDate(...args) : new RealDate(RealDate.now() + off);
      };
      PinnedDate.prototype = RealDate.prototype;
      PinnedDate.now = function () { return RealDate.now() + off; };
      PinnedDate.parse = RealDate.parse;
      PinnedDate.UTC = RealDate.UTC;
      self.Date = PinnedDate;
      self.__rig_pin_clock = { pin_ms: PIN, offset_ms: off };
    }
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


def parse_pin_clock(text):
    """`2026-09-07T07:22:00Z` -> milliseconds since the epoch. UTC only and
    the `Z` is required: a pin without a zone would mean a different instant
    on every runner, which is the one thing a pin exists to prevent."""
    import datetime
    if not text.endswith("Z"):
        raise ValueError("%r must be UTC and end in Z" % text)
    try:
        when = datetime.datetime.strptime(text, "%Y-%m-%dT%H:%M:%SZ")
    except ValueError:
        raise ValueError("%r is not YYYY-MM-DDTHH:MM:SSZ" % text)
    when = when.replace(tzinfo=datetime.timezone.utc)
    return int(when.timestamp() * 1000)


def pin_clock_literal(pin_clock_ms):
    """The JS literal the preludes get for __RIG_PIN_CLOCK_MS__: an integer
    number of milliseconds since the epoch, or `null` for real time."""
    return (b"null" if pin_clock_ms is None
            else str(int(pin_clock_ms)).encode("ascii"))


def transform_index(raw, block_sw=True, seed_local_storage=None,
                    pin_clock_ms=None):
    """index.html bytes -> instrumented page bytes."""
    seed = (json.dumps(seed_local_storage).encode("utf-8")
            if seed_local_storage else b"null")
    prelude = PAGE_PRELUDE.replace(
        b"__RIG_BLOCK_SW__", b"true" if block_sw else b"false").replace(
        b"__RIG_SEED_LS__", seed).replace(
        b"__RIG_PIN_CLOCK_MS__", pin_clock_literal(pin_clock_ms))
    marker = b"<head>"
    idx = raw.find(marker)
    if idx >= 0:
        cut = idx + len(marker)
        return raw[:cut] + b"\n" + prelude + raw[cut:]
    return prelude + raw  # no <head>: prepend (still first script)


def transform_worker(raw, pin_clock_ms=None):
    """worker.js bytes -> instrumented worker bytes."""
    prelude = WORKER_PRELUDE.replace(
        b"__RIG_PIN_CLOCK_MS__", pin_clock_literal(pin_clock_ms))
    return prelude + b"\n" + raw


# Served for the FIRST /worker.js request under --doctor-first-worker. A
# module worker with no imports; it posts a HELLO whose token no real build
# can produce (build_token is version/protocol/sha -- "doctored" is not a
# version this crate will ever have). worker_port::handle_message must read
# it, log "rasterization worker is a different build", terminate this worker,
# and respawn after the first backoff rung (1000 ms) -- the respawn's refetch
# of /worker.js gets the real file (and Cache-Control: no-store on every
# response means it can never be cache-served this stub again).
DOCTORED_WORKER_STUB = b"""/* squallar rig doctored worker stub \
(served once by serve.py --doctor-first-worker) */
self.postMessage({ kind: "hello", token: "doctored/0/deadbeef" });
"""


class RigHandler(http.server.SimpleHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "squallar-rig/1"

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
                lambda raw: transform_index(raw, self.server.rig_block_sw,
                                            self.server.rig_seed_ls,
                                            self.server.rig_pin_clock_ms),
                "text/html; charset=utf-8")
        if path == "/worker.js" and self.server.rig_doctor_first_worker:
            # Exactly the FIRST request gets the stub (threaded server: the
            # claim is under a lock); every later one falls through to the
            # real file so the respawn can attach a genuine worker.
            with self.server.rig_doctor_lock:
                first = not self.server.rig_doctor_served
                self.server.rig_doctor_served = True
            if first:
                self.log_message("rig: serving DOCTORED worker stub (first "
                                 "/worker.js request)")
                return self._send_bytes(DOCTORED_WORKER_STUB,
                                        "text/javascript; charset=utf-8")
        if path == "/worker.js" and self.server.rig_instrument_worker:
            return self._send_transformed(
                "worker.js",
                lambda raw: transform_worker(raw, self.server.rig_pin_clock_ms),
                "text/javascript; charset=utf-8")
        return super().do_GET()

    def _send_transformed(self, relname, transform, ctype):
        fpath = os.path.join(self.server.rig_dir, relname)
        try:
            with open(fpath, "rb") as f:
                raw = f.read()
        except OSError as e:
            self.send_error(404, "rig: cannot read %s: %s" % (relname, e))
            return
        self._send_bytes(transform(raw), ctype)

    def _send_bytes(self, body, ctype):
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


class RigServer(http.server.ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True


def start_server(directory=DEFAULT_DIR, port=0, host="127.0.0.1",
                 log=None, block_sw=True, instrument_worker=True, coep=False,
                 seed_local_storage=None, doctor_first_worker=False,
                 tls_cert=None, tls_key=None, pin_clock_ms=None):
    """Start serving in a daemon thread. Returns (httpd, thread).
    Stop with stop_server(httpd, thread). Port 0 picks a free port;
    read it from httpd.server_address[1]. With tls_cert+tls_key the
    listening socket is TLS-wrapped and the served scheme is https."""
    handler = functools.partial(RigHandler, directory=directory)
    httpd = RigServer((host, port), handler)
    httpd.rig_tls = bool(tls_cert and tls_key)
    if httpd.rig_tls:
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        ctx.load_cert_chain(tls_cert, tls_key)
        httpd.socket = ctx.wrap_socket(httpd.socket, server_side=True)
    httpd.rig_dir = directory
    httpd.rig_log = log
    httpd.rig_log_lock = threading.Lock()
    httpd.rig_block_sw = block_sw
    httpd.rig_instrument_worker = instrument_worker
    httpd.rig_coep = coep
    httpd.rig_seed_ls = seed_local_storage
    httpd.rig_doctor_first_worker = doctor_first_worker
    httpd.rig_pin_clock_ms = pin_clock_ms
    httpd.rig_doctor_served = False
    httpd.rig_doctor_lock = threading.Lock()
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
    ap.add_argument("--seed-local-storage", default=None, metavar="JSON",
                    help="JSON object of localStorage key -> string value, "
                         "written by the page prelude BEFORE any app script "
                         "runs (the Tier-2 gate pins its scene with "
                         "'{\"squallar.ui\": \"{\\\"site\\\":\\\"KTLX\\\"}\"}')")
    ap.add_argument("--doctor-first-worker", action="store_true",
                    help="answer the FIRST /worker.js request with a stub that "
                         "posts a doctored build token; later requests get the "
                         "real file (the Tier-2 respawn leg)")
    ap.add_argument("--pin-clock", default=None, metavar="ISO8601-UTC",
                    help="pin the page's (and worker's) wall clock: Date.now() "
                         "and `new Date()` read this instant plus the real "
                         "time elapsed since the page loaded, e.g. "
                         "2026-09-07T07:22:00Z. A radar loop seeded on a "
                         "site then lists the same archive window every run "
                         "instead of whatever the site is doing today")
    ap.add_argument("--tls-cert", default=None, metavar="PEM",
                    help="serve https with this certificate (pair with "
                         "--tls-key; mkcert output is the intended input -- "
                         "see the module doc for phone provisioning)")
    ap.add_argument("--tls-key", default=None, metavar="PEM",
                    help="private key for --tls-cert")
    ap.add_argument("--tls", action="store_true",
                    help="serve https with a throwaway self-signed pair "
                         "generated via the openssl CLI (SANs: localhost, "
                         "127.0.0.1, this box's LAN IP). Desktop drivers "
                         "accept it via acceptInsecureCerts; phones need "
                         "the mkcert route instead")
    args = ap.parse_args(argv)

    if bool(args.tls_cert) != bool(args.tls_key):
        print("FATAL: --tls-cert and --tls-key come as a pair",
              file=sys.stderr)
        return 1
    tls_cert, tls_key = args.tls_cert, args.tls_key
    if args.tls and not tls_cert:
        try:
            tls_cert, tls_key = selfsigned_pair(
                os.path.dirname(os.path.abspath(args.log))
                if args.log != "-" else ".")
        except (RuntimeError, OSError) as e:
            print("FATAL: %s" % e, file=sys.stderr)
            return 1

    if not os.path.isfile(os.path.join(args.dir, "index.html")):
        print("FATAL: no index.html under %s" % args.dir, file=sys.stderr)
        return 1

    seed = None
    if args.seed_local_storage:
        try:
            seed = json.loads(args.seed_local_storage)
        except ValueError as e:
            print("FATAL: --seed-local-storage is not JSON: %s" % e,
                  file=sys.stderr)
            return 1
        if not isinstance(seed, dict):
            print("FATAL: --seed-local-storage must be a JSON object",
                  file=sys.stderr)
            return 1

    pin_clock_ms = None
    if args.pin_clock:
        try:
            pin_clock_ms = parse_pin_clock(args.pin_clock)
        except ValueError as e:
            print("FATAL: --pin-clock: %s" % e, file=sys.stderr)
            return 1

    log = sys.stderr if args.log == "-" else open(args.log, "a", buffering=1)
    httpd, thread = start_server(
        directory=args.dir, port=args.port, host=args.host, log=log,
        block_sw=not args.no_block_sw,
        instrument_worker=not args.no_instrument_worker, coep=args.coep,
        seed_local_storage=seed,
        doctor_first_worker=args.doctor_first_worker,
        tls_cert=tls_cert, tls_key=tls_key, pin_clock_ms=pin_clock_ms)
    port = httpd.server_address[1]
    scheme = "https" if httpd.rig_tls else "http"
    # Exactly one machine-parseable stdout line.
    print("RIG-SERVE-READY %d %s://%s:%d/" % (port, scheme, args.host, port),
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
