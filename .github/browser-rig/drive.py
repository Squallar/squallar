#!/usr/bin/env python3
"""
drive.py -- dependency-free W3C WebDriver client for driving the rustdar web
app headless. Python 3 stdlib only (urllib; selenium is NOT installed).

Starts the driver binary itself (chromedriver / geckodriver), creates a
headless session, navigates to the served app, waits for boot (the app removes
#rustdar-status on a successful start() and writes "rustdar failed to start:"
into it on failure), then measures and records:

  * canvas #rustdar-canvas presence + client/buffer size + dpr
  * WebGL renderer probe (software vs hardware) and the caps the wasm budget
    cascade is allowed to promote against: MAX_TEXTURE_SIZE,
    MAX_3D_TEXTURE_SIZE, MAX_RENDERBUFFER_SIZE, hardwareConcurrency,
    deviceMemory
  * WebGPU probed by actually calling requestAdapter() (adapter presence,
    info, limits, features). `!!navigator.gpu` alone is not a capability test
  * cross-origin isolation: self.crossOriginIsolated, SharedArrayBuffer
    presence, and the service-worker registration list
  * requestAnimationFrame delta stats over N frames (p50/p90/p95/p99/max),
    sampled twice: right after settle, and again after a data window
  * viewport + canvas-element screenshots, saved as PNG and analysed with a
    built-in pure-python PNG decoder (blank / near-blank detection)
  * error signal: injected window.__rig_errors / __rig_console (both
    browsers, provided by serve.py's /index-rig.html) plus chromedriver's
    non-standard browser-log endpoint (chromium only; geckodriver has none)
  * Tier-2 worker-wire assertions (opt-in): --expect-worker-round-trip fails
    the run unless the console ring shows a worker attach AND a "took N ms
    off the frame" job reply; --expect-doctored-respawn fails it unless the
    doctored-token refusal is followed >=1000 ms later by a clean attach
    (pair with serve.py --doctor-first-worker);
    --expect-service-worker fails it unless a real registration is present
    (pair with serve.py --no-block-sw); --expect-cross-origin-isolated fails
    it unless self.crossOriginIsolated is true (pair with serve.py --coep)

Timing primitive for the instrumented app: poll_global_json() polls a window
global (dot-path) until it holds a JSON-serialisable value -- the future
instrumented build exposes richer stats via such a global, and
`--poll-global NAME` wires it from the CLI.

Two arms, selected with --arm, NEVER merged:

  software  (default) the CI arm and the one every pinned Tier-2 figure was
            taken on. Chromium --disable-gpu + SwiftShader, Firefox on a
            rig-owned Xvfb -> Mesa llvmpipe. Deterministic by construction and
            needs no GPU, which is why CI can run it.
  hardware  the real-driver measurement arm. Drops the software flags and
            points both browsers at the machine's own X display, so both reach
            the actual driver. It is a measurement arm, not a gate: it needs a
            GPU and a logged-in session, and it puts a window on the desktop.
            Pair it with --require-hardware, which refuses to report a figure
            taken on a renderer that turned out to be software after all.

Every cap in the env probe is a property of the ADAPTER, not of the browser
(MAX_TEXTURE_SIZE measured 8192 on SwiftShader and 16384 on llvmpipe for the
same two browsers). So the arm and the classified adapter are recorded in the
artifact and reprinted on every summary line that carries a cap or a frame
time.

Exit codes: 0 pass, 2 measured failure (boot/canvas/rAF/adapter), 1 unexpected
error.

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


CHROMIUM_HEADLESS_ARG = "--headless=new"   # per task; chromium 151 accepts it

CHROMIUM_BASE_ARGS = [
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

# The SOFTWARE arm. This is what CI runs and what every pinned Tier-2 figure
# was taken on: no GPU, SwiftShader WebGL, deterministic by construction.
# CI runners have no GPU, so this arm may never be made conditional on one.
CHROMIUM_SOFTWARE_ARGS = [
    "--disable-gpu",           # deterministic software path in headless
    "--enable-unsafe-swiftshader",  # Chrome 137+: SwiftShader WebGL needs this
]

# The HARDWARE arm. Lifted from the a9 compositor harness
# (`harness/a9-close:webperf/chrome.mjs`, `gpuFlags()`), which is the recipe
# that reached this box's RTX 3090 from a browser. Two things make it work and
# both are load-bearing:
#
#   * `--enable-unsafe-swiftshader` is deliberately ABSENT. Since Chrome 137 a
#     page that cannot get hardware WebGL gets NO WebGL2 context rather than a
#     silent software one -- so this arm fails loudly instead of quietly
#     re-measuring SwiftShader under a "hardware" label.
#   * ANGLE's passthrough to the native EGL/GLES driver (`--use-angle=gl-egl`)
#     is what finds the driver. The GPU sandbox cannot open the DRM render
#     node here and a GPU process that cannot do that falls back to software.
CHROMIUM_HARDWARE_ARGS = [
    "--enable-gpu",
    "--ignore-gpu-blocklist",
    "--enable-gpu-rasterization",
    "--disable-gpu-sandbox",
    "--use-gl=angle",
    "--use-angle=gl-egl",
]

# Headless chromium has no window system, so ozone must be told so explicitly;
# with it, ANGLE still reaches the driver over EGL/GBM. Only for the hardware
# arm: the software arm's --disable-gpu makes the question moot.
#
# MEASURED 2026-08-22 (`--arm hardware --display none`): this path reaches the
# same RTX 3090 as the headed one, with identical caps (32768 / 16384 / 32768)
# and slightly tighter frame timing (p50 16.70 vs 16.40 ms, p99 16.90 vs 18.40).
# So the hardware arm does NOT actually require a logged-in desktop for
# chromium -- only firefox needs the display. A GPU-equipped CI runner could
# take chromium hardware figures with no session at all.
CHROMIUM_HEADLESS_OZONE_ARGS = ["--ozone-platform=headless"]

# The software arm's full flag list in its original order, which is what
# CHROMIUM_ARGS meant before the split. Kept so that arm=software is not just
# the same SET of flags as before but the same LIST.
CHROMIUM_ARGS = ([CHROMIUM_HEADLESS_ARG] + CHROMIUM_SOFTWARE_ARGS
                 + CHROMIUM_BASE_ARGS)

# Substrings that mean "this renderer is not a GPU". Same list the a9 harness
# refused to measure on (`SOFTWARE_MARKERS` in webperf/chrome.mjs), plus
# lavapipe (Mesa's software Vulkan, which this repo's GPU CI lane uses).
SOFTWARE_RENDERER_MARKERS = (
    "swiftshader",
    "llvmpipe",
    "lavapipe",
    "softpipe",
    "software rasterizer",
    "google inc. (google)",   # chromium's vendor/renderer pair for SwiftShader
    "mesa offscreen",
    "microsoft basic render",
)


def classify_adapter(env):
    """Name the adapter behind a set of ENV_PROBE readings.

    Every cap in that probe is a property of the ADAPTER, not of the browser:
    MAX_TEXTURE_SIZE was 8192 on SwiftShader and 16384 on llvmpipe for the
    same two browsers. A figure quoted without this classification beside it
    is not interpretable, so the rig computes it once and prints it on every
    line that carries a cap.

    The class is the trustworthy part; the renderer STRING is not an
    identifier. Firefox sanitises WEBGL_debug_renderer_info to a coarse
    bucket -- on this box's RTX 3090 it answers "NVIDIA GeForce GTX 980, or
    similar", exactly as it answers "llvmpipe, or similar" for Mesa. Chromium
    hands back the real device. So `hardware` vs `software` may be quoted;
    the device name may not, unless chromium said it."""
    env = env or {}
    webgl = env.get("webgl")
    renderer = env.get("gl_renderer")
    vendor = env.get("gl_vendor")
    out = {"renderer": renderer, "vendor": vendor, "webgl": webgl}
    if not webgl or (isinstance(webgl, str) and webgl.startswith("probe error")):
        out["class"] = "none"
        out["why"] = ("no WebGL context: %s" % webgl) if webgl else "no WebGL context"
        return out
    haystack = ("%s %s" % (vendor, renderer)).lower()
    hit = next((m for m in SOFTWARE_RENDERER_MARKERS if m in haystack), None)
    if hit:
        out["class"] = "software"
        out["marker"] = hit
        return out
    out["class"] = "hardware"
    return out


def adapter_label(adapter):
    """One short field for summary lines: `hardware:NVIDIA GeForce RTX 3090`."""
    adapter = adapter or {}
    cls = adapter.get("class", "unknown")
    if cls == "none":
        return "none(%s)" % adapter.get("why", "?")
    return "%s:%s" % (cls, adapter.get("renderer") or "?")


def resolve_host_display(explicit=None):
    """Find the machine's REAL X display, and the cookie that opens it.

    The software arm's Xvfb is a display too, so "there is a display" was
    never the question -- the question is whether the display is backed by a
    driver. This returns the host session's, which on this box is Xwayland
    on :0 in front of an RTX 3090.

    Returns {"display": ":0", "xauthority": path|None, "source": str} or
    {"display": None, "why": str}.

    An explicit request is honoured or refused, never quietly replaced: a
    `--display :7` that silently became :0 would report a figure from a
    display the caller did not ask for. `--display none` asks for no display
    at all, which is how the GPU-less-desktop case (chromium headless over
    ANGLE/EGL) is reached deliberately."""
    if explicit == "none":
        return {"display": None, "why": "disabled by --display none"}
    if explicit:
        num = explicit.split(":")[-1].split(".")[0]
        if num.isdigit() and os.path.exists("/tmp/.X11-unix/X%s" % num):
            return {"display": ":%s" % num, "xauthority": _find_xauth(),
                    "source": "--display"}
        return {"display": None,
                "why": "requested display %r has no socket in /tmp/.X11-unix"
                       % explicit}
    cands = []
    if os.environ.get("RIG_DISPLAY"):
        cands.append((os.environ["RIG_DISPLAY"], "RIG_DISPLAY"))
    if os.environ.get("DISPLAY"):
        cands.append((os.environ["DISPLAY"], "DISPLAY"))
    cands.append((":0", "default :0"))
    chosen = source = None
    for disp, src in cands:
        num = disp.split(":")[-1].split(".")[0]
        if not num.isdigit():
            continue
        if os.path.exists("/tmp/.X11-unix/X%s" % num):
            chosen, source = ":%s" % num, src
            break
    if chosen is None:
        return {"display": None,
                "why": "no X socket in /tmp/.X11-unix for any of %s"
                       % ", ".join(d for d, _ in cands)}
    return {"display": chosen, "xauthority": _find_xauth(), "source": source}


def _find_xauth():
    """The cookie for the host display.

    A host display is nearly always MIT-MAGIC-COOKIE-protected, and a browser
    launched without the cookie dies with "Invalid MIT-MAGIC-COOKIE-1 key" --
    which reads exactly like "there is no display on this box", and is how a
    machine with an RTX 3090 came to be described as unable to reach one."""
    for cand in _xauth_candidates():
        if cand and os.path.isfile(cand) and os.access(cand, os.R_OK):
            return cand
    return None


def _xauth_candidates():
    yield os.environ.get("XAUTHORITY")
    run_dir = os.environ.get("XDG_RUNTIME_DIR") or "/run/user/%d" % os.getuid()
    try:
        entries = sorted(
            (os.path.join(run_dir, n) for n in os.listdir(run_dir)
             if n.startswith("xauth")),
            key=lambda p: os.path.getmtime(p), reverse=True)
    except OSError:
        entries = []
    for e in entries:
        yield e
    yield os.path.expanduser("~/.Xauthority")


def chromium_capabilities(binary, window, headless=True, extra_args=(),
                          arm="software"):
    args = [CHROMIUM_HEADLESS_ARG] if headless else []
    args += (CHROMIUM_SOFTWARE_ARGS if arm == "software"
             else CHROMIUM_HARDWARE_ARGS)
    if arm == "hardware" and headless:
        args += CHROMIUM_HEADLESS_OZONE_ARGS
    args += CHROMIUM_BASE_ARGS
    args += ["--window-size=%d,%d" % window] + list(extra_args)
    return {"capabilities": {"alwaysMatch": {
        "browserName": "chrome",
        "acceptInsecureCerts": True,
        "goog:chromeOptions": {"binary": binary, "args": args},
        "goog:loggingPrefs": {"browser": "ALL"},
    }}}


# Firefox on the hardware arm. Firefox decides its GL provider from the
# graphics blocklist, and a blocklisted driver is answered with llvmpipe --
# which is a successful-looking run on the wrong adapter. These two override
# that decision, and WebRender is named so compositing is on the GPU as well
# as the WebGL context.
#
# MEASURED, and stated because the opposite is the easy thing to assume: on
# this box they are NOT what makes the hardware arm work. A control on
# 2026-08-22 ran the same firefox 153 on :0 with this dict emptied and got the
# identical reading (NVIDIA, 32768 / 16384) -- the real display alone reaches
# the driver here. They are kept for a box whose driver IS blocklisted, and
# --require-hardware, not these prefs, is what makes the arm able to fail.
FIREFOX_HARDWARE_PREFS = {
    "webgl.force-enabled": True,
    "gfx.webrender.all": True,
}


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
           ff_prefs=None, extra_env=None, ff_mode="auto", arm="software",
           display=None, chromium_args=()):
    """Start the right driver binary + create a session.
    Returns (DriverProcess, Session, info_dict).

    arm:
      software  the CI arm, unchanged and deterministic: chromium on
                SwiftShader, firefox on a rig-owned Xvfb -> Mesa llvmpipe.
                Runs anywhere, including a GPU-less CI runner.
      hardware  the real-driver arm: the software flags are dropped and the
                browsers are pointed at the machine's own display, so both
                reach the actual GPU. Needs a GPU and a session; it is a
                MEASUREMENT arm, never a gate.

    ff_mode (firefox only):
      auto      host display on the hardware arm; xvfb when Xvfb exists on
                the software arm; else headless
      host      real firefox on the machine's own X display -> the real
                driver. A window appears on the user's desktop.
      xvfb      real firefox on a rig-owned Xvfb display -> WebGL2 works
                (llvmpipe); the ONLY software mode in which the app renders
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

    # The hardware arm needs a display with a driver behind it. Chromium can
    # also reach the driver headless (ANGLE over EGL/GBM), so a display is
    # optional there and mandatory for firefox, whose headless widget backend
    # has no GL provider on this box at all.
    host = None
    if arm == "hardware":
        host = resolve_host_display(display)

    if browser == "chromium":
        driver_path = driver_path or DEFAULT_CHROMEDRIVER
        pick = pick_chromium_binary(driver_path, preferred=binary)
        chrome_headless = headless
        if arm == "hardware" and host and host.get("display"):
            # Headed on the real display: the same surface path a user gets,
            # and the only way the two browsers are measured on the same
            # window system.
            chrome_headless = False
            env["DISPLAY"] = host["display"]
            if host.get("xauthority"):
                env["XAUTHORITY"] = host["xauthority"]
        caps = chromium_capabilities(pick["binary"], window, chrome_headless,
                                     extra_args=chromium_args, arm=arm)
        argv = [driver_path, "--port=%d" % port, "--log-level=INFO"]
        info = dict(pick)
        info["gpu_mode"] = ("headed-host-display" if not chrome_headless
                            else ("headless-angle-egl" if arm == "hardware"
                                  else "headless-swiftshader"))
    elif browser == "firefox":
        driver_path = driver_path or (shutil.which("geckodriver")
                                      or DEFAULT_GECKODRIVER)
        binary = binary or DEFAULT_FIREFOX
        argv = [driver_path, "--port", str(port), "--log", "info"]
        info = {"binary": binary, "browser_version": _version_of(binary),
                "driver_version": _version_of(driver_path)}
        mode = ff_mode
        if mode == "auto" and arm == "hardware":
            mode = "host"
        elif not headless:
            mode = "headed"                      # caller brings the display
        elif mode == "auto":
            mode = "xvfb" if shutil.which("Xvfb") else "headless"
        info["ff_mode"] = mode
        if mode == "host":
            if not host:
                host = resolve_host_display(display)
            if not host.get("display"):
                raise WebDriverError(
                    "ff_mode=host needs the machine's own X display: %s"
                    % host.get("why"))
            env["DISPLAY"] = host["display"]
            if host.get("xauthority"):
                env["XAUTHORITY"] = host["xauthority"]
            env.pop("MOZ_HEADLESS", None)
            info["host_display"] = dict(host)
            prefs = dict(FIREFOX_HARDWARE_PREFS)
            prefs.update(ff_prefs or {})
            caps = firefox_capabilities(binary, window, headless=False,
                                        extra_prefs=prefs)
        elif mode == "xvfb":
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
    info["arm"] = arm
    if host is not None:
        info["host_display"] = dict(host)
    if browser == "chromium":
        info["chromium_args"] = (caps["capabilities"]["alwaysMatch"]
                                 ["goog:chromeOptions"]["args"])
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
  online: navigator.onLine,
  hardware_concurrency: navigator.hardwareConcurrency || null,
  device_memory: navigator.deviceMemory || null,
  cross_origin_isolated: (typeof self.crossOriginIsolated === 'boolean')
                           ? self.crossOriginIsolated : null,
  shared_array_buffer: (typeof SharedArrayBuffer !== 'undefined')
};
try {
  var c = document.createElement('canvas');
  c.width = 8; c.height = 8;
  var gl = c.getContext('webgl2') || c.getContext('webgl');
  if (!gl) { out.webgl = null; }
  else {
    var is2 = (typeof WebGL2RenderingContext !== 'undefined' &&
               gl instanceof WebGL2RenderingContext);
    out.webgl = is2 ? 'webgl2' : 'webgl1';
    var dbg = gl.getExtension('WEBGL_debug_renderer_info');
    out.gl_vendor = String(dbg ? gl.getParameter(dbg.UNMASKED_VENDOR_WEBGL)
                               : gl.getParameter(gl.VENDOR));
    out.gl_renderer = String(dbg ? gl.getParameter(dbg.UNMASKED_RENDERER_WEBGL)
                                 : gl.getParameter(gl.RENDERER));
    // The cap the wasm budget cascade is allowed to promote against. wgpu's
    // downlevel_webgl2_defaults().using_resolution(adapter) copies
    // max_texture_dimension_2d verbatim from here, so this number -- not any
    // constant in the tree -- is what the adapter actually offers.
    out.max_texture_size = gl.getParameter(gl.MAX_TEXTURE_SIZE);
    out.max_renderbuffer_size = gl.getParameter(gl.MAX_RENDERBUFFER_SIZE);
    // MAX_3D_TEXTURE_SIZE is WebGL2-only; on a webgl1 context the enum is
    // undefined and getParameter would throw an INVALID_ENUM.
    out.max_3d_texture_size = is2 ? gl.getParameter(gl.MAX_3D_TEXTURE_SIZE)
                                  : null;
    var lose = gl.getExtension('WEBGL_lose_context');
    if (lose) lose.loseContext();
  }
} catch (e) { out.webgl = 'probe error: ' + String(e); }
return out;
"""

# WebGPU, probed for real. `!!navigator.gpu` is NOT a capability test: measured
# 2026-08-22, Chromium 151 on SwiftShader exposes navigator.gpu while
# requestAdapter() resolves to null -- the old boolean field read true on a
# machine with no usable WebGPU adapter and could not fail. What decides is
# whether an adapter comes back, and what limits it carries.
#
# The limits are the reason this probe records more than a boolean. Measured
# 2026-08-22 on this box's RTX 3090, hardware arm, WebGL2 vs WebGPU on the SAME
# adapter (see run_gpu_arm.sh for how to reproduce each):
#
#   firefox 153  WebGL2 32768 / 16384   WebGPU 32767 / 16384  (pref-gated)
#   chromium 151 WebGL2 32768 / 16384   WebGPU 16384 /  2048  (vulkan ANGLE)
#
# i.e. on Chromium a WebGPU switch HALVES the 2D cap and cuts the 3D cap by 8x
# against the WebGL2 the same GPU already offers. A promotion keyed on
# max_texture_dimension_2d must therefore name its API as well as its adapter.
WEBGPU_PROBE = """
var done = arguments[arguments.length - 1];
var out = { gpu_object: !!navigator.gpu, adapter: null };
if (!navigator.gpu) { done(out); return; }
var settled = false;
var finish = function (o) { if (!settled) { settled = true; done(o); } };
setTimeout(function () {
  out.adapter = null; out.error = 'requestAdapter timed out after 15000 ms';
  finish(out);
}, 15000);
try {
  navigator.gpu.requestAdapter().then(function (a) {
    out.adapter = !!a;
    if (a) {
      try {
        var info = a.info || {};
        out.adapter_info = { vendor: info.vendor, architecture: info.architecture,
                             device: info.device, description: info.description };
      } catch (e) { out.adapter_info = 'info error: ' + String(e); }
      try {
        var L = a.limits || {};
        out.adapter_limits = {
          maxTextureDimension2D: L.maxTextureDimension2D,
          maxTextureDimension3D: L.maxTextureDimension3D,
          maxBufferSize: L.maxBufferSize,
          maxStorageBufferBindingSize: L.maxStorageBufferBindingSize
        };
      } catch (e) { out.adapter_limits = 'limits error: ' + String(e); }
      try {
        var f = [];
        a.features.forEach(function (x) { f.push(x); });
        out.adapter_features = f.sort();
      } catch (e) { out.adapter_features = 'features error: ' + String(e); }
    }
    finish(out);
  }, function (e) {
    out.adapter = false; out.error = String(e); finish(out);
  });
} catch (e) { out.adapter = false; out.error = String(e); finish(out); }
"""

# Service-worker state. The Tier-2 default blocks registration (serve.py's
# prelude rejects register(); --no-block-sw lets the real one through), so
# `blocked_by_rig` is the first thing to read here: a "no registration" line
# means nothing when the rig itself refused. Under COEP this is load-bearing --
# Chrome will not register a worker for an isolated client unless the worker
# script response carries a matching COEP header.
SW_PROBE = """
var done = arguments[arguments.length - 1];
var out = {
  supported: !!navigator.serviceWorker,
  blocked_by_rig: !!(window.__rig && window.__rig.block_sw),
  controller: null, registrations: null
};
if (!navigator.serviceWorker) { done(out); return; }
out.controller = navigator.serviceWorker.controller
  ? String(navigator.serviceWorker.controller.scriptURL) : null;
var settled = false;
var finish = function () { if (!settled) { settled = true; done(out); } };
setTimeout(function () { out.error = 'getRegistrations timed out'; finish(); },
           15000);
navigator.serviceWorker.getRegistrations().then(function (rs) {
  out.registrations = rs.map(function (r) {
    var w = r.active || r.installing || r.waiting;
    return { scope: String(r.scope),
             script: w ? String(w.scriptURL) : null,
             state: w ? String(w.state) : null,
             active: !!r.active, installing: !!r.installing,
             waiting: !!r.waiting };
  });
  finish();
}, function (e) { out.error = String(e); finish(); });
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
var out = { count: rs.length, fetchxhr: 0, other: 0, hosts: {},
            status_unknown: 0, failed: [] };
for (var i = 0; i < rs.length; i++) {
  var r = rs[i];
  if (r.initiatorType === 'fetch' || r.initiatorType === 'xmlhttprequest')
    out.fetchxhr++;
  else out.other++;
  try {
    var h = new URL(r.name).host;
    out.hosts[h] = (out.hosts[h] || 0) + 1;
  } catch (e) {}
  // responseStatus is 0 for a request that never produced a response --
  // which is what a COEP block looks like from the page's side. It is also 0
  // when the browser does not implement the field at all, so count those
  // separately rather than reporting a fleet of phantom failures.
  var st = r.responseStatus;
  if (typeof st !== 'number') out.status_unknown++;
  else if (st === 0 || st >= 400) {
    if (out.failed.length < 40)
      out.failed.push({ u: r.name.length > 160 ? '...' + r.name.slice(-157)
                                               : r.name,
                        t: r.initiatorType, status: st });
  }
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

# The Tier-2 worker-wire signals, scanned from the console ring the serve.py
# prelude keeps. Timestamps are page-side Date.now() at log time.
#   attached  -- worker_port::handle_message's "rasterization worker attached"
#                (log::info!, HELLO with a matching build token)
#   different -- "rasterization worker is a different build" (log::warn!, the
#                token-mismatch branch that terminates and respawns)
#   off_frame -- offload::deliver_job_reply's "<kind> took <N> ms off the
#                frame" (log::info!): a reply actually crossed the wire.
#   rayon     -- the thread count carried on the SAME attach line, out of the
#                worker's HELLO (worker_protocol::THREADS, which the worker
#                fills from rayon::current_num_threads()). The worker's own
#                console is not scanned here and cannot be: `__rig_console` is
#                a page-side ring.
WORKER_SIGNAL_PROBE = r"""
var C = window.__rig_console || [];
var attached = [], different = [], off_frame = [], rayon = [];
var off_re = /took \d+ ms off the frame/;
var rayon_re = /rayon: (\d+) threads/;
for (var i = 0; i < C.length; i++) {
  var m = String(C[i].msg || "");
  if (m.indexOf("rasterization worker attached") !== -1) attached.push(C[i].t);
  if (m.indexOf("rasterization worker is a different build") !== -1)
    different.push(C[i].t);
  if (off_re.test(m)) off_frame.push(C[i].t);
  var rm = rayon_re.exec(m);
  if (rm) rayon.push(parseInt(rm[1], 10));
}
return { attached: attached, different: different, off_frame: off_frame,
         rayon_threads: rayon, console_total: C.length };
"""

# Which backend the APP settled on, read out of its own startup log.
#
# This is the only observable answer, and it is not the same question the
# WEBGPU_PROBE below asks. That one says what the BROWSER can do; this says what
# the build DID -- the choice is made inside `App::create_instance` by a
# `requestAdapter()` nothing page-side repeats, and once made, WebGPU and WebGL2
# draw the same picture at different ceilings. A run that quotes a cap without
# naming the API that produced it is quoting an adapter it did not identify.
#
# Two lines, both `log::info!` from `rustdar_app::app_state`:
#   backend  -- "wgpu selected the <Backend> backend: <name> (<DeviceType>), ..."
#   ceiling  -- "plan views may reach <N> px: this device reports <M> px 2D
#               textures (the <A> px adapter offered), ..."
# The second is what shows the limit was REQUESTED rather than accepted: device
# and adapter figures agreeing is the property, and on WebGPU the default would
# be 8192 regardless of what the adapter offered.
APP_BACKEND_PROBE = r"""
var C = window.__rig_console || [];
var backend = null, ceiling = null;
for (var i = 0; i < C.length; i++) {
  var m = String(C[i].msg || "");
  if (m.indexOf("wgpu selected the ") !== -1) backend = m;
  if (m.indexOf("plan views may reach ") !== -1) ceiling = m;
}
return { backend: backend, raster_ceiling: ceiling, console_total: C.length };
"""


def wait_app_backend(session, timeout=60.0, interval=0.5):
    """The app's own backend AND ceiling lines, polled until both appear.

    Polled rather than read once because the renderer is stood up
    asynchronously: `booted` is reported before `AppState::new` has met an
    adapter. BOTH lines are waited for, not just the first: `request_device`
    sits between them and on the WebGPU backend it is a JS promise, so a probe
    that stops at the backend line lands in the gap and reports a null ceiling
    perhaps half the time. Measured: one chromium WebGPU leg answered at
    `console_total` 12 with the ceiling still unlogged.

    A miss is not fatal -- what is found is reported and what is not is `null`,
    so a cap then carries no API name. That is worse than a slow answer and
    still not a failure of the leg."""
    t0 = time.monotonic()
    found = {}
    while time.monotonic() - t0 < timeout:
        last = session.execute(APP_BACKEND_PROBE) or {}
        # Accumulated across polls: the ring evicts, and a line seen once is
        # seen.
        for key in ("backend", "raster_ceiling"):
            if not found.get(key) and last.get(key):
                found[key] = last[key]
        found["console_total"] = last.get("console_total")
        if found.get("backend") and found.get("raster_ceiling"):
            found["waited_s"] = round(time.monotonic() - t0, 2)
            return found
        time.sleep(interval)
    found["waited_s"] = round(time.monotonic() - t0, 2)
    missing = [k for k in ("backend", "raster_ceiling") if not found.get(k)]
    found["error"] = ("the app never logged %s within %.0fs"
                      % (" and ".join(missing), timeout))
    return found


def app_backend_name(app):
    """`Gl`, `BrowserWebGpu`, ... out of the app's own log line, or None."""
    line = (app or {}).get("backend") or ""
    head = "wgpu selected the "
    i = line.find(head)
    if i < 0:
        return None
    rest = line[i + len(head):]
    j = rest.find(" backend")
    return rest[:j] if j > 0 else None


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


def wait_worker_round_trip(session, timeout=180.0, interval=2.0):
    """m4: the worker attach AND a real job reply, within `timeout`.

    Both signals must appear: >=1 "rasterization worker attached" (the HELLO
    handshake passed the build-token compare) and >=1 "took <N> ms off the
    frame" (offload::deliver_job_reply -- a real job crossed the wire and came
    back). A booted page with a dead wire shows the first and never the
    second; canvas-non-blank alone is weaker than either. First-seen
    timestamps are accumulated across polls so ring eviction between polls
    cannot lose an event."""
    t0 = time.monotonic()
    first_attached = first_off_frame = None
    while time.monotonic() - t0 < timeout:
        sig = session.execute(WORKER_SIGNAL_PROBE) or {}
        if first_attached is None and sig.get("attached"):
            first_attached = min(sig["attached"])
        if first_off_frame is None and sig.get("off_frame"):
            first_off_frame = min(sig["off_frame"])
        if first_attached is not None and first_off_frame is not None:
            return {"ok": True, "waited_s": round(time.monotonic() - t0, 2),
                    "attached_t": first_attached,
                    "off_frame_t": first_off_frame}
        time.sleep(interval)
    return {"ok": False, "waited_s": round(time.monotonic() - t0, 2),
            "attached_t": first_attached, "off_frame_t": first_off_frame,
            "error": "no worker attach + job reply within %.0fs "
                     "(attached=%s, off_frame=%s)"
                     % (timeout, first_attached is not None,
                        first_off_frame is not None)}


def wait_rayon_pool(session, minimum, timeout=180.0, interval=2.0):
    """WS3b: the rasterization worker's rayon pool has at least `minimum`
    threads.

    The count rides the worker's HELLO and is printed on the attach line, so
    this reads the pool rayon BUILT, not the one `worker.js` asked for. It
    exists because every other Tier-2 assertion is blind to the difference: a
    worker that fell back to `rustdarRayonSerialPool` still attaches, still
    answers jobs and still logs "took N ms off the frame". Without this check
    a browser served without COOP/COEP, or one that refused nested Workers,
    goes green while rasterizing on one thread -- the gate would be asserting
    that WS3b's machinery is *reachable*, never that it *ran*.

    `minimum` is 2 rather than the requested count on purpose: the requested
    count is `navigator.hardwareConcurrency` clamped to the budget, which is a
    property of whatever box the rig runs on. Two threads is the smallest
    observation that distinguishes a real pool from the fallback, and it is
    the only threshold that means the same thing on a 4-core CI runner and a
    32-core desktop."""
    t0 = time.monotonic()
    best = None
    while time.monotonic() - t0 < timeout:
        sig = session.execute(WORKER_SIGNAL_PROBE) or {}
        seen = [n for n in (sig.get("rayon_threads") or []) if isinstance(n, int)]
        if seen:
            best = max(seen) if best is None else max(best, max(seen))
        if best is not None and best >= minimum:
            return {"ok": True, "threads": best, "minimum": minimum,
                    "waited_s": round(time.monotonic() - t0, 2)}
        time.sleep(interval)
    return {"ok": False, "threads": best, "minimum": minimum,
            "waited_s": round(time.monotonic() - t0, 2),
            "error": "rasterization worker reported %s rayon threads, wanted "
                     ">=%d within %.0fs (a worker that fell back to the "
                     "one-thread pool looks identical to every other Tier-2 "
                     "assertion)" % (best, minimum, timeout)}


def wait_doctored_respawn(session, timeout=180.0, respawn_grace=30.0,
                          interval=1.0):
    """m5: the doctored-token detection and the clean respawn.

    Phase 1 (up to `timeout`): "rasterization worker is a different build" --
    the page read the stub's HELLO and refused the token. Phase 2 (up to
    `respawn_grace` from that observation): a "rasterization worker attached"
    stamped >= 1000 ms after the refusal -- the first backoff rung is 1000 ms
    (worker_retry::RESPAWN_BACKOFF_MS), so a qualifying attach proves the
    ladder ran and the REFETCHED worker (the real file; the stub is served
    exactly once) passed the compare. Earlier attaches never qualify: the
    timestamp filter is what makes a pre-doctor attach unable to satisfy the
    respawn assertion."""
    t0 = time.monotonic()
    different_t = None
    while time.monotonic() - t0 < timeout:
        sig = session.execute(WORKER_SIGNAL_PROBE) or {}
        if sig.get("different"):
            different_t = min(sig["different"])
            break
        time.sleep(interval)
    if different_t is None:
        return {"ok": False, "waited_s": round(time.monotonic() - t0, 2),
                "error": "the doctored token was never refused: no "
                         "'different build' line within %.0fs" % timeout}
    t1 = time.monotonic()
    while time.monotonic() - t1 < respawn_grace:
        sig = session.execute(WORKER_SIGNAL_PROBE) or {}
        qualifying = [t for t in (sig.get("attached") or [])
                      if t >= different_t + 1000]
        if qualifying:
            attached_t = min(qualifying)
            return {"ok": True, "waited_s": round(time.monotonic() - t0, 2),
                    "different_t": different_t, "attached_t": attached_t,
                    "delta_ms": attached_t - different_t}
        time.sleep(interval)
    return {"ok": False, "waited_s": round(time.monotonic() - t0, 2),
            "different_t": different_t,
            "error": "detected the doctored token but no attach >=1000 ms "
                     "later within %.0fs of it" % respawn_grace}


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
        # First field after the tag on purpose. Every cap below is a property
        # of the adapter this arm reached, and a reader who does not know
        # which arm ran cannot use any of them.
        "arm": args.arm,
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
            extra_env=parse_kv(args.env), ff_mode=args.ff_mode, arm=args.arm,
            display=args.display, chromium_args=args.chromium_arg)
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
        try:
            result["env"]["webgpu"] = session.execute_async(WEBGPU_PROBE)
        except (WebDriverError, TypeError) as e:
            result["env"]["webgpu"] = {"probe_error": str(e)}
        env0 = result["env"] or {}
        wg = env0.get("webgpu") or {}
        result["adapter"] = classify_adapter(env0)
        stage("env-probe",
              arm=args.arm,
              adapter=adapter_label(result["adapter"]),
              webgl=env0.get("webgl"),
              renderer=env0.get("gl_renderer"),
              max_texture_size=env0.get("max_texture_size"),
              max_3d_texture_size=env0.get("max_3d_texture_size"),
              webgpu_object=wg.get("gpu_object"),
              webgpu_adapter=wg.get("adapter"),
              cross_origin_isolated=env0.get("cross_origin_isolated"),
              visibility=env0.get("visibility"))
        result["nav_timing"] = session.execute(NAV_TIMING_PROBE)

        # Early, and before the 180 s worker-wire waits: serve.py's console ring
        # holds 1200 entries and a live-network leg can push a startup line out
        # of it.
        stage("app-backend-wait")
        result["app_backend"] = wait_app_backend(session)
        stage("app-backend", backend=app_backend_name(result["app_backend"]),
              waited=result["app_backend"].get("waited_s"))

        if args.poll_global:
            stage("poll-global", name=args.poll_global)
            try:
                result["polled_global"] = poll_global_json(
                    session, args.poll_global, timeout=args.poll_timeout,
                    required=False)
            except (WebDriverError, ValueError) as e:
                result["polled_global"] = {"error": str(e)}

        # Tier-2 worker-wire assertions (serve.py's console ring carries the
        # signals). Doctored-respawn first when both are on: its events
        # (refusal, backoff, re-attach) precede the first job reply.
        if args.expect_doctored_respawn:
            stage("doctored-respawn-wait", timeout=args.expect_timeout)
            result["doctored_respawn"] = wait_doctored_respawn(
                session, timeout=args.expect_timeout)
            stage("doctored-respawn-done", **result["doctored_respawn"])
        if args.expect_worker_round_trip:
            stage("worker-round-trip-wait", timeout=args.expect_timeout)
            result["worker_round_trip"] = wait_worker_round_trip(
                session, timeout=args.expect_timeout)
            stage("worker-round-trip-done", **result["worker_round_trip"])
        if args.expect_rayon_threads:
            stage("rayon-pool-wait", minimum=args.expect_rayon_threads,
                  timeout=args.expect_timeout)
            result["rayon_pool"] = wait_rayon_pool(
                session, args.expect_rayon_threads, timeout=args.expect_timeout)
            stage("rayon-pool-done", **result["rayon_pool"])

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
              hosts=list(((result["resources"] or {}).get("hosts") or {}))[:6],
              failed=len((result["resources"] or {}).get("failed") or []))

        # Late on purpose: registration, install and activation all have to
        # finish first, and the data window has just paid for that time.
        try:
            result["service_worker"] = session.execute_async(SW_PROBE)
        except (WebDriverError, TypeError) as e:
            result["service_worker"] = {"probe_error": str(e)}
        sw = result["service_worker"] or {}
        stage("service-worker", blocked_by_rig=sw.get("blocked_by_rig"),
              controller=sw.get("controller"),
              registrations=len(sw.get("registrations") or []))

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
        #
        # The worker-wire assertions fail the run when requested and unmet: a
        # booted page with a dead wire passes every weaker check.
        wrt = result.get("worker_round_trip")
        dr = result.get("doctored_respawn")
        rp = result.get("rayon_pool")
        worker_ok = ((wrt is None or bool(wrt.get("ok")))
                     and (dr is None or bool(dr.get("ok")))
                     and (rp is None or bool(rp.get("ok"))))

        # Isolation assertions (opt-in). Both are written so that they FAIL
        # when the thing they name is absent -- an isolation proof that cannot
        # come back false proves nothing.
        sw_reg = [r for r in (sw.get("registrations") or [])
                  if r.get("active") or r.get("waiting") or r.get("installing")]
        sw_ok = None
        if args.expect_service_worker:
            if sw.get("blocked_by_rig"):
                sw_ok = False
                result["service_worker"]["expect_error"] = (
                    "--expect-service-worker with the rig's own SW block still "
                    "on; pass serve.py --no-block-sw")
            else:
                sw_ok = bool(sw_reg)
                if not sw_ok:
                    result["service_worker"]["expect_error"] = (
                        "no service-worker registration after the data window")
        coi = env0.get("cross_origin_isolated")
        coi_ok = None
        if args.expect_cross_origin_isolated:
            coi_ok = (coi is True)

        # The hardware arm's own non-triviality floor. Without it the arm
        # cannot fail: a chromium that quietly fell back to SwiftShader looks
        # exactly like one that reached the driver, and re-labelling the same
        # software figures "hardware" is the precise defect this arm exists to
        # correct. Same refusal as the a9 harness's assertHardwareRenderer.
        adapter = result.get("adapter") or {}
        hw_ok = None
        if args.require_hardware:
            hw_ok = (adapter.get("class") == "hardware")
            if not hw_ok:
                result["gotchas"].append(
                    "--require-hardware: adapter is %s (renderer %r); "
                    "fix the GPU flags rather than reporting this figure"
                    % (adapter_label(adapter), adapter.get("renderer")))

        result["pass"] = (booted and canvas_ok and raf_ok
                          and canvas_blank is not True and not panics
                          and worker_ok
                          and sw_ok is not False and coi_ok is not False
                          and hw_ok is not False)
        result["verdict"] = {
            "arm": args.arm,
            "adapter_class": adapter.get("class"),
            "adapter_renderer": adapter.get("renderer"),
            "hardware_ok": hw_ok,
            "booted": booted, "canvas_ok": canvas_ok, "raf_ok": raf_ok,
            "service_worker_ok": sw_ok,
            "cross_origin_isolated": coi,
            "cross_origin_isolated_ok": coi_ok,
            "resource_failures": len((result.get("resources") or {})
                                     .get("failed") or []),
            "rig_error_count": len(rig_errors),
            "panic_count": len(panics),
            "first_panic": (str(panics[0].get("msg"))[:300] if panics else None),
            "page_blank": shots["page"].get("blank"),
            "canvas_blank": canvas_blank,
            "worker_round_trip_ok": (None if wrt is None
                                     else bool(wrt.get("ok"))),
            "doctored_respawn_ok": (None if dr is None
                                    else bool(dr.get("ok"))),
            "rayon_pool_ok": (None if rp is None else bool(rp.get("ok"))),
            # Reported unconditionally, not only under --expect-rayon-threads:
            # a measurement run wants the number even when nothing is gating
            # on it, and `null` distinguishes "never observed" from "one".
            "rayon_threads": (None if rp is None else rp.get("threads")),
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
    adapter = result.get("adapter") or classify_adapter(env)
    alabel = adapter_label(adapter)
    # The arm and the adapter lead every summary, and are repeated on each
    # line that carries a cap or a frame time. A figure whose adapter is
    # unknown is worse than no figure: it gets quoted.
    binfo = result.get("binary") or {}
    print("[%s] SUMMARY arm=%s adapter=%s (%s) via %s"
          % (tag, result.get("arm"), alabel, adapter.get("vendor"),
             binfo.get("gpu_mode") or binfo.get("ff_mode") or "?"))
    hd = binfo.get("host_display") or {}
    if hd:
        print("[%s] SUMMARY display=%s (%s) xauthority=%s"
              % (tag, hd.get("display") or "none", hd.get("source")
                 or hd.get("why"), hd.get("xauthority")))
    print("[%s] SUMMARY pass=%s booted=%s canvas=%sx%s (buffer %sx%s) dpr=%s"
          % (tag, result.get("pass"), v.get("booted"),
             b.get("clientWidth"), b.get("clientHeight"),
             b.get("bufferWidth"), b.get("bufferHeight"), env.get("dpr")))
    wg = env.get("webgpu") or {}
    if wg.get("probe_error"):
        webgpu_s = "probe error: %s" % str(wg["probe_error"])[:80]
    elif not wg.get("gpu_object"):
        webgpu_s = "no navigator.gpu"
    elif wg.get("adapter"):
        webgpu_s = "adapter %s (maxTex2D=%s)" % (
            (wg.get("adapter_info") or {}).get("description")
            or (wg.get("adapter_info") or {}).get("vendor") or "?",
            (wg.get("adapter_limits") or {}).get("maxTextureDimension2D"))
    else:
        webgpu_s = "navigator.gpu present but requestAdapter -> null%s" % (
            ("; " + str(wg["error"])[:80]) if wg.get("error") else "")
    print("[%s] SUMMARY gl=%s renderer=%r visibility=%s"
          % (tag, env.get("webgl"), env.get("gl_renderer"),
             env.get("visibility")))
    print("[%s] SUMMARY [%s] max_texture=%s max_3d_texture=%s "
          "max_renderbuffer=%s cores=%s device_memory=%s"
          % (tag, alabel,
             env.get("max_texture_size"), env.get("max_3d_texture_size"),
             env.get("max_renderbuffer_size"),
             env.get("hardware_concurrency"), env.get("device_memory")))
    print("[%s] SUMMARY [%s] webgpu: %s" % (tag, alabel, webgpu_s))
    app = result.get("app_backend") or {}
    print("[%s] SUMMARY [%s] app selected backend=%s"
          % (tag, alabel, app_backend_name(app) or ("UNKNOWN: %s"
             % (app.get("error") or "not logged"))))
    for key in ("backend", "raster_ceiling"):
        if app.get(key):
            print("[%s] SUMMARY [%s]   %s" % (tag, alabel, app[key]))
    if v.get("hardware_ok") is False:
        print("[%s] SUMMARY HARDWARE ARM FAILED: adapter is %s, not a GPU"
              % (tag, alabel))
    swr = result.get("service_worker") or {}
    print("[%s] SUMMARY isolation: crossOriginIsolated=%s SharedArrayBuffer=%s "
          "sw_blocked_by_rig=%s sw_registrations=%s"
          % (tag, env.get("cross_origin_isolated"),
             env.get("shared_array_buffer"), swr.get("blocked_by_rig"),
             len(swr.get("registrations") or [])))
    for r in (swr.get("registrations") or []):
        print("[%s] SUMMARY   sw %s scope=%s state=%s"
              % (tag, r.get("script"), r.get("scope"), r.get("state")))
    if swr.get("expect_error"):
        print("[%s] SUMMARY   sw EXPECT FAILED: %s" % (tag, swr["expect_error"]))
    res = result.get("resources") or {}
    print("[%s] SUMMARY resources=%s hosts=%s failed=%s status_unknown=%s"
          % (tag, res.get("count"), len(res.get("hosts") or {}),
             len(res.get("failed") or []), res.get("status_unknown")))
    for f in (res.get("failed") or [])[:10]:
        print("[%s] SUMMARY   resource status=%s %s"
              % (tag, f.get("status"), f.get("u")))
    print("[%s] SUMMARY [%s] raf warm : %s" % (tag, alabel, fr(rw)))
    if rl:
        print("[%s] SUMMARY [%s] raf later: %s" % (tag, alabel, fr(rl)))
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
    wrt = result.get("worker_round_trip")
    if wrt is not None:
        print("[%s] SUMMARY worker round-trip: %s (attach + off-the-frame "
              "reply%s)"
              % (tag, "OK" if wrt.get("ok") else "FAILED",
                 "" if wrt.get("ok") else "; " + str(wrt.get("error"))))
    dr = result.get("doctored_respawn")
    if dr is not None:
        print("[%s] SUMMARY doctored respawn: %s%s"
              % (tag, "OK" if dr.get("ok") else "FAILED",
                 (" (attach %.0f ms after the refusal)" % dr["delta_ms"])
                 if dr.get("ok") else "; " + str(dr.get("error"))))
    rp = result.get("rayon_pool")
    if rp is not None:
        print("[%s] SUMMARY rayon pool: %s (%s threads, wanted >=%s)"
              % (tag, "OK" if rp.get("ok") else "FAILED",
                 rp.get("threads"), rp.get("minimum")))
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
    ap.add_argument("--expect-worker-round-trip", action="store_true",
                    help="fail unless the console shows a worker attach AND a "
                         "'took N ms off the frame' job reply within "
                         "--expect-timeout (Tier-2 m4)")
    ap.add_argument("--expect-rayon-threads", type=int, default=0,
                    metavar="N",
                    help="fail unless the rasterization worker's HELLO reports "
                         ">=N rayon threads within --expect-timeout (WS3b). "
                         "Pair with serve.py --coep: without cross-origin "
                         "isolation there is no SharedArrayBuffer, the pool "
                         "falls back to one thread, and every other Tier-2 "
                         "assertion still passes")
    ap.add_argument("--expect-doctored-respawn", action="store_true",
                    help="fail unless the console shows the doctored-token "
                         "refusal and then, >=1000 ms later, a clean attach "
                         "(Tier-2 m5; pair with serve.py --doctor-first-worker)")
    ap.add_argument("--expect-service-worker", action="store_true",
                    help="fail the run unless a real service-worker "
                         "registration is present after the data window "
                         "(needs serve.py --no-block-sw)")
    ap.add_argument("--expect-cross-origin-isolated", action="store_true",
                    help="fail the run unless self.crossOriginIsolated is "
                         "true (pair with serve.py --coep)")
    ap.add_argument("--expect-timeout", type=float, default=180.0,
                    help="seconds for the worker-wire assertions (default "
                         "180; Tier 2 runs against LIVE network, so the first "
                         "job reply waits on a real volume fetch)")
    ap.add_argument("--no-second-raf", action="store_true")
    ap.add_argument("--headed", action="store_true",
                    help="disable headless flags (debugging only)")
    ap.add_argument("--tmp-dir", default=None,
                    help="TMPDIR override for browser profiles (default: "
                         "system TMPDIR; must be <60 chars for chromium -- "
                         "chrome's singleton socket hits the 107-byte "
                         "sun_path limit under deep paths)")
    ap.add_argument("--arm", choices=("software", "hardware"),
                    default="software",
                    help="which adapter to measure. software (default, and "
                         "what CI runs): chromium on SwiftShader, firefox on "
                         "a rig-owned Xvfb -> llvmpipe; deterministic and "
                         "GPU-free. hardware: the software flags are dropped "
                         "and both browsers are pointed at the machine's own "
                         "display, so they reach the real driver. The arm is "
                         "recorded in the artifact and printed on every "
                         "summary line that carries a cap")
    ap.add_argument("--require-hardware", action="store_true",
                    help="fail the run unless the WebGL renderer is a real "
                         "GPU (pair with --arm hardware). Without it the "
                         "hardware arm cannot fail: a silent SwiftShader "
                         "fallback reads exactly like a driver")
    ap.add_argument("--display", default=None,
                    help="X display for --arm hardware (default: $RIG_DISPLAY, "
                         "then $DISPLAY, then :0, first one with a live "
                         "socket). The cookie is found automatically under "
                         "$XDG_RUNTIME_DIR; without it a real display reports "
                         "as no display at all. An explicit value is honoured "
                         "or refused, never quietly replaced. `none` asks for "
                         "no display: chromium then reaches the driver "
                         "headless over ANGLE/EGL (measured identical caps), "
                         "firefox cannot and the run fails")
    ap.add_argument("--ff-mode", choices=("auto", "host", "xvfb", "headless"),
                    default="auto",
                    help="firefox display mode: host = the machine's own X "
                         "display, i.e. the real driver (default on --arm "
                         "hardware); xvfb = real firefox on a rig-owned "
                         "virtual display (llvmpipe; WebGL2 works, default on "
                         "--arm software when Xvfb exists); headless = "
                         "firefox -headless (NO WebGL on this box: the app "
                         "boots but panics at surface creation and paints "
                         "nothing)")
    ap.add_argument("--chromium-arg", action="append", default=[],
                    help="extra chromium command-line flag, appended after "
                         "the arm's own; repeatable. The counterpart of "
                         "--ff-pref, and how a capability question is asked "
                         "without editing the rig -- e.g. WebGPU on this box "
                         "needs --enable-unsafe-webgpu plus a Vulkan ANGLE "
                         "backend, and the answer differs from the default "
                         "hardware arm's")
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
