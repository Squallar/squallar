#!/usr/bin/env python3
"""
drive.py -- dependency-free W3C WebDriver client for driving the rustdar web
app headless. Python 3 stdlib only (urllib; selenium is NOT installed).

Starts the driver binary itself (chromedriver / geckodriver), creates a
headless session, navigates to the served app, waits for boot (the app removes
#rustdar-status on a successful start() and writes "rustdar failed to start:"
into it on failure), then measures and records:

  * canvas #rustdar-canvas presence + client/buffer size + dpr
  * WebGL renderer probe (software vs hardware) + WebGPU availability
  * requestAnimationFrame delta stats over N frames (p50/p90/p95/p99/max),
    sampled twice: right after settle, and again after a data window
  * viewport + canvas-element screenshots, saved as PNG and analysed with a
    built-in pure-python PNG decoder (blank / near-blank detection)
  * error signal: injected window.__rig_errors / __rig_console (both
    browsers, provided by serve.py's /index-rig.html) plus chromedriver's
    non-standard browser-log endpoint (chromium only; geckodriver has none)

Timing primitive for the instrumented app: poll_global_json() polls a window
global (dot-path) until it holds a JSON-serialisable value -- the future
instrumented build exposes richer stats via such a global, and
`--poll-global NAME` wires it from the CLI.

Exit codes: 0 pass, 2 measured failure (boot/canvas/rAF), 1 unexpected error.

Typical use (see run_smoke.sh for the orchestrated version):
  python3 drive.py --browser chromium --url http://127.0.0.1:8611/index-rig.html \
      --out-dir out --tag chromium
  python3 drive.py --browser firefox --driver /path/to/geckodriver \
      --url http://127.0.0.1:8611/index-rig.html --out-dir out --tag firefox
"""

import argparse
import base64
import json
import os
import shutil
import signal
import socket
import struct
import subprocess
import sys
import time
import traceback
import urllib.error
import urllib.request
import zlib
from collections import Counter

ELEMENT_KEY = "element-6066-11e4-a52e-4f735466cecf"
DEFAULT_CHROMEDRIVER = "/usr/bin/chromedriver"
# The durable location ensure-geckodriver.sh provisions. The gate never relies
# on this default (run_tier2.sh always passes --driver), but a default that
# pointed anywhere ephemeral would rot silently.
DEFAULT_GECKODRIVER = os.path.expanduser(
    "~/.cache/rustdar-ci/geckodriver-0.37.1/geckodriver")
DEFAULT_FIREFOX = "/usr/bin/firefox"


# --------------------------------------------------------------------------
# W3C wire
# --------------------------------------------------------------------------

class WebDriverError(RuntimeError):
    def __init__(self, message, error=None, stacktrace=None, status=None):
        super().__init__(message)
        self.error = error
        self.stacktrace = stacktrace
        self.status = status


class Wire:
    """Minimal JSON-over-HTTP client for a local WebDriver server."""

    def __init__(self, base_url):
        self.base = base_url.rstrip("/")

    def request(self, method, path, body=None, timeout=60.0):
        url = self.base + path
        data = None
        headers = {"Accept": "application/json"}
        if method == "POST" and body is None:
            body = {}
        if body is not None:
            data = json.dumps(body).encode("utf-8")
            headers["Content-Type"] = "application/json; charset=utf-8"
        req = urllib.request.Request(url, data=data, headers=headers,
                                     method=method)
        try:
            with urllib.request.urlopen(req, timeout=timeout) as resp:
                payload = resp.read()
        except urllib.error.HTTPError as e:
            payload = e.read()
            err = msg = stack = None
            try:
                v = json.loads(payload.decode("utf-8", "replace")).get("value") or {}
                err, msg, stack = v.get("error"), v.get("message"), v.get("stacktrace")
            except (ValueError, AttributeError):
                msg = repr(payload[:400])
            raise WebDriverError(
                "%s %s -> HTTP %d: %s: %s" % (method, path, e.code, err, msg),
                error=err, stacktrace=stack, status=e.code) from None
        except (urllib.error.URLError, TimeoutError, ConnectionError, OSError) as e:
            raise WebDriverError(
                "%s %s -> transport error: %s" % (method, path, e)) from None
        try:
            return json.loads(payload.decode("utf-8", "replace")).get("value")
        except ValueError:
            raise WebDriverError(
                "%s %s -> unparseable response: %r" % (method, path, payload[:400]))


class Session:
    def __init__(self, wire, session_id, caps):
        self.wire = wire
        self.sid = session_id
        self.caps = caps or {}

    @classmethod
    def create(cls, wire, capabilities, timeout=150.0):
        v = wire.request("POST", "/session", capabilities, timeout=timeout)
        if not v or "sessionId" not in v:
            raise WebDriverError("session create returned %r" % (v,))
        return cls(wire, v["sessionId"], v.get("capabilities"))

    def cmd(self, method, path, body=None, timeout=60.0):
        return self.wire.request(method, "/session/%s%s" % (self.sid, path),
                                 body, timeout=timeout)

    def set_timeouts(self, script_ms=90000, page_ms=120000, implicit_ms=0):
        self.cmd("POST", "/timeouts", {"script": script_ms,
                                       "pageLoad": page_ms,
                                       "implicit": implicit_ms})
        self.script_timeout_s = script_ms / 1000.0

    def set_window_rect(self, width, height):
        return self.cmd("POST", "/window/rect",
                        {"x": 0, "y": 0, "width": width, "height": height})

    def navigate(self, url, timeout=150.0):
        self.cmd("POST", "/url", {"url": url}, timeout=timeout)

    def title(self):
        return self.cmd("GET", "/title")

    def execute(self, script, args=None, timeout=100.0):
        return self.cmd("POST", "/execute/sync",
                        {"script": script, "args": args or []}, timeout=timeout)

    def execute_async(self, script, args=None, timeout=None):
        if timeout is None:
            timeout = getattr(self, "script_timeout_s", 90.0) + 20.0
        return self.cmd("POST", "/execute/async",
                        {"script": script, "args": args or []}, timeout=timeout)

    def find_element(self, css):
        try:
            v = self.cmd("POST", "/element",
                         {"using": "css selector", "value": css})
        except WebDriverError as e:
            if e.error == "no such element" or e.status == 404:
                return None
            raise
        return v.get(ELEMENT_KEY) if isinstance(v, dict) else None

    def screenshot_b64(self):
        return self.cmd("GET", "/screenshot", timeout=120.0)

    def element_screenshot_b64(self, element_id):
        return self.cmd("GET", "/element/%s/screenshot" % element_id,
                        timeout=120.0)

    def delete(self):
        try:
            self.wire.request("DELETE", "/session/%s" % self.sid, timeout=30.0)
        except WebDriverError:
            pass


# --------------------------------------------------------------------------
# Driver process management
# --------------------------------------------------------------------------

def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


class XvfbProcess:
    """A rig-owned virtual X server. Headless Firefox cannot create ANY
    WebGL context on this box (blocklist says AllowWebgl2:false, and
    webgl.force-enabled then dies with FEATURE_FAILURE_WEBGL_EXHAUSTED_DRIVERS
    -- the headless widget backend has no usable GL provider). Running the
    real Firefox under Xvfb gives it GLX -> Mesa llvmpipe: software WebGL2,
    same determinism class as chromium's SwiftShader."""

    def __init__(self, out_dir, tag, screen=(1680, 1050)):
        xvfb = shutil.which("Xvfb")
        if not xvfb:
            raise WebDriverError("Xvfb not found on PATH")
        self.display = None
        for n in range(99, 160):
            if (not os.path.exists("/tmp/.X%d-lock" % n)
                    and not os.path.exists("/tmp/.X11-unix/X%d" % n)):
                self.display = n
                break
        if self.display is None:
            raise WebDriverError("no free X display number in :99..:159")
        self.log_path = os.path.join(out_dir, "%s.xvfb.log" % tag)
        # named *.driver.pgid so run_smoke.sh's trap sweeps it up too
        self.pgid_file = os.path.join(out_dir, "%s.xvfb.driver.pgid" % tag)
        self.log = open(self.log_path, "ab")
        self.proc = subprocess.Popen(
            [xvfb, ":%d" % self.display, "-screen", "0",
             "%dx%dx24" % screen, "-nolisten", "tcp"],
            stdout=self.log, stderr=subprocess.STDOUT,
            stdin=subprocess.DEVNULL, start_new_session=True)
        with open(self.pgid_file, "w") as f:
            f.write(str(self.proc.pid))
        sock = "/tmp/.X11-unix/X%d" % self.display
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if self.proc.poll() is not None:
                raise WebDriverError("Xvfb exited rc=%s" % self.proc.returncode)
            if os.path.exists(sock):
                return
            time.sleep(0.1)
        raise WebDriverError("Xvfb socket %s never appeared" % sock)

    def stop(self):
        try:
            if self.proc.poll() is None:
                try:
                    os.killpg(self.proc.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
                try:
                    self.proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    try:
                        os.killpg(self.proc.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
        finally:
            self.log.close()
            if os.path.exists(self.pgid_file):
                try:
                    os.unlink(self.pgid_file)
                except OSError:
                    pass


class DriverProcess:
    """Launches chromedriver/geckodriver in its own process group, logs its
    output, and can kill the whole group (browser included) reliably."""

    def __init__(self, argv, port, log_path, env=None, pgid_file=None,
                 aux_procs=None):
        self.port = port
        self.log_path = log_path
        self.pgid_file = pgid_file
        self.aux_procs = list(aux_procs or [])
        self.log = open(log_path, "ab")
        self.proc = subprocess.Popen(
            argv, stdout=self.log, stderr=subprocess.STDOUT,
            stdin=subprocess.DEVNULL, env=env, start_new_session=True)
        if pgid_file:
            with open(pgid_file, "w") as f:
                f.write(str(self.proc.pid))
        self.wire = Wire("http://127.0.0.1:%d" % port)

    def wait_ready(self, timeout=25.0):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.proc.poll() is not None:
                raise WebDriverError(
                    "driver exited rc=%s before becoming ready; log tail:\n%s"
                    % (self.proc.returncode, self.log_tail()))
            try:
                st = self.wire.request("GET", "/status", timeout=2.0)
                if st is not None and st.get("ready") is not False:
                    return st
            except WebDriverError:
                pass
            time.sleep(0.15)
        raise WebDriverError("driver /status not ready after %ss; log tail:\n%s"
                             % (timeout, self.log_tail()))

    def log_tail(self, lines=60):
        try:
            self.log.flush()
            with open(self.log_path, "rb") as f:
                data = f.read()
            return b"\n".join(data.splitlines()[-lines:]).decode("utf-8", "replace")
        except OSError:
            return "<no driver log>"

    def stop(self):
        try:
            if self.proc.poll() is None:
                try:
                    os.killpg(self.proc.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
                try:
                    self.proc.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    try:
                        os.killpg(self.proc.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                    self.proc.wait(timeout=5)
        finally:
            self.log.close()
            if self.pgid_file and os.path.exists(self.pgid_file):
                try:
                    os.unlink(self.pgid_file)
                except OSError:
                    pass
            for aux in self.aux_procs:
                try:
                    aux.stop()
                except Exception:
                    pass


def _version_of(binary):
    try:
        out = subprocess.run([binary, "--version"], capture_output=True,
                             text=True, timeout=20).stdout.strip()
        return out
    except (OSError, subprocess.TimeoutExpired):
        return None


def _major(version_string):
    if not version_string:
        return None
    for tok in version_string.replace("/", " ").split():
        if tok and tok[0].isdigit() and "." in tok:
            try:
                return int(tok.split(".")[0])
            except ValueError:
                continue
    return None


def pick_chromium_binary(driver_path, preferred=None):
    """Choose a Chrome-family binary whose major version matches chromedriver's."""
    driver_ver = _version_of(driver_path)
    driver_major = _major(driver_ver)
    candidates = []
    if preferred:
        candidates.append(preferred)
    for name in ("/usr/bin/chromium", "chromium", "/usr/bin/google-chrome",
                 "google-chrome", "google-chrome-stable"):
        p = name if os.path.isabs(name) else shutil.which(name)
        if p and p not in candidates:
            candidates.append(p)
    best = None
    infos = []
    for c in candidates:
        v = _version_of(c)
        if v is None:
            continue
        infos.append((c, v))
        if best is None and (driver_major is None or _major(v) == driver_major):
            best = (c, v)
    if best is None and infos:
        best = infos[0]
    if best is None:
        raise WebDriverError("no usable chromium/chrome binary found "
                             "(tried %s)" % candidates)
    return {"binary": best[0], "browser_version": best[1],
            "driver_version": driver_ver,
            "version_match": _major(best[1]) == driver_major}


CHROMIUM_ARGS = [
    "--headless=new",          # per task; chromium 151 accepts it
    "--disable-gpu",           # deterministic software path in headless
    "--enable-unsafe-swiftshader",  # Chrome 137+: SwiftShader WebGL needs this
    "--no-sandbox",            # this rig runs inside a sandboxed shell already
    "--disable-dev-shm-usage",
    "--force-device-scale-factor=1",
    "--hide-scrollbars",
    "--mute-audio",
    "--no-first-run",
    "--no-default-browser-check",
    "--password-store=basic",
    "--use-mock-keychain",
    # rAF pacing must not be throttled by backgrounding heuristics:
    "--disable-background-timer-throttling",
    "--disable-backgrounding-occluded-windows",
    "--disable-renderer-backgrounding",
    # chrome's own background chatter, not the page's fetches:
    "--disable-background-networking",
    "--disable-component-update",
    "--disable-breakpad",
    "--disable-features=Translate,OptimizationHints",
]


def chromium_capabilities(binary, window, headless=True, extra_args=()):
    args = list(CHROMIUM_ARGS) + ["--window-size=%d,%d" % window] + list(extra_args)
    if not headless:
        args = [a for a in args if not a.startswith("--headless")]
    return {"capabilities": {"alwaysMatch": {
        "browserName": "chrome",
        "acceptInsecureCerts": True,
        "goog:chromeOptions": {"binary": binary, "args": args},
        "goog:loggingPrefs": {"browser": "ALL"},
    }}}


def firefox_capabilities(binary, window, headless=True, extra_prefs=None):
    args = []
    if headless:
        args.append("-headless")
    args += ["-width", str(window[0]), "-height", str(window[1])]
    prefs = {
        "browser.shell.checkDefaultBrowser": False,
        "datareporting.policy.dataSubmissionEnabled": False,
        "app.update.disabledForTesting": True,
        "browser.sessionstore.resume_from_crash": False,
        # Stock firefox quantizes performance.now() to 1 ms, which reduces
        # rAF deltas to integers (p50=17, p95=18) -- useless for frame-time
        # percentiles. This is a measurement rig: turn quantization off
        # (verified to restore ~microsecond resolution).
        "privacy.reduceTimerPrecision": False,
    }
    if extra_prefs:
        prefs.update(extra_prefs)
    return {"capabilities": {"alwaysMatch": {
        "browserName": "firefox",
        "acceptInsecureCerts": True,
        "moz:firefoxOptions": {
            "binary": binary,
            "args": args,
            "prefs": prefs,
        },
    }}}


def launch(browser, out_dir, tag, driver_path=None, binary=None,
           window=(1280, 900), headless=True, tmp_root=None,
           ff_prefs=None, extra_env=None, ff_mode="auto"):
    """Start the right driver binary + create a session.
    Returns (DriverProcess, Session, info_dict).

    ff_mode (firefox only):
      auto      xvfb when Xvfb exists, else headless (default)
      xvfb      real firefox on a rig-owned Xvfb display -> WebGL2 works
                (llvmpipe); the ONLY mode in which the app can render here
      headless  firefox -headless: boots and runs JS/rAF/network, but WebGL
                context creation fails on this box, so the app panics at
                surface creation and paints nothing (proven; see gotchas)"""
    port = free_port()
    log_path = os.path.join(out_dir, "%s.driver.log" % tag)
    pgid_file = os.path.join(out_dir, "%s.driver.pgid" % tag)
    aux_procs = []
    env = dict(os.environ)
    if env.get("DISPLAY") == "":
        env.pop("DISPLAY")           # empty DISPLAY confuses nothing headless
    tmp_note = None
    if tmp_root:
        # GOTCHA (proven on this box): chrome creates its profile singleton
        # socket under TMPDIR, and sun_path caps at ~107 bytes. A deep TMPDIR
        # (the scratchpad path is 111 chars) makes chrome die at launch with
        # FATAL process_singleton_posix.cc "Socket path too long". So a tmp
        # override is opt-in and only honoured when it is short enough for
        # chromium; ephemeral profiles otherwise go to the system default
        # TMPDIR (/tmp) and are cleaned up by the drivers themselves.
        if browser == "chromium" and len(tmp_root) > 60:
            tmp_note = ("tmp_root %r too long for chrome's singleton socket "
                        "(107-byte sun_path limit); using default TMPDIR"
                        % tmp_root)
        else:
            os.makedirs(tmp_root, exist_ok=True)
            env["TMPDIR"] = tmp_root

    if browser == "chromium":
        driver_path = driver_path or DEFAULT_CHROMEDRIVER
        pick = pick_chromium_binary(driver_path, preferred=binary)
        caps = chromium_capabilities(pick["binary"], window, headless)
        argv = [driver_path, "--port=%d" % port, "--log-level=INFO"]
        info = dict(pick)
    elif browser == "firefox":
        driver_path = driver_path or (shutil.which("geckodriver")
                                      or DEFAULT_GECKODRIVER)
        binary = binary or DEFAULT_FIREFOX
        argv = [driver_path, "--port", str(port), "--log", "info"]
        info = {"binary": binary, "browser_version": _version_of(binary),
                "driver_version": _version_of(driver_path)}
        mode = ff_mode
        if not headless:
            mode = "headed"                      # caller brings the display
        elif mode == "auto":
            mode = "xvfb" if shutil.which("Xvfb") else "headless"
        info["ff_mode"] = mode
        if mode == "xvfb":
            xvfb = XvfbProcess(out_dir, tag,
                               screen=(window[0] + 400, window[1] + 150))
            aux_procs.append(xvfb)
            env["DISPLAY"] = ":%d" % xvfb.display
            env.pop("MOZ_HEADLESS", None)
            info["xvfb_display"] = xvfb.display
            caps = firefox_capabilities(binary, window, headless=False,
                                        extra_prefs=ff_prefs)
        else:
            if mode == "headless":
                env["MOZ_HEADLESS"] = "1"
            caps = firefox_capabilities(binary, window,
                                        headless=(mode == "headless"),
                                        extra_prefs=ff_prefs)
    else:
        raise ValueError("browser must be chromium or firefox")
    info["driver_path"] = driver_path
    if tmp_note:
        info["tmp_note"] = tmp_note
    if extra_env:
        env.update(extra_env)
        info["extra_env"] = dict(extra_env)
    if browser == "firefox":
        info["prefs"] = (caps["capabilities"]["alwaysMatch"]
                         ["moz:firefoxOptions"]["prefs"])

    try:
        driver = DriverProcess(argv, port, log_path, env=env,
                               pgid_file=pgid_file, aux_procs=aux_procs)
    except Exception:
        for aux in aux_procs:
            try:
                aux.stop()
            except Exception:
                pass
        raise
    try:
        driver.wait_ready()
        session = Session.create(driver.wire, caps)
    except Exception:
        driver.stop()
        raise
    return driver, session, info


# --------------------------------------------------------------------------
# Page probes
# --------------------------------------------------------------------------

BOOT_PROBE = """
var s = document.getElementById('rustdar-status');
var c = document.getElementById('rustdar-canvas');
return {
  readyState: document.readyState,
  hasCanvas: !!c,
  clientWidth: c ? c.clientWidth : 0,
  clientHeight: c ? c.clientHeight : 0,
  bufferWidth: c ? c.width : 0,
  bufferHeight: c ? c.height : 0,
  rig: !!window.__rig,
  status: s ? String(s.textContent).slice(0, 400) : null,
  booted: !s,
  failed: !!(s && /failed to start/i.test(String(s.textContent)))
};
"""

ENV_PROBE = """
var out = {
  ua: navigator.userAgent,
  dpr: window.devicePixelRatio,
  inner: [window.innerWidth, window.innerHeight],
  visibility: document.visibilityState,
  webgpu: !!navigator.gpu,
  online: navigator.onLine
};
try {
  var c = document.createElement('canvas');
  c.width = 8; c.height = 8;
  var gl = c.getContext('webgl2') || c.getContext('webgl');
  if (!gl) { out.webgl = null; }
  else {
    out.webgl = (typeof WebGL2RenderingContext !== 'undefined' &&
                 gl instanceof WebGL2RenderingContext) ? 'webgl2' : 'webgl1';
    var dbg = gl.getExtension('WEBGL_debug_renderer_info');
    out.gl_vendor = String(dbg ? gl.getParameter(dbg.UNMASKED_VENDOR_WEBGL)
                               : gl.getParameter(gl.VENDOR));
    out.gl_renderer = String(dbg ? gl.getParameter(dbg.UNMASKED_RENDERER_WEBGL)
                                 : gl.getParameter(gl.RENDERER));
    var lose = gl.getExtension('WEBGL_lose_context');
    if (lose) lose.loseContext();
  }
} catch (e) { out.webgl = 'probe error: ' + String(e); }
return out;
"""

RAF_SCRIPT = """
var n = arguments[0];
var done = arguments[arguments.length - 1];
try {
  var deltas = [];
  var last = null;
  var t0 = performance.now();
  function tick() {
    var now = performance.now();
    if (last !== null) deltas.push(now - last);
    last = now;
    if (deltas.length >= n) {
      var s = deltas.slice().sort(function (a, b) { return a - b; });
      var q = function (f) {
        return s[Math.min(s.length - 1, Math.round(f * (s.length - 1)))];
      };
      var sum = 0;
      for (var i = 0; i < s.length; i++) sum += s[i];
      done({ ok: true, n: deltas.length,
             p50: q(0.5), p90: q(0.9), p95: q(0.95), p99: q(0.99),
             min: s[0], max: s[s.length - 1], mean: sum / s.length,
             wall_ms: now - t0, visibility: document.visibilityState });
      return;
    }
    requestAnimationFrame(tick);
  }
  requestAnimationFrame(tick);
} catch (e) {
  done({ ok: false, error: String(e) });
}
"""

RESOURCES_PROBE = """
var rs = performance.getEntriesByType('resource');
var out = { count: rs.length, fetchxhr: 0, other: 0, hosts: {} };
for (var i = 0; i < rs.length; i++) {
  var r = rs[i];
  if (r.initiatorType === 'fetch' || r.initiatorType === 'xmlhttprequest')
    out.fetchxhr++;
  else out.other++;
  try {
    var h = new URL(r.name).host;
    out.hosts[h] = (out.hosts[h] || 0) + 1;
  } catch (e) {}
}
out.recent = rs.slice(-10).map(function (r) {
  return { u: r.name.length > 120 ? '...' + r.name.slice(-117) : r.name,
           t: r.initiatorType, ms: Math.round(r.duration) };
});
return out;
"""

NAV_TIMING_PROBE = """
var n = performance.getEntriesByType('navigation')[0];
if (!n) return null;
return { domContentLoaded: Math.round(n.domContentLoadedEventEnd),
         loadEventEnd: Math.round(n.loadEventEnd),
         responseEnd: Math.round(n.responseEnd) };
"""

RIG_ERRORS_PROBE = """
return {
  present: !!window.__rig,
  errors: (window.__rig_errors || []).slice(-120),
  console_tail: (window.__rig_console || []).slice(-60),
  console_total: (window.__rig_console || []).length
};
"""

GLOBAL_PROBE = """
var parts = String(arguments[0]).split('.');
var o = window;
for (var i = 0; i < parts.length; i++) {
  if (o === null || o === undefined) return null;
  o = o[parts[i]];
}
if (o === undefined || o === null) return null;
if (typeof o === 'function') { try { o = o(); } catch (e) {
  return JSON.stringify({ __rig_call_error: String(e) }); } }
try { return JSON.stringify(o); }
catch (e) { return JSON.stringify({ __rig_stringify_error: String(e) }); }
"""


def poll_global_json(session, path, timeout=120.0, interval=0.5,
                     ready=None, required=True):
    """THE readiness primitive for the instrumented app: poll window.<path>
    (dot-path; if it is a function it is called) until it yields a non-null
    JSON-serialisable value and ready(value) (if given) is truthy. Returns the
    parsed value; raises TimeoutError (required=True) or returns the last
    value seen (required=False)."""
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        s = session.execute(GLOBAL_PROBE, [path])
        if s is not None:
            val = json.loads(s)
            last = val
            if ready is None or ready(val):
                return val
        time.sleep(interval)
    if required:
        raise TimeoutError("window.%s not ready after %.0fs (last=%.200s)"
                           % (path, timeout, json.dumps(last)))
    return last


def wait_boot(session, timeout=90.0, interval=0.4):
    """Poll until #rustdar-status is removed (booted) or reports failure.
    Returns (final_probe, seconds_waited, timed_out)."""
    t0 = time.monotonic()
    probe = None
    while time.monotonic() - t0 < timeout:
        probe = session.execute(BOOT_PROBE)
        if probe and (probe.get("booted") or probe.get("failed")):
            return probe, time.monotonic() - t0, False
        time.sleep(interval)
    return probe, time.monotonic() - t0, True


def raf_sample(session, frames):
    try:
        r = session.execute_async(RAF_SCRIPT, [frames])
        return r if isinstance(r, dict) else {"ok": False, "error": repr(r)}
    except WebDriverError as e:
        return {"ok": False,
                "error": "rAF sample failed (script timeout usually means "
                         "rAF is not firing): %s" % e}


def collect_driver_browser_log(session, max_entries=200):
    """chromedriver's non-standard log endpoint; geckodriver has no
    equivalent (returns None -> rely on the injected __rig hooks)."""
    last_err = None
    for path in ("/se/log", "/log"):
        try:
            entries = session.cmd("POST", path, {"type": "browser"},
                                  timeout=30.0)
            if not isinstance(entries, list):
                continue
            out = []
            for e in entries[-max_entries:]:
                if isinstance(e, dict):
                    out.append({"level": e.get("level"),
                                "message": str(e.get("message"))[:2000],
                                "timestamp": e.get("timestamp")})
            return {"endpoint": path, "entries": out}
        except WebDriverError as e:
            last_err = str(e)
    return {"endpoint": None, "entries": None, "note": last_err}


# --------------------------------------------------------------------------
# PNG decoding + blank detection (pure python, stdlib zlib)
# --------------------------------------------------------------------------

def _paeth(a, b, c):
    p = a + b - c
    pa = p - a if p >= a else a - p
    pb = p - b if p >= b else b - p
    pc = p - c if p >= c else c - p
    if pa <= pb and pa <= pc:
        return a
    return b if pb <= pc else c


def png_decode(data):
    """Returns (width, height, channels, pixels) for 8-bit non-interlaced
    greyscale/RGB/greyscale+alpha/RGBA PNGs (what browsers emit)."""
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError("not a PNG (bad signature)")
    pos = 8
    ihdr = None
    idat = bytearray()
    while pos + 8 <= len(data):
        (length,) = struct.unpack(">I", data[pos:pos + 4])
        ctype = data[pos + 4:pos + 8]
        chunk = data[pos + 8:pos + 8 + length]
        pos += 12 + length
        if ctype == b"IHDR":
            ihdr = struct.unpack(">IIBBBBB", chunk)
        elif ctype == b"IDAT":
            idat += chunk
        elif ctype == b"IEND":
            break
    if ihdr is None:
        raise ValueError("no IHDR")
    w, h, depth, color, _comp, _filt, interlace = ihdr
    if depth != 8:
        raise ValueError("unsupported bit depth %d" % depth)
    if interlace:
        raise ValueError("interlaced PNG unsupported")
    channels = {0: 1, 2: 3, 4: 2, 6: 4}.get(color)
    if channels is None:
        raise ValueError("unsupported color type %d (palette?)" % color)
    raw = zlib.decompress(bytes(idat))
    stride = w * channels
    if len(raw) < (stride + 1) * h:
        raise ValueError("short pixel data")
    out = bytearray(stride * h)
    prev = bytes(stride)
    pos = 0
    for y in range(h):
        f = raw[pos]
        pos += 1
        line = bytearray(raw[pos:pos + stride])
        pos += stride
        if f == 0:
            pass
        elif f == 1:
            for x in range(channels, stride):
                line[x] = (line[x] + line[x - channels]) & 0xFF
        elif f == 2:
            for x in range(stride):
                line[x] = (line[x] + prev[x]) & 0xFF
        elif f == 3:
            for x in range(stride):
                a = line[x - channels] if x >= channels else 0
                line[x] = (line[x] + ((a + prev[x]) >> 1)) & 0xFF
        elif f == 4:
            for x in range(stride):
                a = line[x - channels] if x >= channels else 0
                c = prev[x - channels] if x >= channels else 0
                line[x] = (line[x] + _paeth(a, prev[x], c)) & 0xFF
        else:
            raise ValueError("bad filter %d at row %d" % (f, y))
        out[y * stride:(y + 1) * stride] = line
        prev = line
    return w, h, channels, bytes(out)


def png_stats(data, target_samples=30000):
    """Decode + sample a grid of pixels; classify blank / near-blank."""
    try:
        w, h, ch, px = png_decode(data)
    except Exception as e:  # report, do not crash the run
        return {"bytes": len(data), "decode_error": str(e), "blank": None}
    step = max(1, int(((w * h) / float(target_samples)) ** 0.5))
    colors = Counter()
    stride = w * ch
    lmin, lmax, lsum, lsq, n = 255.0, 0.0, 0.0, 0.0, 0
    for y in range(0, h, step):
        row = y * stride
        for x in range(0, w, step):
            o = row + x * ch
            if ch == 1:
                r = g = b = px[o]; a = 255
            elif ch == 2:
                r = g = b = px[o]; a = px[o + 1]
            elif ch == 3:
                r, g, b = px[o], px[o + 1], px[o + 2]; a = 255
            else:
                r, g, b, a = px[o], px[o + 1], px[o + 2], px[o + 3]
            colors[(r, g, b, a)] += 1
            lum = 0.2126 * r + 0.7152 * g + 0.0722 * b
            lmin = min(lmin, lum); lmax = max(lmax, lum)
            lsum += lum; lsq += lum * lum; n += 1
    modal, modal_n = colors.most_common(1)[0]
    mean = lsum / n
    var = max(0.0, lsq / n - mean * mean)
    return {
        "bytes": len(data), "width": w, "height": h, "channels": ch,
        "samples": n, "sample_step": step,
        "distinct_colors": len(colors),
        "modal_color": list(modal), "modal_fraction": round(modal_n / n, 4),
        "luminance": {"min": round(lmin, 1), "max": round(lmax, 1),
                      "mean": round(mean, 1), "stddev": round(var ** 0.5, 2)},
        "blank": len(colors) == 1,
        "near_blank": (modal_n / n) >= 0.99,
    }


def save_screenshot(b64, path):
    data = base64.b64decode(b64)
    with open(path, "wb") as f:
        f.write(data)
    return data


# --------------------------------------------------------------------------
# PNG selftest (run with --selftest)
# --------------------------------------------------------------------------

def _png_encode_test(w, h, ch, px, filter_of_row):
    colortype = {1: 0, 2: 4, 3: 2, 4: 6}[ch]
    stride = w * ch
    scan = bytearray()
    for y in range(h):
        line = px[y * stride:(y + 1) * stride]
        prev = px[(y - 1) * stride:y * stride] if y else bytes(stride)
        ft = filter_of_row(y)
        scan.append(ft)
        for x in range(stride):
            a = line[x - ch] if x >= ch else 0
            b = prev[x]
            c = prev[x - ch] if x >= ch else 0
            v = line[x]
            if ft == 0:
                f = v
            elif ft == 1:
                f = (v - a) & 0xFF
            elif ft == 2:
                f = (v - b) & 0xFF
            elif ft == 3:
                f = (v - ((a + b) >> 1)) & 0xFF
            else:
                f = (v - _paeth(a, b, c)) & 0xFF
            scan.append(f)

    def chunk(t, d):
        return (struct.pack(">I", len(d)) + t + d
                + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF))
    ihdr = struct.pack(">IIBBBBB", w, h, 8, colortype, 0, 0, 0)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr)
            + chunk(b"IDAT", zlib.compress(bytes(scan))) + chunk(b"IEND", b""))


def selftest():
    failures = []
    # 1. round-trip through every filter type (encoder shares _paeth with the
    #    decoder, so this catches asymmetric bugs, not a wrong shared paeth --
    #    decoding real browser/encoder PNGs below is the external check).
    w, h, ch = 37, 23, 4
    px = bytes(((x * 7 + (x // ch) * 3 + 13 * (x // (w * ch))) ^ (x % 251)) & 0xFF
               for x in range(w * h * ch))
    for mode, f in (("cycling", lambda y: y % 5), ("all-paeth", lambda y: 4),
                    ("all-avg", lambda y: 3)):
        data = _png_encode_test(w, h, ch, px, f)
        dw, dh, dch, dpx = png_decode(data)
        if (dw, dh, dch) != (w, h, ch) or dpx != px:
            failures.append("round-trip failed (filters=%s)" % mode)
    # 2. blank classifier vectors
    uni = bytes([16, 16, 20] * (64 * 64))
    st = png_stats(_png_encode_test(64, 64, 3, uni, lambda y: y % 5))
    if st.get("blank") is not True:
        failures.append("uniform image not classified blank: %s" % st)
    grad = bytes(b for y in range(64) for x in range(64)
                 for b in ((x * 4) & 0xFF, (y * 4) & 0xFF, 128))
    st = png_stats(_png_encode_test(64, 64, 3, grad, lambda y: (y % 4) + 1))
    if st.get("blank") is not False or st.get("near_blank") is not False:
        failures.append("gradient image classified blank: %s" % st)
    # 3. real-world PNGs from an external encoder (repo icons), if readable
    icons_dir = "/home/reddragon/projects/rustdar/rustdar-web/icons"
    if os.path.isdir(icons_dir):
        for name in sorted(os.listdir(icons_dir)):
            if not name.endswith(".png"):
                continue
            with open(os.path.join(icons_dir, name), "rb") as fh:
                st = png_stats(fh.read())
            if st.get("blank") is None:
                # unsupported flavour (e.g. palette) is a documented limit,
                # not a failure -- but surface it
                print("  note: %s not decodable: %s"
                      % (name, st.get("decode_error")))
            elif st.get("blank") is True:
                failures.append("icon %s classified blank" % name)
            else:
                print("  ok: %s %dx%d distinct=%d" %
                      (name, st["width"], st["height"], st["distinct_colors"]))
    if failures:
        for f in failures:
            print("SELFTEST FAIL: %s" % f)
        return 1
    print("SELFTEST PASS")
    return 0


# --------------------------------------------------------------------------
# Smoke flow
# --------------------------------------------------------------------------

def parse_kv(pairs, json_values=False):
    """['k=v', ...] -> dict; with json_values, v is parsed as JSON when it
    looks like JSON (true/false/numbers), else kept as string."""
    if not pairs:
        return None
    out = {}
    for p in pairs:
        k, _, v = p.partition("=")
        if json_values:
            try:
                out[k] = json.loads(v)
            except ValueError:
                out[k] = v
        else:
            out[k] = v
    return out


def run_smoke(args):
    t0 = time.monotonic()
    out_dir = os.path.abspath(args.out_dir)
    os.makedirs(out_dir, exist_ok=True)
    tag = args.tag or args.browser
    tmp_root = args.tmp_dir  # None = system default; see launch() for why
    window = tuple(int(v) for v in args.window.split("x"))

    result = {
        "tag": tag, "browser": args.browser, "url": args.url,
        "invocation": sys.argv, "started_utc": time.strftime(
            "%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "stages": [], "gotchas": [],
    }

    def stage(name, **detail):
        entry = {"stage": name, "t": round(time.monotonic() - t0, 2)}
        entry.update(detail)
        result["stages"].append(entry)
        line = "[%s] %6.2fs %s" % (tag, entry["t"], name)
        if detail:
            line += " " + json.dumps(detail, default=str)[:300]
        print(line, flush=True)

    driver = session = None
    exit_code = 0
    try:
        stage("launch-driver")
        driver, session, info = launch(
            args.browser, out_dir, tag, driver_path=args.driver,
            binary=args.binary, window=window, headless=not args.headed,
            tmp_root=tmp_root, ff_prefs=parse_kv(args.ff_pref, json_values=True),
            extra_env=parse_kv(args.env), ff_mode=args.ff_mode)
        result["binary"] = info
        caps = session.caps
        result["session"] = {
            "browserName": caps.get("browserName"),
            "browserVersion": caps.get("browserVersion"),
            "platformName": caps.get("platformName"),
        }
        stage("session-created", browser=caps.get("browserName"),
              version=caps.get("browserVersion"))

        session.set_timeouts(script_ms=args.script_timeout * 1000,
                             page_ms=args.page_timeout * 1000)
        try:
            session.set_window_rect(*window)
        except WebDriverError as e:
            result["gotchas"].append("set_window_rect failed: %s" % e)

        stage("navigate", url=args.url)
        nav_t = time.monotonic()
        session.navigate(args.url, timeout=args.page_timeout + 30)
        stage("navigated", load_s=round(time.monotonic() - nav_t, 2))

        boot, boot_wait, timed_out = wait_boot(session,
                                               timeout=args.boot_timeout)
        result["boot"] = {"probe": boot,
                          "seconds_after_load": round(boot_wait, 2),
                          "timed_out": timed_out}
        stage("boot", booted=bool(boot and boot.get("booted")),
              failed=bool(boot and boot.get("failed")), waited=round(boot_wait, 2),
              status=(boot or {}).get("status"))

        canvas_ok = bool(boot and boot.get("hasCanvas")
                         and boot.get("clientWidth", 0) > 0
                         and boot.get("clientHeight", 0) > 0)
        result["canvas_ok"] = canvas_ok

        result["env"] = session.execute(ENV_PROBE)
        stage("env-probe",
              webgl=(result["env"] or {}).get("webgl"),
              renderer=(result["env"] or {}).get("gl_renderer"),
              webgpu=(result["env"] or {}).get("webgpu"),
              visibility=(result["env"] or {}).get("visibility"))
        result["nav_timing"] = session.execute(NAV_TIMING_PROBE)

        if args.poll_global:
            stage("poll-global", name=args.poll_global)
            try:
                result["polled_global"] = poll_global_json(
                    session, args.poll_global, timeout=args.poll_timeout,
                    required=False)
            except (WebDriverError, ValueError) as e:
                result["polled_global"] = {"error": str(e)}

        stage("settle", seconds=args.settle)
        time.sleep(args.settle)

        stage("raf-warm", frames=args.frames)
        result["raf_warm"] = raf_sample(session, args.frames)
        stage("raf-warm-done", **{k: (round(v, 2) if isinstance(v, float) else v)
                                  for k, v in (result["raf_warm"] or {}).items()
                                  if k in ("ok", "n", "p50", "p95", "max")})

        stage("data-window", seconds=args.data_window)
        time.sleep(args.data_window / 2)
        result["resources_mid"] = session.execute(RESOURCES_PROBE)
        time.sleep(args.data_window / 2)
        result["resources"] = session.execute(RESOURCES_PROBE)
        stage("resources", count=(result["resources"] or {}).get("count"),
              hosts=list(((result["resources"] or {}).get("hosts") or {}))[:6])

        if not args.no_second_raf:
            stage("raf-later", frames=args.frames)
            result["raf_later"] = raf_sample(session, args.frames)
            stage("raf-later-done",
                  **{k: (round(v, 2) if isinstance(v, float) else v)
                     for k, v in (result["raf_later"] or {}).items()
                     if k in ("ok", "n", "p50", "p95", "max")})

        stage("screenshots")
        # canvas state NOW (the boot-time probe runs before the app sizes
        # the drawing buffer; reporting that one as "the" buffer misleads)
        result["canvas_final"] = session.execute(BOOT_PROBE)
        shots = {}
        page_png = os.path.join(out_dir, "%s.page.png" % tag)
        data = save_screenshot(session.screenshot_b64(), page_png)
        shots["page"] = png_stats(data)
        shots["page"]["path"] = page_png
        canvas_el = session.find_element("#rustdar-canvas")
        if canvas_el:
            canvas_png = os.path.join(out_dir, "%s.canvas.png" % tag)
            data = save_screenshot(
                session.element_screenshot_b64(canvas_el), canvas_png)
            shots["canvas"] = png_stats(data)
            shots["canvas"]["path"] = canvas_png
        else:
            shots["canvas"] = {"error": "element #rustdar-canvas not found"}
        result["screenshots"] = shots
        stage("screenshots-done",
              page_blank=shots["page"].get("blank"),
              canvas_blank=shots.get("canvas", {}).get("blank"),
              page_distinct=shots["page"].get("distinct_colors"),
              canvas_distinct=shots.get("canvas", {}).get("distinct_colors"))

        stage("collect-errors")
        result["rig_signal"] = session.execute(RIG_ERRORS_PROBE)
        if args.browser == "chromium":
            result["driver_browser_log"] = collect_driver_browser_log(session)
        else:
            result["driver_browser_log"] = {
                "endpoint": None, "entries": None,
                "note": "geckodriver exposes no log endpoint; console/error "
                        "signal comes from the injected __rig hooks "
                        "(serve.py /index-rig.html + worker.js prelude)"}

        # ---- verdict ----------------------------------------------------
        rig_errors = (result["rig_signal"] or {}).get("errors") or []
        panics = [e for e in rig_errors
                  if "panicked at" in str(e.get("msg", ""))]
        raf_ok = bool((result.get("raf_warm") or {}).get("ok"))
        booted = bool(boot and boot.get("booted"))
        canvas_blank = shots.get("canvas", {}).get("blank")
        # A fully-uniform canvas after the settle+data window is a rendering
        # failure even when data has not arrived: the basemap always draws
        # (index.html documents exactly that). blank=None (decode issue)
        # does not fail the run, it is reported.
        result["pass"] = (booted and canvas_ok and raf_ok
                          and canvas_blank is not True and not panics)
        result["verdict"] = {
            "booted": booted, "canvas_ok": canvas_ok, "raf_ok": raf_ok,
            "rig_error_count": len(rig_errors),
            "panic_count": len(panics),
            "first_panic": (str(panics[0].get("msg"))[:300] if panics else None),
            "page_blank": shots["page"].get("blank"),
            "canvas_blank": canvas_blank,
        }
        exit_code = 0 if result["pass"] else 2

    except Exception as e:
        result["failed_stage"] = (result["stages"][-1]["stage"]
                                  if result["stages"] else "init")
        result["exception"] = "".join(
            traceback.format_exception_only(type(e), e)).strip()
        result["traceback"] = traceback.format_exc()
        result["pass"] = False
        exit_code = 1 if not isinstance(e, WebDriverError) else 2
        print("[%s] FAILED at stage %r: %s" % (tag, result["failed_stage"], e),
              flush=True)
        # best-effort diagnostics
        if session is not None:
            for key, script in (("boot_probe_at_failure", BOOT_PROBE),
                                ("rig_signal_at_failure", RIG_ERRORS_PROBE)):
                try:
                    result[key] = session.execute(script, timeout=20)
                except Exception as diag:
                    result[key] = "unavailable: %s" % diag
            try:
                p = os.path.join(out_dir, "%s.fail.png" % tag)
                save_screenshot(session.screenshot_b64(), p)
                result["failure_screenshot"] = p
            except Exception:
                pass
        if driver is not None:
            result["driver_log_tail"] = driver.log_tail(80)
    finally:
        if session is not None:
            session.delete()
        if driver is not None:
            driver.stop()

    result["total_s"] = round(time.monotonic() - t0, 2)
    json_path = os.path.join(out_dir, "%s.json" % tag)
    with open(json_path, "w") as f:
        json.dump(result, f, indent=2, default=str)
    print("[%s] result -> %s" % (tag, json_path), flush=True)

    # human summary
    v = result.get("verdict") or {}
    env = result.get("env") or {}
    rw = result.get("raf_warm") or {}
    rl = result.get("raf_later") or {}
    b = (result.get("canvas_final")
         or (result.get("boot") or {}).get("probe") or {})

    def fr(d):
        if not d.get("ok"):
            return "FAILED: %s" % d.get("error")
        return ("n=%d p50=%.2f p90=%.2f p95=%.2f p99=%.2f max=%.2f mean=%.2f ms"
                % (d["n"], d["p50"], d["p90"], d["p95"], d["p99"], d["max"],
                   d["mean"]))
    print("[%s] SUMMARY pass=%s booted=%s canvas=%sx%s (buffer %sx%s) dpr=%s"
          % (tag, result.get("pass"), v.get("booted"),
             b.get("clientWidth"), b.get("clientHeight"),
             b.get("bufferWidth"), b.get("bufferHeight"), env.get("dpr")))
    print("[%s] SUMMARY gl=%s renderer=%r webgpu=%s visibility=%s"
          % (tag, env.get("webgl"), env.get("gl_renderer"), env.get("webgpu"),
             env.get("visibility")))
    print("[%s] SUMMARY raf warm : %s" % (tag, fr(rw)))
    if rl:
        print("[%s] SUMMARY raf later: %s" % (tag, fr(rl)))
    sh = result.get("screenshots") or {}
    for which in ("page", "canvas"):
        s = sh.get(which) or {}
        print("[%s] SUMMARY shot %-6s blank=%s near_blank=%s distinct=%s modal=%s(%.1f%%)"
              % (tag, which, s.get("blank"), s.get("near_blank"),
                 s.get("distinct_colors"), s.get("modal_color"),
                 100.0 * (s.get("modal_fraction") or 0)))
    print("[%s] SUMMARY rig_errors=%s driver_log=%s"
          % (tag, v.get("rig_error_count"),
             len(((result.get("driver_browser_log") or {}).get("entries"))
                 or [])))
    return exit_code


def main(argv=None):
    ap = argparse.ArgumentParser(
        description="headless WebDriver smoke/measurement rig for rustdar-web")
    ap.add_argument("--browser", choices=("chromium", "firefox"))
    ap.add_argument("--url")
    ap.add_argument("--out-dir", default="out")
    ap.add_argument("--tag", default=None, help="output file prefix "
                    "(default: browser name)")
    ap.add_argument("--driver", default=None,
                    help="driver binary (default: %s / geckodriver on PATH "
                         "then %s)" % (DEFAULT_CHROMEDRIVER, DEFAULT_GECKODRIVER))
    ap.add_argument("--binary", default=None, help="browser binary override")
    ap.add_argument("--window", default="1280x900")
    ap.add_argument("--frames", type=int, default=120,
                    help="rAF deltas per sample (default 120)")
    ap.add_argument("--settle", type=float, default=6.0,
                    help="seconds after boot before the warm rAF sample")
    ap.add_argument("--data-window", type=float, default=10.0,
                    help="seconds to let live data arrive before second sample")
    ap.add_argument("--boot-timeout", type=float, default=90.0)
    ap.add_argument("--script-timeout", type=float, default=90.0)
    ap.add_argument("--page-timeout", type=float, default=120.0)
    ap.add_argument("--poll-global", default=None,
                    help="after boot, poll this window global (dot-path) for "
                         "JSON stats (the instrumented app's hook)")
    ap.add_argument("--poll-timeout", type=float, default=120.0)
    ap.add_argument("--no-second-raf", action="store_true")
    ap.add_argument("--headed", action="store_true",
                    help="disable headless flags (debugging only)")
    ap.add_argument("--tmp-dir", default=None,
                    help="TMPDIR override for browser profiles (default: "
                         "system TMPDIR; must be <60 chars for chromium -- "
                         "chrome's singleton socket hits the 107-byte "
                         "sun_path limit under deep paths)")
    ap.add_argument("--ff-mode", choices=("auto", "xvfb", "headless"),
                    default="auto",
                    help="firefox display mode: xvfb = real firefox on a "
                         "rig-owned virtual display (WebGL2 works, default "
                         "when Xvfb exists); headless = firefox -headless "
                         "(NO WebGL on this box: the app boots but panics at "
                         "surface creation and paints nothing)")
    ap.add_argument("--ff-pref", action="append", default=[],
                    help="extra firefox pref, key=value (value parsed as "
                         "JSON when possible); repeatable")
    ap.add_argument("--env", action="append", default=[],
                    help="extra env var for the driver+browser process, "
                         "K=V; repeatable")
    ap.add_argument("--selftest", action="store_true",
                    help="run the PNG decoder/blank-detector selftest and exit")
    args = ap.parse_args(argv)

    if args.selftest:
        return selftest()
    if not args.browser or not args.url:
        ap.error("--browser and --url are required (or use --selftest)")
    return run_smoke(args)


if __name__ == "__main__":
    sys.exit(main())
