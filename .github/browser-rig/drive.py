#!/usr/bin/env python3
"""
drive.py -- dependency-free W3C WebDriver client for driving the squallar web
app headless. Python 3 stdlib only (urllib; selenium is NOT installed).

Starts the driver binary itself (chromedriver / geckodriver), creates a
headless session, navigates to the served app, waits for boot (the app removes
#squallar-status on a successful start() and writes "squallar failed to start:"
into it on failure), then measures and records:

  * canvas #squallar-canvas presence + client/buffer size + dpr
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

Frame telemetry (WO-5): scrapes the app's own `frame service` / `frame
segments` / `frame prep costs` / `gpu passes` / `frame cadence` lines
(seeded loud via squallar.frame_telemetry) and BIN-DIFFS the embedded
42-count histograms between the gesture marker lines to report
gesture-window-only percentiles -- cumulative-from-boot figures are labelled
as such and never quoted as window figures. `--w3c-gesture` drives real
input through the driver's /actions endpoint (pointer + wheel input
sources; a driver that refuses the wheel source falls back to a synthesized
WheelEvent, recorded per browser). `--expect-interaction-frames` is the one
gate built on any of it, and it is a COUNT assert: the interact family
strictly grew. `--wait-console-regex` is the generic wait-until-console-line
primitive. `--android` (BOTH engines: chromedriver + goog:chromeOptions or
geckodriver + moz:firefoxOptions, each carrying androidPackage, plus `adb
reverse`) and `--browser safari` (safaridriver) are implemented per spec but
NOT device-tested from this box; they say so where they run.

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
import errno
import json
import os
import re
import shutil
import signal
import socket
import struct
import subprocess
import sys
import time
import traceback
import urllib.error
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET
import zlib
from collections import Counter

# ---------------------------------------------------------------------------
# EXIT CODES, and the reason there are four of them rather than two
# ---------------------------------------------------------------------------
#
# A wrapper reading only "zero or not" cannot tell "the leg ran and the app
# failed" from "this process never got as far as opening a browser", and that
# is not a theoretical distinction: on 2026-08-31 a rebase dropped the commit
# that added `--expect-frame-progress`, argparse rejected the flag, twelve legs
# died in under a second each, and the launcher went on to read whatever JSON
# was already on disk and reported all twelve as PASS. A usage error and a red
# leg both exited 2, so nothing downstream could have told them apart.
#
# 64 and 69 are sysexits.h's EX_USAGE and EX_UNAVAILABLE. They are outside the
# range any assertion in here produces, so a wrapper can switch on them.
EXIT_PASS = 0
# The leg ran and something it asserts on was false, or the driver wire broke
# mid-leg. A RESULT EXISTS; read the JSON for which assertion it was.
EXIT_LEG_FAILED = 2
# The command line was wrong -- an unknown flag, a missing required pair, a
# mode that needs a tool that is not installed. NOTHING RAN and no result was
# written. This is fatal to the whole run, not to one leg: every other leg is
# about to be handed the same argument list and die the same way.
EXIT_USAGE = 64
# The BOX failed, not the app: no space left, quota exceeded. Also nothing
# usable ran, but the distinction from EXIT_USAGE matters because the repair is
# somewhere else entirely.
EXIT_INFRA = 69

# The errnos that mean "the filesystem, not the code". EDQUOT is the one that
# actually fired: a full disk raised `OSError: [Errno 122] Disk quota exceeded`
# from inside a `print()` during teardown, escaped as an unhandled traceback,
# and was reported as a failed leg.
INFRA_ERRNOS = (errno.ENOSPC, errno.EDQUOT)

ELEMENT_KEY = "element-6066-11e4-a52e-4f735466cecf"
DEFAULT_CHROMEDRIVER = "/usr/bin/chromedriver"
# The durable location ensure-geckodriver.sh provisions. The gate never relies
# on this default (run_tier2.sh always passes --driver), but a default that
# pointed anywhere ephemeral would rot silently.
DEFAULT_GECKODRIVER = os.path.expanduser(
    "~/.cache/squallar-ci/geckodriver-0.37.1/geckodriver")
DEFAULT_FIREFOX = ("/Applications/Firefox.app/Contents/MacOS/firefox"
                   if sys.platform == "darwin" else "/usr/bin/firefox")


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


class FirefoxDirectProcess:
    """Firefox launched by the RIG rather than by geckodriver, for macOS.

    WHY THIS EXISTS (measured on an M2, 2026-08-31, Firefox 154.0.1):
    geckodriver launches firefox through mozrunner, which always appends
    `-foreground` on macOS. From a non-GUI context -- an ssh session, which is
    how this rig is driven -- that makes firefox re-exec itself through
    LaunchServices to reach the user's Aqua session: the pid geckodriver is
    watching exits 0 within ~330 ms and geckodriver reports

        Process (pid=N) unexpectedly closed with status 0

    while a perfectly healthy firefox -- same args, same profile, GPU helper
    and all -- keeps running under a NEW pid. Launching the binary ourselves
    WITHOUT `-foreground` was verified to keep the original pid (the process
    stays put and Marionette comes up), so the rig starts firefox and hands
    geckodriver `--connect-existing --marionette-port N`.

    PROFILE SAFETY. The profile is a throwaway directory this class creates and
    owns, passed as `-profile`, alongside `-no-remote` so the instance can
    never attach to -- or be attached to by -- the user's running firefox. The
    user's real profile is never opened, read or written. This mirrors what
    geckodriver itself does (it builds a `rust_mozprofile*` temp dir); the only
    difference is who creates it.

    A CAVEAT THAT IS NOT ABOUT PROFILES. Launching the user's INSTALLED firefox
    runs its updater, which applies any already-staged update to the app bundle
    and can prompt the user's own running instance to restart. That was
    observed on this box: a 153.0.4 -> 154.0.1 update staged five days earlier
    applied two minutes after the rig's first launch. It costs no user data,
    but it moves the browser version out from under a measurement campaign, so
    read `browser_version` off the row rather than trusting what was installed
    when the leg was queued. A dedicated channel install would avoid it.

    Prefs cannot ride on `moz:firefoxOptions` in connect-existing mode --
    geckodriver applies those only when it launches the browser itself -- so
    they are written into the profile's `user.js` before launch. That path
    matters for measurement: `privacy.reduceTimerPrecision=false` is what gives
    rAF deltas sub-millisecond resolution, and a row measured without it is
    quantised to 1 ms and useless for percentiles."""

    def __init__(self, binary, profile_dir, marionette_port, window,
                 prefs=None, log_path=None, env=None):
        self.profile_dir = profile_dir
        self.marionette_port = marionette_port
        os.makedirs(profile_dir, exist_ok=True)
        prefs = dict(prefs or {})
        prefs["marionette.port"] = marionette_port
        with open(os.path.join(profile_dir, "user.js"), "w") as f:
            for k, v in sorted(prefs.items()):
                f.write("user_pref(%s, %s);\n"
                        % (json.dumps(k), json.dumps(v)))
        self.argv = [binary, "--marionette", "-no-remote",
                     "-profile", profile_dir,
                     "-width", str(window[0]), "-height", str(window[1])]
        self.log = open(log_path or os.devnull, "ab")
        self.proc = subprocess.Popen(
            self.argv, stdout=self.log, stderr=subprocess.STDOUT,
            stdin=subprocess.DEVNULL, env=env, start_new_session=True)

    def wait_ready(self, timeout=90.0):
        """Block until Marionette is listening, proven by the port file the
        browser writes into its own profile."""
        port_file = os.path.join(self.profile_dir, "MarionetteActivePort")
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.proc.poll() is not None:
                raise WebDriverError(
                    "firefox exited rc=%s before Marionette came up"
                    % self.proc.returncode)
            try:
                with open(port_file) as f:
                    got = int(f.read().strip())
                if got:
                    return got
            except (OSError, ValueError):
                pass
            time.sleep(0.2)
        raise WebDriverError("Marionette did not come up in %ss" % timeout)

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
            try:
                self.log.close()
            except Exception:
                pass
            shutil.rmtree(self.profile_dir, ignore_errors=True)


def _loadavg():
    """The host's 1-minute load average, or None where the OS has no such
    idea. `os.getloadavg` covers both platforms this rig runs on -- procfs on
    linux, sysctl on macOS -- so the row reads the same on either."""
    try:
        return round(os.getloadavg()[0], 2)
    except (OSError, AttributeError):
        return None


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
    names = ("/usr/bin/chromium", "chromium", "/usr/bin/google-chrome",
             "google-chrome", "google-chrome-stable")
    if sys.platform == "darwin":
        # macOS ships no chrome on PATH; the app bundle is the binary.
        names = ("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
                 "/Applications/Chromium.app/Contents/MacOS/Chromium") + names
    for name in names:
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

# The macOS hardware arm. The list above is a LINUX recipe and saying so is the
# point: `--use-angle=gl-egl` names desktop-GL-over-EGL, which is not the
# backend ANGLE uses on macOS -- Metal is -- and forcing it was MEASURED on an
# M2 (2026-08-31, Chrome 152.0.7977.65) to break WebGPU outright:
#
#     "Failed to create WebGPU Context Provider"
#
# after which the app's GL fallback ALSO fails, because the failed
# `getContext("webgpu")` has already bound the canvas:
#
#     canvas.getContext() returned null; webgl2 not available or canvas
#     already in use
#
# so the leg ends in a surface-creation panic and prints an INVALID row reading
# `adapter=none:None backend=UNKNOWN` -- i.e. the rig reports a fully capable
# machine as having no GPU. That is worse than failing: a row like that gets
# read as a fact about the hardware.
#
# It has a GPU. With these flags instead, requestAdapter() returns an `apple`
# adapter at maxTextureDimension2D=16384 and WebGL2 comes up as
# "ANGLE (Apple, ANGLE Metal Renderer: Apple M2)".
#
# `--disable-gpu-sandbox` is dropped too: it is a Linux workaround and the
# macOS GPU process is happy sandboxed. What is deliberately KEPT is the
# absence of `--enable-unsafe-swiftshader`, so this arm still fails loudly
# rather than quietly measuring a software rasteriser.
CHROMIUM_HARDWARE_ARGS_DARWIN = [
    "--enable-gpu",
    "--ignore-gpu-blocklist",
    "--enable-gpu-rasterization",
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
    # Recorded as DATA, not only rendered into a label, because the row line
    # and the summary line build their adapter field independently and both
    # need to be able to say it. `masked` is decided by the string's own
    # suffix -- firefox's fingerprinting resistance writes it literally -- and
    # `host_gpu` is asked of the OS, which is the only place a web leg can get
    # a true answer.
    out["masked"] = bool(renderer
                         and renderer.endswith(MASKED_RENDERER_SUFFIX))
    if out["masked"]:
        out["host_gpu"] = host_gpu()
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


# Firefox's fingerprinting-resistant renderer string ends in this, literally.
# It is not a hedge this rig added and it is not "close enough": on the box
# these lines were written on, the page says "NVIDIA GeForce GTX 980, or
# similar" while `nvidia-smi` says GeForce RTX 3090, and on an M2 the page
# says "Apple M1, or similar". Chromium does not mask -- it reported the M2
# correctly through ANGLE -- so a row's adapter can be true or masked
# depending only on which browser took it.
MASKED_RENDERER_SUFFIX = ", or similar"

_HOST_GPU_CACHE = []


def host_gpu():
    """What the HOST says it has, asked of the OS rather than of the page.

    The browser's renderer string is the only GPU identity a web leg has, and
    for firefox it is deliberately false. Anything generalising from a web row
    to a class of hardware needs this instead."""
    if _HOST_GPU_CACHE:
        return _HOST_GPU_CACHE[0]
    got = None
    probes = []
    if sys.platform == "darwin":
        probes = [["system_profiler", "SPDisplaysDataType"]]
    else:
        probes = [["nvidia-smi", "--query-gpu=name", "--format=csv,noheader"],
                  ["lspci"]]
    for argv in probes:
        try:
            out = subprocess.run(argv, capture_output=True, text=True,
                                 timeout=20).stdout
        except (OSError, subprocess.SubprocessError):
            continue
        if not out:
            continue
        if argv[0] == "nvidia-smi":
            got = out.strip().splitlines()[0].strip() or None
        elif argv[0] == "lspci":
            for line in out.splitlines():
                if "VGA compatible controller" in line or "3D controller" in line:
                    got = line.split(":", 2)[-1].strip()
                    break
        else:
            for line in out.splitlines():
                if "Chipset Model:" in line:
                    got = line.split(":", 1)[1].strip()
                    break
        if got:
            break
    _HOST_GPU_CACHE.append(got)
    return got


def adapter_label(adapter):
    """One short field for summary lines: `hardware:NVIDIA GeForce RTX 3090`.

    A MASKED renderer says so here rather than only in `classify_adapter`'s
    docstring, because this string is the one that gets copied onto a board.
    It was copied onto one as a fact about the GPU."""
    adapter = adapter or {}
    cls = adapter.get("class", "unknown")
    if cls == "none":
        return "none(%s)" % adapter.get("why", "?")
    renderer = adapter.get("renderer") or "?"
    label = "%s:%s" % (cls, renderer)
    if renderer.endswith(MASKED_RENDERER_SUFFIX):
        label += " [MASKED by the browser, NOT the hardware; host reports: %s]" \
                 % (host_gpu() or "unknown")
    return label


def classify_webgpu_adapter(wg):
    """Name the adapter behind a set of WEBGPU_PROBE readings, as
    `classify_adapter` names the one behind the WebGL probe.

    A second classification because it is a second question. A page the app
    runs on the WebGPU backend has no WebGL2 context by design -- the canvas
    has answered `getContext("webgpu")` and answers no other -- so on that arm
    `classify_adapter` reads `none` while a real GPU is drawing, and a
    hardware floor that consults only WebGL fails the very run the arm exists
    to take. `hardware` here means requestAdapter() returned an adapter that
    the browser does not flag as a fallback (the spec's word for software:
    SwiftShader, and lavapipe, which Dawn types as a CPU adapter) and whose
    info strings carry none of the SOFTWARE_RENDERER_MARKERS. A browser that
    redacts `description` to "" -- chromium does -- leaves the fallback flag
    as the load-bearing test, which is why the probe records it.

    The classes match `classify_adapter`'s (`none`, `software`, `hardware`)
    so a verdict can name which of the two adapters it read."""
    wg = wg or {}
    info = wg.get("adapter_info")
    if not isinstance(info, dict):
        info = {}
    limits = wg.get("adapter_limits")
    if not isinstance(limits, dict):
        limits = {}
    out = {"vendor": info.get("vendor"),
           "architecture": info.get("architecture"),
           "description": info.get("description"),
           "is_fallback": wg.get("adapter_is_fallback"),
           "max_texture_dimension_2d": limits.get("maxTextureDimension2D")}
    if wg.get("probe_error"):
        out["class"] = "none"
        out["why"] = "probe error: %s" % str(wg["probe_error"])[:80]
        return out
    if not wg.get("gpu_object"):
        out["class"] = "none"
        out["why"] = "no navigator.gpu"
        return out
    if not wg.get("adapter"):
        out["class"] = "none"
        out["why"] = "requestAdapter -> null%s" % (
            ("; " + str(wg["error"])[:80]) if wg.get("error") else "")
        return out
    if out["is_fallback"] is True:
        out["class"] = "software"
        out["marker"] = "isFallbackAdapter"
        return out
    haystack = " ".join(str(info.get(k) or "")
                        for k in ("vendor", "architecture", "device",
                                  "description")).lower()
    hit = next((m for m in SOFTWARE_RENDERER_MARKERS if m in haystack), None)
    if hit:
        out["class"] = "software"
        out["marker"] = hit
        return out
    out["class"] = "hardware"
    return out


def webgpu_adapter_label(adapter):
    """`adapter_label`'s counterpart for the WebGPU classification:
    `hardware:nvidia (maxTex2D=16384)`, `software:google [swiftshader]`,
    `none(requestAdapter -> null)`."""
    adapter = adapter or {}
    cls = adapter.get("class", "unknown")
    if cls == "none":
        return "none(%s)" % adapter.get("why", "?")
    name = adapter.get("description") or adapter.get("vendor") or "?"
    label = "%s:%s" % (cls, name)
    if adapter.get("max_texture_dimension_2d") is not None:
        label += " (maxTex2D=%s)" % adapter["max_texture_dimension_2d"]
    if cls == "software":
        label += " [%s]" % adapter.get("marker")
    return label


def hardware_floor(adapter, webgpu_adapter):
    """--require-hardware's decision: the adapter that is a GPU, or None.

    `webgl` when the WebGL renderer classified hardware -- the reading this
    floor has always taken, kept first so the WebGL2 arm's rows do not move;
    `webgpu` when it did not but requestAdapter() returned a hardware
    adapter; None when neither did, i.e. a software or absent WebGL adapter
    beside an absent, null or software WebGPU one."""
    if (adapter or {}).get("class") == "hardware":
        return "webgl"
    if (webgpu_adapter or {}).get("class") == "hardware":
        return "webgpu"
    return None


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
             else (CHROMIUM_HARDWARE_ARGS_DARWIN if sys.platform == "darwin"
                   else CHROMIUM_HARDWARE_ARGS))
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


# --------------------------------------------------------------------------
# Android: BOTH engines, because one engine is not "web"
# --------------------------------------------------------------------------
#
# Until 2026-08-31 `--android` accepted chromium and nothing else, so every
# Android figure this campaign holds is Blink's. Firefox governs the web
# target here and runs roughly 2x Chromium's service time on the desktop, so a
# Blink-only Android column reports the engine that tends to win and calls the
# result "web". That is the defect these two capability builders exist to
# close, and `the_android_mode_is_not_one_engine` in drive.py's selftest is
# what keeps it closed.
#
# THE GECKODRIVER CONTRACT BELOW WAS READ OUT OF THE PINNED BINARY -- the
# geckodriver 0.37.1 that ensure-geckodriver.sh provisions -- not taken on
# trust from documentation:
#
#   * `moz:firefoxOptions` accepts androidPackage, androidActivity,
#     androidDeviceSerial and androidIntentArguments. There is NO
#     `androidStorage` CAPABILITY: `--android-storage` is a (deprecated)
#     geckodriver command-line flag and appears nowhere in the capability
#     parser.
#   * "androidPackage and binary are mutual exclusive" is a literal error
#     string in that binary. The desktop path ALWAYS sends `binary`, so the
#     Android path must not -- and getting this wrong fails at session
#     creation, on the phone, three minutes into a leg.
#   * "Cannot use a named profile on Android".
#   * androidPackage is validated against
#     ^([a-zA-Z][a-zA-Z0-9_]*\.){1,}([a-zA-Z][a-zA-Z0-9_]*)$ -- at least one
#     dot, and NO hyphens. Rejection message: "Not a valid androidPackage
#     name".
#   * androidActivity "should not contain '/'": geckodriver composes
#     `am start -S -W -n <package>/<activity>` itself.
#   * geckodriver carries default activities for the packages it ships with
#     (org.mozilla.fenix.IntentReceiverActivity for the Firefox/Fenix family,
#     org.mozilla.focus.activity.IntentReceiverActivity for Focus/Klar), so
#     androidActivity is optional for a release Firefox and is exposed here
#     only for a build geckodriver does not know.
#   * prefs reach the app through a GeckoView configuration YAML that
#     geckodriver pushes to the device and arms with `am set-debug-app` -- not
#     through a profile. So the rig's prefs still apply, but whether they
#     LANDED is a device question, not a host one (see the pre-flight
#     checklist: `privacy.reduceTimerPrecision` is the one that decides
#     whether the rAF percentiles are integers or microseconds).
#
# NOT VALIDATED AGAINST HARDWARE. No phone was attached when this landed. What
# is exercised from this box is the argument validation and the capability
# shape; every device-side answer is enumerated in the checklist rather than
# guessed at here.

ANDROID_BROWSERS = ("chromium", "firefox")

# THE DRIVERS WIPE THE BROWSER'S DATA BEFORE EVERY SESSION. This is not a
# hazard to be careful about; it is what they do, unconditionally, and the rig
# refuses rather than documents it.
#
# MEASURED 2026-08-31 from geckodriver 0.37.1's own `--log trace`, on the
# user's phone, on a default invocation that asked for nothing of the kind:
#
#   mozdevice TRACE execute_host_command: >> "shell:pm clear org.mozilla.firefox"
#   mozdevice TRACE execute_host_command: << "Success\n"
#
# `pm clear` deletes the app's data: profile, tabs, logins, bookmarks,
# history. The user opened Firefox afterwards and got the first-run welcome
# wizard, which is what a wiped profile looks like from the outside. The same
# thing had already cost them their Chrome earlier that day through the Blink
# Android path.
#
# There is NO CAPABILITY TO TURN IT OFF. geckodriver's capability parser
# accepts exactly androidActivity, androidDeviceSerial, androidPackage,
# profile, androidIntentArguments, binary, env, log and prefs (read out of the
# binary's string table); none of them gates the clear, and the deprecated
# `--android-storage` flag only chooses WHERE the driver's own test_root goes.
# `moz:firefoxOptions.profile` is a profile the driver PUSHES, not the app's
# own -- and "Cannot use a named profile on Android" is a literal refusal in
# the same binary.
#
# So the only safe way to drive Gecko on a phone somebody actually uses is to
# drive a DIFFERENT PACKAGE. Firefox Beta and Nightly are separate installs
# with separate storage, so the release browser is never opened at all.
ANDROID_DAILY_DRIVER_PACKAGES = {
    "org.mozilla.firefox":
        "release Firefox -- somebody's daily browser. geckodriver runs "
        "`pm clear org.mozilla.firefox` before every session (MEASURED, and "
        "it returned Success on a real phone), which deletes tabs, logins, "
        "bookmarks and history. Drive org.mozilla.firefox_beta or "
        "org.mozilla.fenix instead: separate installs, separate storage, same "
        "engine",
    "com.android.chrome":
        "release Chrome -- somebody's daily browser. chromedriver's Android "
        "mode clears the app's data the same way, and it has already cost "
        "this project's user their Chrome once. Drive a Beta/Dev channel "
        "package instead",
}

# THE BLINK ANDROID PATH IS ALLOWED EXACTLY ONE PACKAGE, BY NAME.
#
# The refusal above says "drive a Beta/Dev channel package instead", and until
# 2026-08-31 that advice pointed at nothing: the device had only release
# Chrome, so the guard's own recommendation was unreachable and the chromium
# Android path had no package it was allowed to drive at all.
#
# The user then installed Chrome Beta (com.chrome.beta, 153.0.8010.18,
# 2026-08-31 21:39) specifically to unblock this, and explicitly authorised
# the rig to let chromedriver wipe THAT package and no other. So the allowance
# is spelled as a one-entry allow-list rather than as "anything not on the
# refusal list": before this, com.chrome.dev, com.chrome.canary and every
# typo'd package name were all silently accepted, which is a much wider
# permission than anybody granted.
#
# Adding a name here is granting permission to delete that app's data on every
# session. It is not a place to add a package because a run failed.
ANDROID_CHROMIUM_ALLOWED_PACKAGES = ("com.chrome.beta",)

# The package each engine means by "the browser on the phone". Resolved from
# the browser rather than defaulted on the flag, because one default cannot be
# right for two engines -- and a Chrome package silently handed to geckodriver
# would fail with a package-not-found on the device rather than at the CLI.
# Firefox defaults to BETA, not release, because the default is the thing that
# runs when nobody thought about it and the driver wipes whatever it drives.
# Beta was already installed on the device (`pm list packages` returned both
# org.mozilla.firefox_beta and org.mozilla.firefox), so this costs nothing.
# Chromium keeps its historical default so the existing Blink rows stay
# describable -- but the daily-driver refusal below applies to it too, so
# reaching that package now takes a conscious flag.
ANDROID_DEFAULT_PACKAGE = {
    "chromium": "com.android.chrome",
    "firefox": "org.mozilla.firefox_beta",
}

# geckodriver's own androidPackage validator, verbatim. Applied at the CLI so
# a typo costs milliseconds instead of a device session. Firefox only: the
# chromium path predates this and stays exactly as it was.
ANDROID_PACKAGE_RE = re.compile(
    r"^([a-zA-Z][a-zA-Z0-9_]*\.){1,}([a-zA-Z][a-zA-Z0-9_]*)$")

# The packages the pinned geckodriver knows a default activity for. Not a
# restriction -- any package is accepted -- but one this rig can WARN about,
# because an unknown package needs --android-activity and the failure without
# it happens on the phone.
GECKODRIVER_KNOWN_PACKAGES = (
    "org.mozilla.firefox",
    "org.mozilla.firefox_beta",
    "org.mozilla.fenix",
    "org.mozilla.fenix.debug",
    "org.mozilla.reference.browser",
    "org.mozilla.focus",
    "org.mozilla.focus.debug",
    "org.mozilla.klar",
    "org.mozilla.klar.debug",
)


# Firefox on Android comes up in first-run onboarding, in front of the page,
# on every geckodriver launch -- because geckodriver pushes a FRESH profile
# each time and onboarding is a property of a fresh profile. A human tapped
# through it on every leg taken before this, which means those legs were not
# unattended and their figures are provisional.
#
# These are set on the AUTOMATION profile, which geckodriver creates and
# destroys; the user's own profile is a different directory and is never
# opened by any of this. Applied on the Android arm only, so no desktop
# Firefox row moves.
#
# WHICH OF THESE FENIX ACTUALLY HONOURS IS AN OPEN QUESTION, and the honest
# answer needs the device: Fenix's onboarding is Kotlin UI over Android
# SharedPreferences, and a Gecko pref cannot reach that layer. If the wizard
# still appears with all of these set, the finding is that prefs cannot
# suppress it and the only unattended route is a package whose onboarding has
# already been completed once.
FIREFOX_ANDROID_PREFS = {
    # Geolocation. The app's UserLocation layer is in the seed's enabled set,
    # so the page asks for a position and Fenix puts a prompt in front of
    # everything. Unlike the onboarding wizard -- Android SharedPreferences,
    # out of reach of any pref -- site permissions ARE Gecko profile state, so
    # this is the right layer to answer at. 1 = allow, 2 = block; the value is
    # recorded on every row either way, because granted and denied are
    # different app behaviours (the location handler draws on the Ground
    # surface and feeds a content key) and rows taken under the two are not
    # comparable.
    "permissions.default.geo": 1,
    # And the position itself, so a leg never waits on a real GPS fix that a
    # phone indoors may never produce. Recorded as the denominator it is.
    "geo.provider.network.url":
        "data:application/json,{\"location\":{\"lat\":35.33,\"lng\":-97.28},"
        "\"accuracy\":100.0}",
    "geo.prompt.testing": True,
    "geo.prompt.testing.allow": True,
    "browser.aboutwelcome.enabled": False,
    "browser.onboarding.enabled": False,
    "datareporting.policy.dataSubmissionPolicyBypassNotification": True,
    "browser.startup.homepage_override.mstone": "ignore",
    "toolkit.telemetry.reportingpolicy.firstRun": False,
}


def validate_android_args(browser, package, activity, adb_present=True,
                          allow_daily_driver=False, clear_app_data=False):
    """Is this `--android` invocation drivable? Returns the reason it is not,
    or None.

    Pure and side-effect-free ON PURPOSE: the mode cannot be smoke-tested
    without a device, so the validator is the only part of it this box can
    exercise, and it can only be exercised if it does not need a parser, a
    driver or a phone to run. `main()` funnels every `--android` refusal
    through here and the selftest drives the same function over the whole
    matrix."""
    if browser not in ANDROID_BROWSERS:
        return ("--android drives %s on the device; --browser %s has no "
                "Android path (chromium: chromedriver + "
                "goog:chromeOptions.androidPackage; firefox: geckodriver + "
                "moz:firefoxOptions.androidPackage)"
                % (" and ".join(ANDROID_BROWSERS), browser))
    if not adb_present:
        return "--android needs adb on PATH (for `adb reverse`)"
    if browser == "firefox":
        if package is not None and not ANDROID_PACKAGE_RE.match(package):
            return ("--android-package %r is not a valid androidPackage name: "
                    "geckodriver requires dot-separated segments of "
                    "[A-Za-z][A-Za-z0-9_]* (no hyphens, at least one dot)"
                    % package)
        if activity and "/" in activity:
            return ("--android-activity must not contain '/': geckodriver "
                    "composes `am start -n <package>/<activity>` itself")
    elif activity:
        return ("--android-activity is a geckodriver capability; the chromium "
                "Android path drives the package's launcher activity and does "
                "not take one")
    # Last, so a malformed package is named as malformed rather than as
    # somebody's browser. Resolved default included: the default is precisely
    # the case where nobody chose, and "nobody chose" is how the user's Chrome
    # and then their Firefox were wiped on the same day.
    effective = package or ANDROID_DEFAULT_PACKAGE[browser]
    reason = ANDROID_DAILY_DRIVER_PACKAGES.get(effective)
    if reason and not allow_daily_driver:
        return ("REFUSING to drive %s: %s.\n"
                "This is not a warning to read past -- the driver deletes the "
                "app's data before the session starts and nothing in the rig "
                "can prevent it. If this really is a throwaway device, pass "
                "--android-allow-daily-driver and accept that the browser's "
                "tabs, logins, bookmarks and history are gone."
                % (effective, reason))
    # Last of all, and chromium-only: the allow-list. This runs AFTER the
    # daily-driver refusal so com.android.chrome keeps its own, louder message
    # naming what it costs, rather than being reported as merely "not on a
    # list". Everything else that is not the one authorised package lands here.
    if (browser == "chromium" and not allow_daily_driver
            and effective not in ANDROID_CHROMIUM_ALLOWED_PACKAGES):
        return ("REFUSING to drive %s: the Blink Android path is allowed "
                "exactly one package -- %s -- and that is the only package "
                "anybody authorised chromedriver to wipe. chromedriver's "
                "Android mode deletes the app's data before every session, so "
                "an unrecognised Chrome package here is somebody's browser "
                "until proven otherwise. If this really is a throwaway "
                "device, pass --android-allow-daily-driver."
                % (effective, ", ".join(ANDROID_CHROMIUM_ALLOWED_PACKAGES)))
    # --android-clear-app-data asks for the wipe ON PURPOSE, so it is fenced
    # to the authorised package by NAME and not merely by the allow-list
    # above: --android-allow-daily-driver opens that list to anything, and the
    # combination of the two flags is the one way to aim a deliberate wipe at
    # somebody's daily browser. The escape hatch may unlock driving a package;
    # it does not unlock deleting one that was never authorised.
    if clear_app_data and effective not in ANDROID_CHROMIUM_ALLOWED_PACKAGES:
        return ("REFUSING --android-clear-app-data for %s: that flag deletes "
                "the app's data before every session and it is authorised "
                "for %s alone. --android-allow-daily-driver unlocks DRIVING "
                "a package, not wiping one nobody named."
                % (effective, ", ".join(ANDROID_CHROMIUM_ALLOWED_PACKAGES)))
    return None


def chromium_android_capabilities(package, use_running_app=False,
                                  keep_app_data=True):
    """chromedriver + the phone's own Chrome, WITHOUT wiping it.

    `androidKeepAppDataDir` is the capability that stops the clear. Its
    absence is why every Blink Android figure this campaign holds was taken on
    a freshly `pm clear`ed browser -- cold profile, cold HTTP cache, cold
    service worker, on every single pass. That is an unstated denominator on
    all of them and it is not what a real user's browser looks like; it may
    well be the whole reason those rows read as outliers.

    Both spellings were read out of /usr/bin/chromedriver's own string table,
    beside the literal `pm clear ` that adjoins `|shell:`:
      androidKeepAppDataDir   do not delete the app's data directory
      androidUseRunningApp    attach to the running app instead of
                              restart-and-clear -- which means the CALLER MUST
                              HAVE LAUNCHED IT, handled in launch() rather
                              than left as a trap

    NOT VALIDATED AGAINST HARDWARE. Proving the data survives requires driving
    a Chrome package, and the only one on this device is the user's daily
    browser -- there is no Beta or Dev channel installed. The daily-driver
    guard therefore STAYS exactly as it was: this makes a safe Chrome row
    possible in principle, and nothing here is evidence that it is safe yet.

    `keep_app_data=False` sends NOTHING and lets chromedriver do its default
    thing, which is to `pm clear` the package on every session. That is a
    deliberate, authorised option for com.chrome.beta and it buys a
    denominator: every pass then starts from the same cleared state, so
    pass-to-pass variance is not contaminated by a cache that warmed up
    somewhere in the middle of the run. It is also the PESSIMISTIC posture --
    a cold HTTP cache and a cold service worker on every pass is not what a
    returning user pays -- and `profile_state` on the row is what keeps those
    two kinds of figure from ever being quoted as one."""
    opts = {"androidPackage": package}
    if keep_app_data:
        opts["androidKeepAppDataDir"] = True
    if use_running_app:
        opts["androidUseRunningApp"] = True
    return {"capabilities": {"alwaysMatch": {
        "browserName": "chrome",
        "acceptInsecureCerts": True,
        "goog:chromeOptions": opts,
        "goog:loggingPrefs": {"browser": "ALL"},
    }}}


def firefox_capabilities(binary, window, headless=True, extra_prefs=None,
                         android_package=None, android_activity=None,
                         android_serial=None):
    """The desktop and Android capability shapes from ONE builder.

    With `android_package` set, three things change and nothing else does:
    `binary` is omitted (geckodriver refuses both together), the window/
    headless args are dropped (a phone has neither), and the android
    capabilities are added. The prefs -- including the timer-precision
    override every frame percentile depends on -- are shared, which is the
    reason this is an extension and not a second builder."""
    android = android_package is not None
    args = []
    if not android:
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
    # Insertion order is preserved deliberately on the desktop arm: the dict
    # below is serialised straight onto the wire, and this builder's job on
    # that arm is to be byte-for-byte what it was before Android existed.
    if android:
        opts = {"androidPackage": android_package,
                "args": args,
                "prefs": prefs}
        if android_activity:
            opts["androidActivity"] = android_activity
        if android_serial:
            opts["androidDeviceSerial"] = android_serial
    else:
        opts = {"binary": binary, "args": args, "prefs": prefs}
    return {"capabilities": {"alwaysMatch": {
        "browserName": "firefox",
        "acceptInsecureCerts": True,
        "moz:firefoxOptions": opts,
    }}}


def launch(browser, out_dir, tag, driver_path=None, binary=None,
           window=(1280, 900), headless=True, tmp_root=None,
           ff_prefs=None, extra_env=None, ff_mode="auto", arm="software",
           display=None, chromium_args=(), android=False,
           android_package=None, android_activity=None,
           android_serial=None, android_use_running_app=False,
           android_keep_app_data=True):
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
    if android:
        android_package = android_package or ANDROID_DEFAULT_PACKAGE[browser]
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
    #
    # Safari is exempt: it runs on macOS, where there is no X display to
    # resolve and Quartz is the only compositor. Probing /tmp/.X11-unix there
    # answers {"display": None, "why": ...}, which is not a fact about the
    # leg -- it would be recorded as `host_display` and read as though a
    # display lookup had failed. Safari also has no software arm to fall back
    # to (WebKit always reaches the real GPU), so the hardware arm needs no
    # display negotiation to be honest about what rendered.
    #
    # Firefox-on-Android is exempt for exactly Safari's reason: the phone's own
    # compositor is what renders and there is no X display anywhere in the
    # story, so recording this box's :0 on such a leg files a HOST fact as a
    # leg fact. Chromium-on-Android is deliberately NOT exempted even though
    # the same argument applies to it: it resolves and records the display
    # today, the Blink Android rows already taken carry that field, and this
    # change is additive. Fixing that is a separate, non-additive edit.
    host = None
    if arm == "hardware" and browser != "safari" and sys.platform != "darwin" \
            and not (android and browser == "firefox"):
        # The darwin exemption is safari's, for safari's reason: on macOS there
        # is no X display to resolve, and recording a failed lookup as
        # `host_display` would read as though one had been attempted and lost.
        host = resolve_host_display(display)

    if browser == "chromium" and android:
        # The Android mode: chromedriver drives the phone's own Chrome over
        # adb. No binary is picked (the package IS the binary), no headless
        # flag exists, and the arm flags are moot -- the phone's GPU answers.
        # The caller must have `adb reverse tcp:P tcp:P` in place before
        # navigating (run_smoke does it), or 127.0.0.1 on the phone is the
        # phone. IMPLEMENTED PER SPEC, NOT DEVICE-TESTED IN THIS TREE: no
        # Android device was attached when this landed; arg validation and
        # the wire shape are what is exercised.
        driver_path = driver_path or DEFAULT_CHROMEDRIVER
        caps = chromium_android_capabilities(
            android_package, use_running_app=android_use_running_app,
            keep_app_data=android_keep_app_data)
        if android_use_running_app:
            # androidUseRunningApp attaches to a running app and does NOT
            # start one. Left to the caller this is a trap: chromedriver fails
            # with a bare "no chrome binary" style error and nothing says the
            # app simply was not up. So the rig starts it and waits.
            _adb(["shell", "monkey", "-p", android_package, "-c",
                  "android.intent.category.LAUNCHER", "1"], android_serial)
            time.sleep(3.0)
        argv = [driver_path, "--port=%d" % port, "--log-level=INFO"]
        info = {"android_package": android_package,
                "driver_version": _version_of(driver_path),
                "gpu_mode": "android-device",
                "android_keep_app_data_dir": bool(android_keep_app_data),
                "android_use_running_app": bool(android_use_running_app),
                # THE DENOMINATOR THAT WAS MISSING. Every Blink Android row
                # taken before 2026-08-31 was taken on a browser chromedriver
                # had just `pm clear`ed, and none of them said so.
                #
                # This landed as a conditional whose test was the constant
                # true, so it could not take its other branch: the field could
                # only ever print "preserved", and a cleared-profile row would
                # have described itself as a warm one. Both branches are now
                # reachable and the flag that selects them is on the row
                # beside this.
                "profile_state": ("preserved (androidKeepAppDataDir)"
                                  if android_keep_app_data
                                  else "cleared (chromedriver pm clear, "
                                       "every pass)")}
    elif browser == "chromium":
        driver_path = driver_path or DEFAULT_CHROMEDRIVER
        pick = pick_chromium_binary(driver_path, preferred=binary)
        chrome_headless = headless
        if arm == "hardware" and sys.platform == "darwin":
            # Same intent as the X11 branch below -- headed on the real
            # compositor -- but on macOS there is no display to hand over:
            # Quartz is the only one and chrome finds it itself. Without this
            # the darwin leg stays headless while the firefox and safari legs
            # beside it are headed, which is a window-system difference that
            # gets read later as an engine difference.
            chrome_headless = False
        elif arm == "hardware" and host and host.get("display"):
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
        if not chrome_headless:
            # The label is a denominator, so it names the compositor actually
            # used rather than implying an X display on a box that has none.
            info["gpu_mode"] = ("macos-quartz-headed"
                                if sys.platform == "darwin"
                                else "headed-host-display")
        else:
            info["gpu_mode"] = ("headless-angle-egl" if arm == "hardware"
                                else "headless-swiftshader")
    elif browser == "firefox" and android:
        # geckodriver drives the phone's own Firefox over adb, the same way
        # chromedriver drives its Chrome: the package IS the binary. See the
        # contract block above for what was read out of the pinned binary --
        # the load-bearing line is that `binary` and `androidPackage` are
        # mutually exclusive, so DEFAULT_FIREFOX is never filled in here and
        # ff_mode is not negotiated at all (there is no display to negotiate).
        #
        # FIREFOX_HARDWARE_PREFS are deliberately NOT applied. On the desktop
        # they exist to defeat a graphics blocklist that would answer with
        # llvmpipe; on the phone there is no llvmpipe to fall back to, and
        # forcing webgl on would hide a real blocklist decision that a user
        # would also be subject to. --require-hardware is what makes this arm
        # able to fail, exactly as the desktop comment says.
        #
        # IMPLEMENTED PER THE PINNED DRIVER'S CONTRACT, NOT DEVICE-TESTED IN
        # THIS TREE: no Android device was attached when this landed.
        driver_path = driver_path or (shutil.which("geckodriver")
                                      or DEFAULT_GECKODRIVER)
        argv = [driver_path, "--port", str(port), "--log", "info"]
        android_prefs = dict(FIREFOX_ANDROID_PREFS)
        android_prefs.update(ff_prefs or {})
        caps = firefox_capabilities(None, window, headless=False,
                                    extra_prefs=android_prefs,
                                    android_package=android_package,
                                    android_activity=android_activity,
                                    android_serial=android_serial)
        info = {"android_package": android_package,
                "driver_version": _version_of(driver_path),
                "gpu_mode": "android-device",
                "ff_mode": "android"}
        if android_activity:
            info["android_activity"] = android_activity
        else:
            info["android_activity"] = (
                "geckodriver default"
                if android_package in GECKODRIVER_KNOWN_PACKAGES
                else "UNKNOWN PACKAGE: geckodriver has no default activity "
                     "for %r; pass --android-activity or the launch fails on "
                     "the device" % android_package)
        if android_serial:
            info["android_device_serial"] = android_serial
    elif browser == "firefox":
        driver_path = driver_path or (shutil.which("geckodriver")
                                      or DEFAULT_GECKODRIVER)
        binary = binary or DEFAULT_FIREFOX
        argv = [driver_path, "--port", str(port), "--log", "info"]
        info = {"binary": binary, "browser_version": _version_of(binary),
                "driver_version": _version_of(driver_path)}
        mode = ff_mode
        if sys.platform == "darwin":
            # macOS has no X display to negotiate and Quartz is the only
            # compositor, so none of the display modes below apply. See
            # FirefoxDirectProcess for why the browser is launched by the rig
            # and geckodriver is pointed at it, rather than the other way
            # round.
            mode = "macos-connect"
        elif mode == "auto" and arm == "hardware":
            mode = "host"
        elif not headless:
            mode = "headed"                      # caller brings the display
        elif mode == "auto":
            mode = "xvfb" if shutil.which("Xvfb") else "headless"
        info["ff_mode"] = mode
        if mode == "macos-connect":
            prefs = dict(FIREFOX_HARDWARE_PREFS) if arm == "hardware" else {}
            prefs.update({
                "browser.shell.checkDefaultBrowser": False,
                "datareporting.policy.dataSubmissionEnabled": False,
                "app.update.disabledForTesting": True,
                "browser.sessionstore.resume_from_crash": False,
                "privacy.reduceTimerPrecision": False,
            })
            prefs.update(ff_prefs or {})
            mport = free_port()
            profile_dir = os.path.join(tmp_root or out_dir,
                                       "%s-ffprofile" % tag)
            ff = FirefoxDirectProcess(
                binary, profile_dir, mport, window, prefs=prefs,
                log_path=os.path.join(out_dir, "%s.firefox.log" % tag),
                env=env)
            aux_procs.append(ff)
            try:
                got_port = ff.wait_ready()
            except Exception:
                ff.stop()
                raise
            argv = [driver_path, "--port", str(port), "--log", "info",
                    "--connect-existing", "--marionette-port", str(got_port)]
            # connect-existing ignores moz:firefoxOptions entirely (the browser
            # is already up), so the prefs above went in via user.js and the
            # caps here are the bare minimum the W3C handshake needs.
            caps = {"capabilities": {"alwaysMatch": {"browserName": "firefox"}}}
            info["marionette_port"] = got_port
            info["profile_dir"] = profile_dir
            info["profile_is_throwaway"] = True
            info["prefs_via"] = "user.js (connect-existing)"
            info["prefs"] = dict(prefs)
            info["gpu_mode"] = "macos-quartz-headed"
        elif mode == "host":
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
    elif browser == "safari":
        # The safaridriver mode: an external driver the rig does not manage
        # beyond starting it on a port -- Apple ships it with macOS and it
        # drives the system Safari (or, with `--driver .../safaridriver` on
        # an iOS-paired Mac, a device through Safari's remote automation).
        # IMPLEMENTED PER SPEC, NOT EXECUTED IN THIS TREE: there is no macOS
        # or safaridriver on this box; the wire shape is W3C-standard and the
        # probes above are all plain /execute scripts, but nothing here has
        # met a real Safari. acceptInsecureCerts is deliberately absent --
        # safaridriver rejects it; TLS legs need the mkcert CA trusted on the
        # device instead (serve.py's header documents the provisioning).
        driver_path = driver_path or shutil.which("safaridriver")
        if not driver_path:
            raise WebDriverError("no safaridriver binary; pass --driver")
        argv = [driver_path, "-p", str(port)]
        caps = {"capabilities": {"alwaysMatch": {"browserName": "safari"}}}
        info = {"binary": "safari (system)",
                "driver_version": _version_of(driver_path)}
    else:
        raise ValueError("browser must be chromium, firefox or safari")
    info["driver_path"] = driver_path
    info["arm"] = arm
    if host is not None:
        info["host_display"] = dict(host)
    if browser == "chromium" and not android:
        info["chromium_args"] = (caps["capabilities"]["alwaysMatch"]
                                 ["goog:chromeOptions"]["args"])
    if tmp_note:
        info["tmp_note"] = tmp_note
    if extra_env:
        env.update(extra_env)
        info["extra_env"] = dict(extra_env)
    if browser == "firefox" and "prefs" not in info:
        # macos-connect has already recorded its prefs: they never rode on the
        # capabilities there (the browser was up before geckodriver attached),
        # they went into the profile's user.js. Reading them back off a cap
        # that is absent by design would KeyError.
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


def adb_reverse(port, serial=None):
    """Map the device's 127.0.0.1:<port> back to this host over adb, so the
    served rig URL works unchanged inside the phone's browser -- and, because
    127.0.0.1 is a secure context by definition, so does cross-origin
    isolation, with no TLS to provision. Engine-agnostic: geckodriver does its
    own `adb forward` for Marionette but nothing forwards the SERVED port, so
    this is needed identically on both. See the launch() branches for the
    device-untested status."""
    adb = shutil.which("adb")
    if not adb:
        raise WebDriverError("--android needs adb on PATH for `adb reverse`")
    argv = [adb] + (["-s", serial] if serial else []) + \
           ["reverse", "tcp:%d" % port, "tcp:%d" % port]
    r = subprocess.run(argv, capture_output=True, text=True, timeout=20)
    if r.returncode != 0:
        raise WebDriverError("adb reverse failed rc=%d: %s"
                             % (r.returncode, (r.stderr or r.stdout).strip()))
    # Registered for teardown. Each leg serves on a fresh kernel-chosen port,
    # so a rig that only ever ADDS mappings leaves one per leg behind on
    # somebody's phone -- thirteen of them after one evening, measured. The
    # rig cleans up what it creates.
    _ADB_REVERSES.append((port, serial))
    return " ".join(argv)


_ADB_REVERSES = []


def adb_reverse_cleanup():
    """Remove every `adb reverse` mapping this process created. Best effort:
    a mapping that will not come off is a note, never a failed leg."""
    removed = []
    while _ADB_REVERSES:
        port, serial = _ADB_REVERSES.pop()
        adb = shutil.which("adb")
        if not adb:
            break
        argv = [adb] + (["-s", serial] if serial else []) + \
               ["reverse", "--remove", "tcp:%d" % port]
        try:
            if subprocess.run(argv, capture_output=True, text=True,
                              timeout=20).returncode == 0:
                removed.append(port)
        except Exception:                          # noqa: BLE001 - never fatal
            pass
    return removed


# --------------------------------------------------------------------------
# Fenix onboarding: dismissed from the accessibility tree, never from pixels
# --------------------------------------------------------------------------
#
# geckodriver pushes a FRESH profile and `pm clear`s the package on every
# launch (both MEASURED from its own trace), so Firefox comes up in first-run
# onboarding, in front of the page, every single time. A human tapped through
# it on every leg taken before this, which means those legs were not
# unattended and their figures were provisional.
#
# PREFS CANNOT DO THIS. Fenix's wizard state lives in Android
# SharedPreferences under /data/data/<pkg>/shared_prefs/ -- Kotlin app state --
# and `moz:firefoxOptions.prefs` writes the GECKO PROFILE. All five candidate
# prefs were set, confirmed delivered in the artifact, and the wizard still
# appeared. That is a measured negative, not an untried idea.
#
# AND NOT FROM PIXELS EITHER. A screenshot plus hardcoded tap coordinates
# breaks on a different density, theme, locale, or -- on the device this runs
# against -- a fold. `uiautomator dump` hands back every node's resource-id,
# text and bounds, so a control is located by IDENTITY and its centre computed
# from its OWN bounds. Nothing here guesses where anything is.
#
# The parsing is split out from the adb I/O so the whole decision procedure --
# including the case that must FAIL -- is exercised offline by the selftest,
# with no phone, no driver and no browser.

# Resource-id fragments for the onboarding controls, most-preferred first.
# Matched as a SUFFIX after the `<package>:id/` prefix, so the same list works
# for release, beta and nightly packages.
ONBOARDING_ID_FRAGMENTS = (
    "onboarding_",
    "primary_button",
    "secondary_button",
    "positive_button",
    "negative_button",
    "skip_button",
    "close_button",
)

# System dialogs are a DIFFERENT package sitting in front of ours, so they are
# matched separately and always DECLINED. MEASURED on this device: after the
# welcome screen, Fenix raises the platform "Set Firefox Beta as your default
# browser" chooser (com.android.permissioncontroller), listing Chrome as the
# current default. `android:id/button2` is its Cancel.
SYSTEM_DECLINE_ID_FRAGMENTS = (
    "permission_deny_button",
    "button2",                  # the platform AlertDialog's negative button
    "cancel_button",
)
SYSTEM_DECLINE_TEXT = (
    "cancel", "not now", "no thanks", "don't allow", "dont allow", "deny",
    "skip", "maybe later", "later",
)

# NEVER TAPPED, on any screen, by any matcher. These change the DEVICE, not
# the wizard: the rig is a guest on somebody's phone and making Firefox Beta
# the default browser is a settings change nobody asked for -- and the chooser
# that offers it also lists the user's real Chrome and release Firefox.
#
# This filter runs BEFORE classification, so it applies to id matches and text
# matches alike. A control the rig cannot identify is skipped; a control it
# identifies as device-altering is refused.
NEVER_TAP_TEXT = (
    "set as default", "set default", "make default", "set as browser",
    "allow", "always allow", "allow all the time", "while using the app",
    "turn on", "enable", "sign in", "sync", "import",
    # Clickable and right next to the wizard's exit on the home screen. A
    # private window would load the page with a different storage partition
    # and no persistence -- a different browser, measured under this one's name.
    "private browsing", "private tab",
)

# "JUMP BACK IN" -- the user's own route, and the last step of this pipeline.
#
# Fenix loads the navigated page into a tab and then keeps its HOME SCREEN in
# front of it: the page is complete, the rayon pool is up, and the GeckoView
# has never been laid out (canvas 0x0 at the untouched 300x150 default). The
# home screen's recent-tabs card is what brings that tab forward, and tapping
# it by hand is exactly what a human was doing before every leg.
#
# MEASURED on Fenix 155, and the trap this exists for: NONE of these nodes
# carry clickable="true". They are Compose semantics nodes, tappable but not
# marked, so the clickable filter that finds every onboarding button excludes
# every one of them. This lookup deliberately ignores that attribute.
RECENT_TAB_ID_FRAGMENTS = (
    "recent.tab.title",
    "recent.tab.url",
    "recent.tabs",
)

# English-only text fallback, used ONLY when no id matched. The locale
# limitation is real and is recorded on the result as the mechanism that
# fired, so a row can say whether it converged by identity or by guessing at
# a language.
ONBOARDING_TEXT_HINTS = (
    "start browsing", "get started", "not now", "skip", "maybe later",
    "no thanks", "continue", "done", "next",
)

# The browser is REACHABLE when one of these is on screen. This is the
# non-triviality floor: the dismissal does not get to succeed by doing
# nothing, it has to prove it arrived somewhere.
#
# `homepageView` counts because Fenix legitimately shows its home screen when
# the current tab is blank, which is where a dismissed wizard lands and is a
# correct place to stop tapping. It is NOT evidence that a page will render --
# see verify_android_content_view, which is the check that catches the state
# this list cannot: chrome present, content view never laid out.
BROWSER_READY_ID_FRAGMENTS = (
    "mozac_browser_toolbar_url_view",
    "engineView",
    "homepageView",
    "toolbar",
)


def _uia_bounds_center(bounds):
    """`[x1,y1][x2,y2]` -> (cx, cy), or None when unparseable.

    The centre comes from the node's OWN bounds every time. There is no
    fallback to a remembered coordinate: a tap this function cannot place is a
    tap that does not happen."""
    m = re.match(r"^\[(-?\d+),(-?\d+)\]\[(-?\d+),(-?\d+)\]$", (bounds or "").strip())
    if not m:
        return None
    x1, y1, x2, y2 = (int(g) for g in m.groups())
    if x2 <= x1 or y2 <= y1:
        return None
    return ((x1 + x2) // 2, (y1 + y2) // 2)


def _uia_id_suffix(resource_id):
    """`org.mozilla.firefox_beta:id/primary_button` -> `primary_button`."""
    return (resource_id or "").rsplit("/", 1)[-1]


def _uia_area(bounds):
    m = re.match(r"^\[(-?\d+),(-?\d+)\]\[(-?\d+),(-?\d+)\]$", (bounds or "").strip())
    if not m:
        return 1 << 62
    x1, y1, x2, y2 = (int(g) for g in m.groups())
    return max(0, x2 - x1) * max(0, y2 - y1)


def _uia_labels(node):
    """Every text/content-desc at or under this node.

    Fenix's Compose onboarding buttons have neither of their own -- the label
    is a non-clickable child -- so a clickable node's identity has to be read
    from its subtree or it has no identity at all."""
    out = []
    for n in node.iter("node"):
        for key in ("text", "content-desc"):
            v = (n.attrib.get(key) or "").strip()
            if v:
                out.append(v)
    return out


def android_parse_hierarchy(xml_text, package):
    """Decide what to do with one `uiautomator dump`, without touching a phone.

    Returns {"reachable", "targets", "nodes", "packages"}:
      reachable  the browser's own URL bar / content view is on screen, so
                 there is nothing left to dismiss
      targets    clickable onboarding or permission controls, each with the
                 centre computed from its own bounds, best candidate first

    THE CASE THAT MATTERS is `reachable False, targets []`: a hierarchy with
    no onboarding to act on and no browser either. That is not success and
    must never be reported as such -- it is the shape a silent fall-through
    would take, and it would navigate into a wizard and measure the app not
    running."""
    out = {"reachable": False, "targets": [], "nodes": 0, "packages": []}
    try:
        root = ET.fromstring(xml_text or "")
    except ET.ParseError as e:
        out["error"] = "uiautomator dump is not parseable XML: %s" % e
        return out
    seen_packages = set()
    ids, perms, texts = [], [], []
    for node in root.iter("node"):
        out["nodes"] += 1
        a = node.attrib
        pkg = a.get("package") or ""
        if pkg:
            seen_packages.add(pkg)
        rid = _uia_id_suffix(a.get("resource-id"))
        if any(f in rid for f in BROWSER_READY_ID_FRAGMENTS):
            out["reachable"] = True
        if a.get("clickable") != "true" or a.get("enabled") == "false":
            continue
        center = _uia_bounds_center(a.get("bounds"))
        if center is None:
            continue
        # MEASURED on Fenix 155 (Beta): the onboarding is Compose and its
        # buttons carry NO resource-id and NO text of their own -- the visible
        # label is a separate, non-clickable CHILD node. A parser that reads
        # only each node's own text finds nothing to tap on the one screen it
        # exists for, which is exactly what the first version did.
        labels = _uia_labels(node)
        cand = {"resource_id": a.get("resource-id"),
                "text": (labels[0] if labels else ""),
                "center": center, "package": pkg, "area": _uia_area(a.get("bounds"))}
        # DECLINE IS DECIDED FIRST, and it has to be: NEVER_TAP_TEXT is
        # matched as a substring, and "allow" is a substring of "Don't allow".
        # Checking the ban first would refuse the very button that dismisses a
        # permission dialog and leave the flow stuck in front of it. Declines
        # are exact-match, so they cannot be widened by accident.
        decline = (any(f in rid for f in SYSTEM_DECLINE_ID_FRAGMENTS)
                   or any(l.strip().lower() in SYSTEM_DECLINE_TEXT
                          for l in labels))
        if not decline:
            # A control that changes the DEVICE is never a candidate, however
            # it would otherwise have matched.
            banned = next((l for l in labels
                           if any(b in l.strip().lower()
                                  for b in NEVER_TAP_TEXT)), None)
            if banned is not None:
                out.setdefault("refused", []).append(
                    {"text": banned, "center": center,
                     "why": "would change the device, not the wizard"})
                continue
        if decline:
            cand["matched"] = "system-decline:%s" % (rid or cand["text"])
            perms.append(cand)
        elif rid and any(f in rid for f in ONBOARDING_ID_FRAGMENTS):
            cand["matched"] = "onboarding-id:%s" % rid
            ids.append(cand)
        else:
            hit = next((l for l in labels
                        if l.strip().lower() in ONBOARDING_TEXT_HINTS), None)
            if hit is not None:
                cand["text"] = hit
                cand["matched"] = "text(en-only):%s" % hit.strip().lower()
                texts.append(cand)
    # SMALLEST MATCHING BOX FIRST. Descendant labels mean a big container that
    # happens to enclose the button also "matches", and tapping its centre
    # lands wherever the middle of the screen happens to be. The tight box
    # around the control is the control.
    texts.sort(key=lambda c: c["area"])
    out["packages"] = sorted(seen_packages)
    # A system permission dialog is in FRONT of everything, so it goes first;
    # then ids, which are identity; then the English text guess, last.
    if not out["reachable"]:
        out["targets"] = perms + ids + texts
    return out


def android_dismiss_onboarding(package, serial=None, max_rounds=10,
                               _dump=None, _tap=None, dump_sink=None):
    """Tap through Fenix's first-run wizard until the browser is reachable.

    Bounded, verified and LOUD. It loops because the wizard is several screens
    whose order changes between builds; it verifies arrival against the
    browser's own toolbar rather than assuming a tap worked because `input
    tap` exited 0; and when it cannot converge it RAISES, because the
    alternative is navigating into a wizard and reporting a row for an app
    that never ran.

    `_dump`/`_tap` are injection seams for the selftest -- the loop's control
    flow is the part that has to be right, and it is exercised offline."""
    dump = _dump or (lambda: _adb_uia_dump(package, serial))
    tap = _tap or (lambda x, y: _adb_tap(x, y, serial))
    trail = []
    for round_ in range(max_rounds):
        raw = dump()
        # The failure mode here is "the wizard changed and the rig no longer
        # recognises it", and that is unreadable without the hierarchy that
        # was actually on screen. Kept for every round, written out by the
        # caller when this raises.
        if dump_sink is not None:
            dump_sink.append(raw)
        state = android_parse_hierarchy(raw, package)
        if state.get("reachable"):
            return {"ok": True, "rounds": round_, "taps": trail,
                    "note": "browser reachable (toolbar/content view on screen)"}
        targets = state.get("targets") or []
        if not targets:
            raise WebDriverError(
                "onboarding dismissal did not converge after %d round(s): the "
                "UI hierarchy has NO onboarding control to act on and NO "
                "browser toolbar either. Foreground packages seen: %s; %d "
                "nodes; %s. Refusing to navigate -- a leg that starts inside "
                "a wizard measures the app not running."
                % (round_, state.get("packages"), state.get("nodes"),
                   state.get("error") or "hierarchy parsed"))
        t = targets[0]
        tap(*t["center"])
        trail.append({"round": round_, "matched": t["matched"],
                      "text": t["text"], "center": list(t["center"])})
        time.sleep(1.2)          # let the wizard advance before re-reading
    raise WebDriverError(
        "onboarding dismissal hit its %d-round bound without the browser "
        "becoming reachable; taps so far: %s"
        % (max_rounds, json.dumps(trail)))


# The OS half of the same question. Gecko can auto-allow a site all it likes;
# if Android has not granted the APP the runtime permission there is no
# position to hand over.
ANDROID_LOCATION_PERMISSIONS = (
    "android.permission.ACCESS_FINE_LOCATION",
    "android.permission.ACCESS_COARSE_LOCATION",
)


def android_grant_location(package, serial=None):
    """Grant the measurement browser the OS location permission.

    A WRITE to the device, and the only one this rig performs, so it is
    fenced: REFUSED outright for any daily-driver package. On
    org.mozilla.firefox_beta it is a package that exists solely for
    measurement and whose data geckodriver wipes on every launch anyway, so
    the grant does not survive to affect anything the owner uses.

    Returns what was granted; never raises on a per-permission failure, because
    a device that refuses one of these is a denominator, not a crash."""
    if package in ANDROID_DAILY_DRIVER_PACKAGES:
        raise WebDriverError(
            "REFUSING to grant location to %s: that is somebody's daily "
            "browser and this rig does not change permissions on it" % package)
    out = {"package": package, "granted": [], "failed": {}}
    for perm in ANDROID_LOCATION_PERMISSIONS:
        try:
            r = _adb(["shell", "pm", "grant", package, perm], serial)
        except Exception as e:                    # noqa: BLE001 - never fatal
            out["failed"][perm] = str(e)[:120]
            continue
        if r.returncode == 0 and not (r.stderr or "").strip():
            out["granted"].append(perm)
        else:
            out["failed"][perm] = ((r.stderr or r.stdout).strip() or
                                   "rc=%d" % r.returncode)[:120]
    return out


# Read back from the PAGE, which is the only place that knows what the app
# actually got. `permissions.query` reports Gecko's answer; the position probe
# reports whether anything is really available, which is the OS half.
GEO_PROBE = """
var done = arguments[arguments.length - 1];
var out = {permission: null, position: null, error: null};
var finish = function () { done(out); };
var probe = function () {
  if (!navigator.geolocation) { out.error = 'no navigator.geolocation'; return finish(); }
  var settled = false;
  var t = setTimeout(function () {
    if (!settled) { settled = true; out.error = 'timeout'; finish(); }
  }, 8000);
  navigator.geolocation.getCurrentPosition(
    function (p) {
      if (settled) return; settled = true; clearTimeout(t);
      out.position = {lat: p.coords.latitude, lon: p.coords.longitude,
                      accuracy: p.coords.accuracy};
      finish();
    },
    function (e) {
      if (settled) return; settled = true; clearTimeout(t);
      out.error = 'code ' + e.code + ': ' + e.message;
      finish();
    }, {timeout: 7000, maximumAge: 0});
};
if (navigator.permissions && navigator.permissions.query) {
  navigator.permissions.query({name: 'geolocation'}).then(function (s) {
    out.permission = s.state; probe();
  }).catch(function () { probe(); });
} else { probe(); }
"""


def android_find_loaded_tab(xml_text, url_hint=None):
    """Find the home screen's recent-tab card for the page we just loaded.

    Returns {"center", "matched", "url_verified", "url_text"} or None.

    Identity, not position, and identity of the RIGHT tab: Fenix renders the
    tab's own URL into `recent.tab.url`, so when the served host:port appears
    there this is provably the card for our page and not somebody's last
    browsing session. A card that cannot be verified that way is still
    returned -- it is the only recent tab and the wizard wiped everything else
    -- but `url_verified` says which happened, and the row carries it.

    `clickable` is deliberately NOT consulted. These are Compose semantics
    nodes: tappable, and not one of them sets the attribute."""
    try:
        root = ET.fromstring(xml_text or "")
    except ET.ParseError:
        return None
    hint = (url_hint or "").strip()
    best = None
    for node in root.iter("node"):
        a = node.attrib
        rid = _uia_id_suffix(a.get("resource-id"))
        if not any(rid == f or rid.startswith(f)
                   for f in RECENT_TAB_ID_FRAGMENTS):
            continue
        center = _uia_bounds_center(a.get("bounds"))
        if center is None:
            continue
        text = (a.get("text") or a.get("content-desc") or "").strip()
        verified = bool(hint and hint in text)
        cand = {"center": center, "matched": "recent-tab-id:%s" % rid,
                "url_verified": verified, "url_text": text,
                "area": _uia_area(a.get("bounds"))}
        # A URL-verified node wins outright; otherwise the smallest box, which
        # is the title or url line rather than the whole card container.
        if best is None:
            best = cand
        elif verified and not best["url_verified"]:
            best = cand
        elif verified == best["url_verified"] and cand["area"] < best["area"]:
            best = cand
    return best


def android_surface_loaded_tab(url, serial=None, max_rounds=4,
                               _dump=None, _tap=None, dump_sink=None):
    """Bring the loaded tab to the front -- the step a human was doing by hand.

    The user described it exactly: "I always had to go through it then
    navigate to the 'jump back in' on the home page before the test would
    start". This is that tap, by identity.

    Raises when it cannot find the card, because the alternative is measuring
    a page that was never on screen."""
    dump = _dump or (lambda: _adb_uia_dump(None, serial))
    tap = _tap or (lambda x, y: _adb_tap(x, y, serial))
    parts = urllib.parse.urlsplit(url or "")
    hint = parts.netloc or ""
    trail = []
    for round_ in range(max_rounds):
        raw = dump()
        if dump_sink is not None:
            dump_sink.append(raw)
        found = android_find_loaded_tab(raw, hint)
        if found is None:
            raise WebDriverError(
                "the page is loaded but not shown, and the home screen has no "
                "recent-tab card to bring it forward (round %d, hint %r). "
                "Refusing to report figures for a viewport that was never "
                "displayed." % (round_, hint))
        tap(*found["center"])
        trail.append({"round": round_, "matched": found["matched"],
                      "url_verified": found["url_verified"],
                      "center": list(found["center"])})
        return {"ok": True, "rounds": round_ + 1, "taps": trail,
                "url_verified": found["url_verified"],
                "url_text": found["url_text"]}
    raise WebDriverError("surfacing the loaded tab hit its %d-round bound"
                         % max_rounds)


def verify_android_content_view(session, timeout=30.0, probe=None, soft=False):
    """After navigate: the page must actually be ON SCREEN, not merely loaded.

    MEASURED on Fenix 155, and it is the failure this exists for: with the
    wizard dismissed, geckodriver navigates and the page BOOTS -- Marionette
    reaches it, JS runs, `crossOriginIsolated` is true, the rayon pool comes
    up with 8 threads -- while Fenix keeps `homepageView` on top and never
    lays out the GeckoView. The canvas stays at the HTML default 300x150
    buffer and 0x0 css, screenshots fail with "Unable to capture screenshot",
    and every frame figure the leg would print describes a viewport that was
    never displayed.

    Nothing weaker sees it. `booted` is true, the worker wire is up, the
    adapter is real hardware. Only the canvas having a SIZE distinguishes a
    page that rendered from a page that merely ran, so that is what is
    asserted -- loudly, because the alternative is a full row of plausible
    numbers for a window nobody was shown."""
    read = probe or (lambda: session.execute(BOOT_PROBE))
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        last = read() or {}
        if (last.get("clientWidth", 0) > 0 and last.get("clientHeight", 0) > 0
                and last.get("bufferWidth", 0) > 300):
            return {"ok": True, "waited_s": round(
                timeout - (deadline - time.monotonic()), 2),
                "client": [last.get("clientWidth"), last.get("clientHeight")],
                "buffer": [last.get("bufferWidth"), last.get("bufferHeight")]}
        time.sleep(0.5)
    if soft:
        # The caller has a repair to try (surfacing the tab). It gets one
        # chance, and then this same check runs again WITHOUT soft -- the
        # verdict is never softened, only deferred.
        return None
    raise WebDriverError(
        "the page loaded but was never DISPLAYED: after %.0fs the canvas is "
        "still %sx%s css / %sx%s buffer. On Fenix this is the browser holding "
        "its home screen in front of the content view -- the app runs, the "
        "worker pool comes up and nothing else in this rig can tell. Refusing "
        "to report frame figures for a viewport that was never on screen."
        % (timeout, (last or {}).get("clientWidth"),
           (last or {}).get("clientHeight"), (last or {}).get("bufferWidth"),
           (last or {}).get("bufferHeight")))


def _adb(argv, serial=None, timeout=30):
    adb = shutil.which("adb")
    if not adb:
        raise WebDriverError("--android needs adb on PATH")
    full = [adb] + (["-s", serial] if serial else []) + argv
    return subprocess.run(full, capture_output=True, text=True, timeout=timeout)


def _adb_uia_dump(package, serial=None):
    """One `uiautomator dump`, read back and then REMOVED -- the rig cleans up
    what it creates and nothing else."""
    remote = "/data/local/tmp/squallar-rig-uia.xml"
    r = _adb(["shell", "uiautomator", "dump", remote], serial, timeout=60)
    if r.returncode != 0:
        raise WebDriverError("uiautomator dump failed rc=%d: %s"
                             % (r.returncode, (r.stderr or r.stdout)[:300]))
    try:
        cat = _adb(["shell", "cat", remote], serial, timeout=60)
        if cat.returncode != 0:
            raise WebDriverError("could not read the uiautomator dump: %s"
                                 % (cat.stderr or cat.stdout)[:300])
        return cat.stdout
    finally:
        _adb(["shell", "rm", "-f", remote], serial, timeout=30)


def _adb_tap(x, y, serial=None):
    r = _adb(["shell", "input", "tap", str(int(x)), str(int(y))], serial)
    if r.returncode != 0:
        raise WebDriverError("input tap failed rc=%d: %s"
                             % (r.returncode, (r.stderr or r.stdout)[:200]))


def android_device_state(serial=None):
    """Battery level and temperature, READ ONLY, at a moment in time.

    A phone is a thermally throttled device with a battery, and neither of
    those is constant across a three-minute leg. A row taken at 26.9 C and one
    taken at 32.3 C are not obviously the same measurement, so both ends of
    every leg are recorded and the DELTA travels with the figures -- which is
    also the only way to tell a thermal explanation apart from a real one
    (the Blink lane refuted its own thermal hypothesis by reproducing an early
    reading at its highest temperature, and it could only do that because it
    had both ends).

    `dumpsys battery` and nothing else. This rig is a GUEST on a personal
    phone: it reads, it never clears, force-stops, uninstalls or re-provisions
    anything, and it does not tidy up after itself beyond the `adb reverse`
    mappings it created. Never add a command here that writes.

    Returns a dict, or {"error": ...} -- never raises. A missing reading is a
    missing column, not a failed leg.
    """
    adb = shutil.which("adb")
    if not adb:
        return {"error": "no adb on PATH"}
    argv = [adb] + (["-s", serial] if serial else []) + \
           ["shell", "dumpsys", "battery"]
    try:
        r = subprocess.run(argv, capture_output=True, text=True, timeout=20)
    except Exception as e:                        # noqa: BLE001 - never fatal
        return {"error": "adb dumpsys battery failed: %s" % e}
    if r.returncode != 0:
        return {"error": "adb dumpsys battery rc=%d: %s"
                         % (r.returncode, (r.stderr or r.stdout).strip()[:200])}
    out = {}
    for line in (r.stdout or "").splitlines():
        k, _, v = line.strip().partition(":")
        k = k.strip()
        v = v.strip()
        if k == "level":
            out["battery_percent"] = int(v) if v.lstrip("-").isdigit() else v
        elif k == "temperature":
            # deci-degrees Celsius, which is the unit `dumpsys battery` uses
            # and the unit every reading here is quoted in after dividing.
            out["battery_temp_c"] = (round(int(v) / 10.0, 1)
                                     if v.lstrip("-").isdigit() else v)
        elif k in ("status", "health", "plugged"):
            out[k] = int(v) if v.lstrip("-").isdigit() else v
    return out or {"error": "dumpsys battery returned nothing parseable"}


# --------------------------------------------------------------------------
# Page probes
# --------------------------------------------------------------------------

BOOT_PROBE = """
var s = document.getElementById('squallar-status');
var c = document.getElementById('squallar-canvas');
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
        // The spec's word for a software adapter (SwiftShader; lavapipe, which
        // Dawn types as a CPU adapter). Read off `info`, where the spec keeps
        // it; the older `GPUAdapter.isFallbackAdapter` is deprecated and logs
        // on every read. null when the browser does not say.
        out.adapter_is_fallback = (typeof info.isFallbackAdapter === 'boolean')
                                    ? info.isFallbackAdapter : null;
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

# Did THIS BROWSER really apply the scene seed, or is it measuring a page that
# chose its own scene?
#
# THE HOLE THIS CLOSES, and both halves of it happened. `rig_seed_tests` and
# `measure_seed_tests` prove a seed literal parses into the scene its script
# claims -- on the HOST, against the config loader. Neither says a browser ever
# read it, and nothing else did either:
#
#   * A leg navigated to /index.html instead of /index-rig.html. serve.py only
#     injects PAGE_PRELUDE on the -rig route, so there was no window.__rig, no
#     localStorage write, and the app opened on a site derived from the
#     machine's timezone. Every figure was against a scene nobody chose and
#     the row read as valid.
#   * A seed that parses but is REFUSED at load -- an unknown site, a body the
#     serde chain rejects -- falls through the same way, with a warning nobody
#     was reading.
#
# Four fields, and the first two are the floor that stops the last two passing
# vacuously. "No fallback line was logged" is an ABSENCE, and an absence is
# satisfied for free by a page that logged nothing at all -- which is precisely
# what the /index.html leg looked like, since without the prelude there is no
# console ring to log into.
#
#   seeded     -- the keys PAGE_PRELUDE actually wrote into localStorage
#                 (serve.py sets window.__rig.seeded). Null means no prelude
#                 ran: wrong page, or no --seed-local-storage.
#   loud       -- a `loop state:` line was seen. report_frame_telemetry writes
#                 it every period UNCONDITIONALLY when squallar.frame_telemetry
#                 is loud, and that key comes from the same seed object as
#                 squallar.ui. So this is a POSITIVE proof that the app read
#                 this seed, not merely that the prelude wrote it, and it is
#                 what makes the two absences below mean something.
#   fallback   -- "opening on <site>, nearest to timezone <zone>" (app.rs and
#                 app_render.rs, both log::info!). Reachable ONLY where
#                 gui.load_ui_config(store) returned false.
#   refused    -- "Failed to parse config" (ui_config.rs) or "config names a
#                 site no radar could be called" -- a seed that arrived and was
#                 rejected in whole or in part.
SEED_APPLIED_PROBE = r"""
var C = window.__rig_console || [];
var fallback = [], refused = [], loud = 0;
for (var i = 0; i < C.length; i++) {
  var m = String(C[i].msg || "");
  if (m.indexOf("nearest to timezone") !== -1) fallback.push(m);
  if (m.indexOf("Failed to parse config") !== -1) refused.push(m);
  if (m.indexOf("no radar could be called") !== -1) refused.push(m);
  if (m.indexOf("loop state:") !== -1) loud++;
}
return {
  seeded: (window.__rig && window.__rig.seeded) || null,
  loud: loud,
  console_total: C.length,
  fallback: fallback.slice(0, 4),
  refused: refused.slice(0, 4)
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
var by_kind = {};
var transport = null;
var off_re = /([A-Za-z0-9_-]+) took (\d+) ms off the frame/;
var rayon_re = /rayon: (\d+) threads/;
// The LAST match wins, not the first: `worker_port::account` logs RUNNING
// TOTALS, so the newest line is the whole answer and an older one is a prefix
// of it. Scanning forward and overwriting is what makes that true.
var transport_re = /transport: (\d+) replies, (\d+) B out with (\d+) B copied out of the worker, (\d+) B in with (\d+) B copied out of this page, (\d+) us encoding, (\d+) us posting/;
// The two raster-telemetry lines, written once a frame by
// `App::report_raster_telemetry` and only on a frame where something moved.
// Running totals, so the LAST match wins here for the same reason it does for
// `transport` above.
//
// They are kept SEPARATE and are never added, because their denominators are
// different: `rasters` is the whole-picture overlay dispatch only, `uploads` is
// every texture delta the renderer was shown -- font atlas, basemap tiles and
// radar included. A single "bytes uploaded" figure over the union would
// describe neither, and would move when the mix moved.
// `inked` is a SUBSET OF `pictures`, never a term beside it and never added to
// anything: it is how many of those pictures had a single non-zero byte in
// them. `pictures` counts buffers handed to egui whatever is in them, so a
// layer that rasterizes a fully transparent pixmap moves `pictures` and
// `picture_bytes` exactly as a working one does -- measured 2026-08-31, six
// dispatched / six arrived / six pictures over a map drawing nothing.
var rasters_re = /overlay rasters: (\d+) dispatched, (\d+) arrived, (\d+) pictures of (\d+) B, (\d+) inked, (\d+) shown, (\d+) promoted, (\d+) dropped, (\d+) superseded, (\d+) cancelled/;
// `whole` is a routing subset of `blocking` (a whole delta moves through a
// blocking write_texture on the frame's own queue); the GPU total is the
// disjoint pair staged + blocking. Never add whole to anything.
var uploads_re = /texture uploads: (\d+) deltas, (\d+) B to the GPU, (\d+) B whole, (\d+) bands, (\d+) B staged, (\d+) B blocking/;
// A THIRD denominator, and it is added to neither of the two above. These
// count archive tile BODIES DECODED, split by the archive header's declared
// tile_type: `vector` is the self-hosted basemap's MVT, `raster` the terrain
// hillshade, `sniffed` an archive that declared nothing (no archive this app
// opens does, so non-zero there is the finding). A vector decode uploads no
// texture at all and so appears in neither raster figure; a raster decode is
// one egui texture and is therefore INSIDE `uploads`, a subset of it and never
// a term to add to it. Running totals, so the LAST match wins here too.
var basemap_re = /basemap tiles: (\d+) vector, (\d+) raster, (\d+) sniffed/;
// A FOURTH, and it is not the third restated: `basemap` counts tile bodies
// DECODED, this counts what the ground phase then PLACED on the frame thread.
// The two come apart exactly where it matters -- a leg can decode tiles and
// place nothing -- and together they are what makes a row comparable across
// the pmtiles 32-bit truncation fix (`4882611e`, 2026-08-31 -- resolve it by
// message, not by hash: the pre-rebase twin `a7465238` carries the same
// subject and date and is not on main), before which no
// vector tile resolved on wasm32 at all while every native leg did the whole
// placement, tessellation and upload. `placed` alone is not the test, and
// since 2026-09-01 neither is `stroke pts`: the fills AND the strokes go to
// the GPU, so a drawing pane reads 0 in both and positive in labels, draws
// and uploads. The pattern is deliberately not anchored at the end, so the
// `stroke draws` field appended after `unrendered` leaves every capture
// index below where it was.
var ground_re = /ground tiles: (\d+) placed, (\d+) stroke pts, (\d+) labels, (\d+) draws, (\d+) uploads of (\d+) B, (\d+) evicted, (\d+) B resident, (\d+) unrendered/;
// `stroke draws` is its OWN pattern and NOT an optional group on the line
// above, and the reason is `native_row.py`: it reads these patterns out of
// this file at run time (deliberately, so the two cannot drift) and `int()`s
// EVERY group of a match. A non-participating optional group hands it `None`
// and it dies with `int() argument ... not 'NoneType'` -- on the OLD line
// only, so the arm running the old binary is the one that appears broken. A
// separate pattern keeps every group of every shared probe mandatory.
var ground_stroke_draws_re = /ground tiles: .*, (\d+) stroke draws/;
// A SIXTH denominator, and the one the fourth cannot supply: events AT THE
// TILE CACHE, per cache role (`base` = the basemap sources, `terrain` = the
// hillshade), where a first sight, a refetch, a restyle and a duplicate are
// different events. `ground_re`'s `uploads`/`evicted` are the GPU store's,
// keyed on an identity minted per mesh, so they cannot say whether an upload
// was a tile's first sight or the same tile fetched again after the LRU
// dropped it -- which is the whole question behind "3,070 uploads against
// 2,848 evictions". `refetch after eviction` is the SUBSET of `asks` whose id
// the cache remembers evicting; `duplicate` and `orphan` are the two shapes
// of a body fetched for nothing. `entries`, `B resident` and `parsed` are
// LEVELS and go down; the `B` figures are the lower bound a slot can price.
// Never subtracted from `uploads`: a put is a cache slot, an upload is a mesh
// buffer write, and a put with no fills uploads nothing. Its OWN pattern with
// every group mandatory, for `ground_stroke_draws_re`'s reason: `native_row.py`
// reads these literals at run time and `int()`s every group of the shared
// probes; the role group is a WORD, so that file gives this its own arm, as
// it does `budget_state_re`. Running totals: the LAST match per role wins for
// the headline reading; every match is kept for the settle assertion.
var tile_cache_re = /tile cache \(([a-z0-9-]+)\): (\d+) asks, (\d+) restyle asks, (\d+) refetch after eviction, (\d+) puts first, (\d+) restyle, (\d+) duplicate, (\d+) orphan, (\d+) evicted pending, (\d+) evicted resident of (\d+) B, (\d+) entries, (\d+) B resident, (\d+) parsed, snap (\d+)/;
// A FIFTH, and the only one about the 3D floor path. `paints` is per 3D pane
// per frame its off-screen map strip really drew, `mirror renders` per mirror
// pass encoded (per frame, not per pane) -- two denominators, never added, and
// neither is a term of any figure above. The last three say WHY a paint
// happened and overlap both each other and `paints`: `key moves` is the
// repaint rate the floor's own content asked for, so a `paints` figure far
// above it is a floor repainting for a reason that is not its content, and
// `incomplete` tracking `paints` is a completeness latch stuck open rather
// than a key that is moving. Running totals, so the LAST match wins.
var floor_re = /floor strips: (\d+) paints, (\d+) mirror renders, (\d+) key moves, (\d+) on a stable key, (\d+) incomplete/;
// THE SKEW READING, and it is what makes a red on the three lines above
// mean anything at all. Each strict pattern above names a FIELD LIST, and
// that list is a property of the BUNDLE BEING DRIVEN rather than of this
// file: `inked` joined the raster line on 2026-08-31 (79bad6c7). Point this
// rig at a bundle built before that commit and the strict pattern does not
// match, the reading stays null, and null is indistinguishable from "the
// overlay path never ran" -- so the leg reddens blaming the renderer for
// what is a stale `wasm-pack build`.
//
// MEASURED 2026-08-31, this rig against a bundle built at 79bad6c7^: both
// expect attempts burned the full 180 s timeout (411 s for the leg against
// 28-31 s green) and the summary printed "no overlay raster ever moved"
// while the app was writing the line every frame with every counter moving.
// Two careful lanes read that as a regression in the renderer.
//
// The loose patterns match the SENTENCE without its field list. A loose
// match where the strict one failed is PROOF of skew rather than a guess
// at it: the app wrote this telemetry line, and this rig cannot read the
// shape it wrote. The question in front of the reader is then a BUILD
// question, not a rendering one, and the error says which.
var rasters_loose_re = /overlay rasters: \d+ dispatched/;
var uploads_loose_re = /texture uploads: \d+ deltas/;
var basemap_loose_re = /basemap tiles: \d+ vector/;
var rasters_unparsed = null, uploads_unparsed = null, basemap_unparsed = null;
var rasters = null, uploads = null, basemap = null, ground = null, floor = null;
// The settle assertion's history: every match, page-stamped, so a window can
// be differenced. `tile_cache` is the headline reading, last match PER ROLE.
var tile_cache = null, tile_cache_all = [], ground_all = [], basemap_all = [];
for (var i = 0; i < C.length; i++) {
  var m = String(C[i].msg || "");
  if (m.indexOf("rasterization worker attached") !== -1) attached.push(C[i].t);
  if (m.indexOf("rasterization worker is a different build") !== -1)
    different.push(C[i].t);
  var om = off_re.exec(m);
  if (om) {
    off_frame.push(C[i].t);
    // Kept PER KIND. `deliver_job_reply` logs this line for every offloaded
    // job -- alerts, discussions and zone work as well as radar decode and
    // render -- and those are different workloads with different costs. One
    // median over the union would be a number describing no job in
    // particular, and would move when the mix moved.
    var k = om[1];
    (by_kind[k] = by_kind[k] || []).push(parseInt(om[2], 10));
  }
  var rm = rayon_re.exec(m);
  if (rm) rayon.push(parseInt(rm[1], 10));
  var tm = transport_re.exec(m);
  if (tm) transport = { replies: parseInt(tm[1], 10),
                        out_moved: parseInt(tm[2], 10),
                        out_copied: parseInt(tm[3], 10),
                        in_moved: parseInt(tm[4], 10),
                        in_copied: parseInt(tm[5], 10),
                        // The two halves of the overlay dispatch's hand-off,
                        // cumulative us on the FRAME THREAD. Never added to
                        // each other's bytes and never to any frame segment:
                        // these are a SUBSET of `frame dispatch (offload)`,
                        // which is itself a cut of `frame post (dispatch)`.
                        encode_us: parseInt(tm[6], 10),
                        post_us: parseInt(tm[7], 10) };
  var rm2 = rasters_re.exec(m);
  if (rm2) rasters = { dispatched: parseInt(rm2[1], 10),
                       arrived: parseInt(rm2[2], 10),
                       pictures: parseInt(rm2[3], 10),
                       picture_bytes: parseInt(rm2[4], 10),
                       inked: parseInt(rm2[5], 10),
                       shown: parseInt(rm2[6], 10),
                       promoted: parseInt(rm2[7], 10),
                       dropped: parseInt(rm2[8], 10),
                       superseded: parseInt(rm2[9], 10),
                       cancelled: parseInt(rm2[10], 10) };
  else if (rasters_loose_re.test(m)) rasters_unparsed = m;
  var um = uploads_re.exec(m);
  if (um) uploads = { deltas: parseInt(um[1], 10),
                      bytes: parseInt(um[2], 10),
                      whole_bytes: parseInt(um[3], 10),
                      bands: parseInt(um[4], 10),
                      staged_bytes: parseInt(um[5], 10),
                      blocking_bytes: parseInt(um[6], 10) };
  else if (uploads_loose_re.test(m)) uploads_unparsed = m;
  var bm = basemap_re.exec(m);
  if (bm) basemap = { vector_tiles: parseInt(bm[1], 10),
                      raster_tiles: parseInt(bm[2], 10),
                      sniffed_tiles: parseInt(bm[3], 10) };
  else if (basemap_loose_re.test(m)) basemap_unparsed = m;
  var gm = ground_re.exec(m);
  // `stroke_draws` is an OPTIONAL group: a bundle built before 2026-09-01
  // does not print the field, and this rig must still read the eight that
  // predate it rather than reporting the whole line unparsed. `null` here
  // therefore means "that bundle is older", not "zero stroke runs drew" --
  // the two are different findings and a caller must not conflate them.
  if (gm) ground = { placed: parseInt(gm[1], 10),
                     stroke_points: parseInt(gm[2], 10),
                     labels: parseInt(gm[3], 10),
                     draws: parseInt(gm[4], 10),
                     uploads: parseInt(gm[5], 10),
                     upload_bytes: parseInt(gm[6], 10),
                     resident_bytes: parseInt(gm[8], 10),
                     unrendered: parseInt(gm[9], 10),
                     stroke_draws: null };
  // Filled from its own pattern; absent on a bundle built before 2026-09-01,
  // where `null` means "that bundle is older" and NOT "zero stroke runs drew".
  if (gm) { var sd = ground_stroke_draws_re.exec(m);
            if (sd) ground.stroke_draws = parseInt(sd[1], 10); }
  // Kept for the settle assertion: the two GPU-store figures with their
  // page stamp, and the one decode figure with its.
  if (gm) ground_all.push({ t: C[i].t, uploads: parseInt(gm[5], 10),
                            evicted: parseInt(gm[7], 10) });
  if (bm) basemap_all.push({ t: C[i].t, vector_tiles: parseInt(bm[1], 10) });
  var tcm = tile_cache_re.exec(m);
  if (tcm) {
    var tc = { t: C[i].t, role: tcm[1],
               asks: parseInt(tcm[2], 10),
               restyle_asks: parseInt(tcm[3], 10),
               refetch_after_eviction: parseInt(tcm[4], 10),
               puts_first: parseInt(tcm[5], 10),
               puts_restyle: parseInt(tcm[6], 10),
               puts_duplicate: parseInt(tcm[7], 10),
               puts_orphan: parseInt(tcm[8], 10),
               evicted_pending: parseInt(tcm[9], 10),
               evicted_resident: parseInt(tcm[10], 10),
               evicted_bytes: parseInt(tcm[11], 10),
               resident_entries: parseInt(tcm[12], 10),
               resident_bytes: parseInt(tcm[13], 10),
               parsed_entries: parseInt(tcm[14], 10) };
    if (!tile_cache) tile_cache = {};
    tile_cache[tcm[1]] = tc;
    tile_cache_all.push(tc);
  }
  var fm = floor_re.exec(m);
  if (fm) floor = { paints: parseInt(fm[1], 10),
                    mirror_renders: parseInt(fm[2], 10),
                    key_moves: parseInt(fm[3], 10),
                    paints_on_stable_key: parseInt(fm[4], 10),
                    incomplete_paints: parseInt(fm[5], 10) };
}
return { attached: attached, different: different, off_frame: off_frame,
         off_frame_by_kind: by_kind, rayon_threads: rayon,
         transport: transport, rasters: rasters, uploads: uploads,
         basemap: basemap, ground: ground, floor: floor,
         tile_cache: tile_cache, tile_cache_all: tile_cache_all,
         ground_all: ground_all, basemap_all: basemap_all,
         rasters_unparsed: rasters_unparsed,
         uploads_unparsed: uploads_unparsed,
         basemap_unparsed: basemap_unparsed,
         console_total: C.length };
"""

# The frame timing lines, written by App::report_frame_telemetry every 2 s
# when the `squallar.frame_telemetry` localStorage key is seeded (a separate
# switch from `squallar.raster_telemetry`; a leg seeds only what it reads).
# All running totals, so the LAST match wins for the headline reading -- but
# the interact and cadence lines embed their whole 42-count histograms
# (under-floor clamp, 40 geometric bins, over-ceiling clamp) precisely so a
# reading can be BIN-DIFFED against an earlier one: percentiles do not
# difference, histograms do. Every reading of those two families is therefore
# kept, with its page-side timestamp, for the gesture-window diff.
#
# Sentences pinned from the Rust side by `frame_telemetry_line_tests`, which
# reads these very patterns out of this file -- the same seam
# `raster_telemetry_line_tests` holds for the two raster lines. The absence
# sentence is scanned VERBATIM: on an adapter with no TIMESTAMP_QUERY (every
# WebGL2 leg) the app states the absence rather than extrapolating, and the
# rig reports it as such rather than as null.
#
# NO figure scraped here ever gates CI; the only gate built on these lines is
# --expect-interaction-frames, a count assert.
FRAME_LINE_PROBE = r"""
var C = window.__rig_console || [];
var svc_interact_re = /frame service \(interact\): n=(\d+), p50=(\d+|none|over) us, p90=(\d+|none|over) us, p99=(\d+|none|over) us, hist=([0-9,]+)/;
var svc_idle_re = /frame service \(idle\): n=(\d+), p50=(\d+|none|over) us, p90=(\d+|none|over) us, p99=(\d+|none|over) us, hist=([0-9,]+)/;
var segments_re = /frame segments \(interact, p99 us\): pre=(\d+|none|over), pump=(\d+|none|over), ui=(\d+|none|over), prepare=(\d+|none|over), finish=(\d+|none|over), post=(\d+|none|over); acquire n=(\d+), p50=(\d+|none|over) us, p99=(\d+|none|over) us/;
var prep_costs_re = /frame prep costs: (\d+) passes, (\d+) us tessellate, (\d+) us upload apply, (\d+) us mirror, (\d+) us buffers and callbacks/;
// The byte side of `prep_costs_re`'s `buffers` phase, on its OWN
// denominator: `stagings` is every `update_buffers` call, which a pass
// rendering a pane mirror makes twice, so it is never `prep.passes`.
// `bytes_staged` over the same reading's `buffers_and_callbacks_us` PLUS its
// `mirror_us` is the staging copy's effective bandwidth -- the mirror stages
// on its own clock, so dividing by the first term alone credits its bytes to
// the wrong phase. Both sides are running totals, so a windowed rate is a
// subtraction. Mirror off: exact. Mirror on: a lower bound, never an
// overstatement. `vertices`/`indices` are the staging
// identity -- the same picture staged by another route reads the same two.
var prep_geometry_re = /frame prep geometry: (\d+) stagings, (\d+) vertices, (\d+) indices, (\d+) B staged, (\d+) through the ring, (\d+) declined/;
var gpu_passes_re = /gpu passes: raymarch n=(\d+), p50=(\d+|none|over) us, p99=(\d+|none|over) us; ground n=(\d+), p50=(\d+|none|over) us, p99=(\d+|none|over) us; mirror n=(\d+), p50=(\d+|none|over) us, p99=(\d+|none|over) us; main n=(\d+), p50=(\d+|none|over) us, p99=(\d+|none|over) us; (\d+) frames/;
var cadence_re = /frame cadence: n=(\d+), p50=(\d+|none|over) us, p99=(\d+|none|over) us, hist=([0-9,]+)/;
// Scene E's denominators. `listed` is frame SLOTS across every animating
// layer of every pane; `resident`, `in flight` and `failed` are DISJOINT
// SUBSETS of it and are never added to it -- a slot may be none of the three.
// `allowed`/`cap`/`held` are ceilings on frames TEXTURED, a different
// denominator from slots. Bytes: `share` is one loop's slice, `pool` the
// application's whole allowance, and floor/ceiling the tier's bracket -- a
// pool below the value it booted at is `LoopPool::back_off` having fired.
var loop_state_re = /loop state: (\d+) panes, (\d+) layers animating, (\d+) frames listed, (\d+) resident, (\d+) in flight, (\d+) failed; allowed plan=(\d+) section=(\d+) volume=(\d+) overlay=(\d+), cap (\d+), held (\d+); share (\d+) B, pool (\d+) B, floor (\d+) B, ceiling (\d+) B; advance (\d+) us/;
// The `budget state:` line: the bracket and rung the budgets were resolved
// at, and every host signal the device profile carries beside them. A LEVEL,
// every group MANDATORY: the app prints every field on every tick, with 0
// where a signal is unread (0 is not a possible measurement of RAM, VRAM,
// threads or a live heap) and `form` 0/1/2 for unknown/handheld/desktop. A
// bundle built before the line never matches, and that reads as `null`
// below -- "that bundle is older", never "zero". Its OWN pattern rather than
// a group on any shared probe, for `ground_stroke_draws_re`'s reason. The
// bracket group is a WORD: `native_row.py` `int()`s every group of the
// probes in its running-totals loop, so this one has its own arm there.
// Denominators, never added: `pool` is the LIVE loop pool in MiB -- what the
// scene's loops need, capped by the room the rest of the scene leaves; the
// same figure `loop state:` prints in B -- and `ceiling` the whole-app
// texture ceiling, the bracket's constant; `vram`/`ram`/`declared` are
// three sources (measured VRAM, measured RAM, a browser's `deviceMemory`
// declaration) and never one figure; `linear` is the page instance's heap
// over the rasterization worker's -- two instances, two ceilings. `cap` is
// the capacity IN FORCE this session, in MiB -- the measured figure where the
// readings amount to one, the bracket's presumption where they do not, held
// to what pressure has taught the session -- and the integer after it is how
// it was learned: 0 presumed, 1 measured, 2 probed. `cap` is not `vram`: a
// unified-memory part's capacity is half its `ram` with `vram` 0, a software
// rasteriser's is the presumption with `vram` read. Firefox on WebGL2 reads
// `cap 288 0` on every leg; Chromium reads `cap N 2` once the probe lands.
// `probe` is where that WebGPU probe stands, carried HERE because its own
// lines are said once and this console ring evicts them within seconds: 0
// absent (native, or not asked yet), 1 skipped (a WebGL2 page -- every
// Firefox/Linux leg), 2 pending, 3 empty (ran, held nothing), 4 found at the
// device's refusal, 5 found at the probe's own bound (the figure is a floor).
// `balloon` is what the loops hold ABOVE their base, in MiB -- the bytes the
// pool's planner spent on density past what `fit` charged, summed over every
// loop; a SUBSET of `pool`, never added to it, and a real 0 when every loop
// holds its base or less. Last on the line, and mandatory like every other
// group: a bundle older than it matches nothing and reads `null`.
var budget_state_re = /budget state: bracket ([a-z0-9]+), rung (\d+), steps (\d+), pool (\d+) MiB, ceiling (\d+) MiB, vram (\d+) MiB, ram (\d+) MiB, declared (\d+) MiB, threads (\d+), form (\d+), linear (\d+)\/(\d+) MiB, cap (\d+) (\d+), probe (\d+), balloon (\d+) MiB/;
// The two WINDOWABLE families, both `<prefix> (<name>):` with the same
// payload. `n` and `sum` are running totals and both subtract, so a windowed
// mean is exact: (sum_b - sum_a) / (n_b - n_a). `hist` is the same 42-slot
// shape every other hist-bearing line carries.
//
// `frame segment (...)` is the per-segment spelling of `frame segments
// (interact, p99 us): ...`, which stays exactly as it was. The old line is
// p99 CUMULATIVE FROM BOOT with no bins, so no windowed per-segment figure
// was obtainable from it at all; these carry the bins that make one a
// subtraction. Same denominator as the old line -- interact frames only, six
// contiguous cuts of one frame's service, acquire NOT among them.
//
// `tile take (...)` is per TAKE -- one completion moved off a tile source's
// channel and handled to completion (decode/tessellate/put). Never per tile
// requested, per tile drawn, per frame or per pass, and never added to
// `basemap tiles` (decodes: excludes restyles and failures), to `overlay
// rasters`, to `texture uploads`, or to any frame segment. Several takes can
// share one `frame segment (pump)` sample.
var frame_segment_re = /frame segment \(([a-z0-9-]+)\): n=(\d+), sum=(\d+) us, p50=(\d+|none|over) us, p90=(\d+|none|over) us, p99=(\d+|none|over) us, hist=([0-9,]+)/;
// `frame segment (prepare)` opened up: six contiguous cuts of that ONE span,
// same denominator (presented interact frames), so their sums telescope to
// its sum. A DECOMPOSITION of the prepare segment, never a seventh segment --
// adding `frame prepare (*)` to `frame segment (prepare)` double-counts the
// whole of it. Also never added to `frame prep costs:`, which counts every
// pass ENDED (idle frames and non-presenting frames included) and therefore
// holds more samples than there are frames here.
var frame_prepare_re = /frame prepare \(([a-z0-9-]+)\): n=(\d+), sum=(\d+) us, p50=(\d+|none|over) us, p90=(\d+|none|over) us, p99=(\d+|none|over) us, hist=([0-9,]+)/;
// `frame segment (post)` opened up, on `frame prepare`'s terms exactly: six
// contiguous cuts of that ONE span, same denominator (presented interact
// frames), so their sums telescope to its sum. A DECOMPOSITION, never a
// seventh segment -- adding `frame post (*)` to `frame segment (post)`
// double-counts the whole of it.
//
// Why this family exists at all: `post` is not a per-frame cost. On the
// scene A Safari leg of 2026-09-01 it read under the 62.5 us histogram floor
// on 79% of interact frames and 8 ms at p99 over the same 475 frames. A
// percentile of a distribution that shape says an occasional event happened;
// it cannot say WHICH of the six things the tail does was the event.
var frame_post_re = /frame post \(([a-z0-9-]+)\): n=(\d+), sum=(\d+) us, p50=(\d+|none|over) us, p90=(\d+|none|over) us, p99=(\d+|none|over) us, hist=([0-9,]+)/;
// `frame post (dispatch)` opened up, one level below `frame post` on exactly
// its terms: six named cuts plus a residual, contiguous within that one span,
// same denominator, so their sums telescope to it. NEVER added to
// `frame post (*)` and never to `frame segment (post)` -- each is the one
// below it opened up, and adding any pair double-counts.
//
// `n` here is SMALLER than on `frame post (dispatch)` and that is a figure,
// not a loss: a frame whose tail dispatched nothing has no dispatch to
// decompose and contributes no sample, while the parent cut records a
// near-zero span on every interact frame. So this `n` is the count of
// DISPATCHING frames in the window.
//
// Why this family exists: on Firefox scene D, `dispatch` held 84% of `post`
// (27,728 of 33,043 us) on 19 of 176 frames. `post`'s split could say that
// and could not say WHICH of the six things the dispatch inlines it was.
// Read the answer off `sum`, never off a percentile: Hist is four bins per
// octave, so every percentile is quantized to a bin edge and any true ratio
// between 1.68x and 2.38x prints as exactly 2.00x, while `sum` is exact.
var frame_dispatch_re = /frame dispatch \(([a-z0-9-]+)\): n=(\d+), sum=(\d+) us, p50=(\d+|none|over) us, p90=(\d+|none|over) us, p99=(\d+|none|over) us, hist=([0-9,]+)/;
var tile_take_re = /tile take \(([a-z0-9-]+)\): n=(\d+), sum=(\d+) us, p50=(\d+|none|over) us, p90=(\d+|none|over) us, p99=(\d+|none|over) us, hist=([0-9,]+)/;
// One vector take opened up: `parse` (per source layer, at most sixteen) and
// `style` (per feature, thousands). A DECOMPOSITION of `tile take (vector)`,
// never a sixth take family and never added to one -- the two phases sum to a
// vector take minus its cache put. Denominator: one vector BODY decoded. A
// restyle records `style` and no `parse`, so the two n's differ by design.
var tile_phase_re = /tile phase \(([a-z0-9-]+)\): n=(\d+), sum=(\d+) us, p50=(\d+|none|over) us, p90=(\d+|none|over) us, p99=(\d+|none|over) us, hist=([0-9,]+)/;
var gesture_begin_re = /gesture script ([a-z0-9-]+) begin/;
var gesture_loop_re = /gesture script ([a-z0-9-]+) loop complete: (\d+) frames/;
var interact = null, idle = null, segments = null, prep = null, gpu = null;
var prep_geometry = null;
var cadence = null, gpu_unavailable = false, loop_state = null;
var loop_state_all = [];
var budget_state = null, budget_state_all = [];
var interact_all = [], idle_all = [], cadence_all = [];
var frame_segment_all = [], tile_take_all = [], tile_phase_all = [];
var frame_prepare_all = [], frame_post_all = [], frame_dispatch_all = [];
var begins = [], loops = [];
for (var i = 0; i < C.length; i++) {
  var m = String(C[i].msg || "");
  var t = C[i].t;
  var x = svc_interact_re.exec(m);
  if (x) {
    interact = { t: t, n: parseInt(x[1], 10), p50: x[2], p90: x[3],
                 p99: x[4], hist: x[5] };
    interact_all.push(interact);
  }
  x = svc_idle_re.exec(m);
  if (x) {
    idle = { t: t, n: parseInt(x[1], 10), p50: x[2], p90: x[3],
             p99: x[4], hist: x[5] };
    idle_all.push(idle);
  }
  x = segments_re.exec(m);
  if (x) segments = { t: t, pre: x[1], pump: x[2], ui: x[3], prepare: x[4],
                      finish: x[5], post: x[6],
                      acquire_n: parseInt(x[7], 10), acquire_p50: x[8],
                      acquire_p99: x[9] };
  x = prep_costs_re.exec(m);
  if (x) prep = { t: t, passes: parseInt(x[1], 10),
                  tessellate_us: parseInt(x[2], 10),
                  upload_apply_us: parseInt(x[3], 10),
                  mirror_us: parseInt(x[4], 10),
                  buffers_and_callbacks_us: parseInt(x[5], 10) };
  x = prep_geometry_re.exec(m);
  if (x) prep_geometry = { t: t, stagings: parseInt(x[1], 10),
                           vertices: parseInt(x[2], 10),
                           indices: parseInt(x[3], 10),
                           bytes_staged: parseInt(x[4], 10),
                           // Which route those bytes took. A DIFFERENT
                           // denominator from `stagings`: these two sum to the
                           // stagings that had a mesh to move, so they are
                           // never subtracted from it.
                           ring_staged: parseInt(x[5], 10),
                           ring_declined: parseInt(x[6], 10) };
  x = gpu_passes_re.exec(m);
  if (x) gpu = { t: t,
                 raymarch: { n: parseInt(x[1], 10), p50: x[2], p99: x[3] },
                 ground: { n: parseInt(x[4], 10), p50: x[5], p99: x[6] },
                 mirror: { n: parseInt(x[7], 10), p50: x[8], p99: x[9] },
                 main: { n: parseInt(x[10], 10), p50: x[11], p99: x[12] },
                 frames: parseInt(x[13], 10) };
  if (m.indexOf("gpu passes: unavailable (adapter lacks TIMESTAMP_QUERY)") !== -1)
    gpu_unavailable = true;
  x = cadence_re.exec(m);
  if (x) {
    cadence = { t: t, n: parseInt(x[1], 10), p50: x[2], p99: x[3],
                hist: x[4] };
    cadence_all.push(cadence);
  }
  x = loop_state_re.exec(m);
  if (x) {
    // A LEVEL, not a running total: every field is what the loops hold at
    // the moment of the reading, so the last one inside a window is the
    // reading, and there is nothing here to bin-diff.
    loop_state = { t: t, panes: parseInt(x[1], 10),
                   layers: parseInt(x[2], 10), listed: parseInt(x[3], 10),
                   resident: parseInt(x[4], 10),
                   in_flight: parseInt(x[5], 10), failed: parseInt(x[6], 10),
                   allowed_plan: parseInt(x[7], 10),
                   allowed_section: parseInt(x[8], 10),
                   allowed_volume: parseInt(x[9], 10),
                   allowed_overlay: parseInt(x[10], 10),
                   cap: parseInt(x[11], 10), held: parseInt(x[12], 10),
                   share_bytes: parseInt(x[13], 10),
                   pool_bytes: parseInt(x[14], 10),
                   floor_bytes: parseInt(x[15], 10),
                   ceiling_bytes: parseInt(x[16], 10),
                   advance_us: parseInt(x[17], 10) };
    loop_state_all.push(loop_state);
  }
  x = budget_state_re.exec(m);
  if (x) {
    // A LEVEL, like `loop_state`: the last reading inside a window is the
    // reading. Stays `null` when no line matched -- an older bundle, never a
    // zero reading.
    budget_state = { t: t, bracket: x[1], rung: parseInt(x[2], 10),
                     steps: parseInt(x[3], 10), pool_mib: parseInt(x[4], 10),
                     ceiling_mib: parseInt(x[5], 10),
                     vram_mib: parseInt(x[6], 10), ram_mib: parseInt(x[7], 10),
                     declared_mib: parseInt(x[8], 10),
                     threads: parseInt(x[9], 10), form: parseInt(x[10], 10),
                     linear_page_mib: parseInt(x[11], 10),
                     linear_worker_mib: parseInt(x[12], 10),
                     cap_mib: parseInt(x[13], 10),
                     cap_source: parseInt(x[14], 10),
                     probe: parseInt(x[15], 10),
                     balloon_mib: parseInt(x[16], 10) };
    budget_state_all.push(budget_state);
  }
  x = frame_segment_re.exec(m);
  if (x) frame_segment_all.push({ t: t, name: x[1], n: parseInt(x[2], 10),
                                  sum: parseInt(x[3], 10), p50: x[4],
                                  p90: x[5], p99: x[6], hist: x[7] });
  x = frame_prepare_re.exec(m);
  if (x) frame_prepare_all.push({ t: t, name: x[1], n: parseInt(x[2], 10),
                                  sum: parseInt(x[3], 10), p50: x[4],
                                  p90: x[5], p99: x[6], hist: x[7] });
  x = frame_post_re.exec(m);
  if (x) frame_post_all.push({ t: t, name: x[1], n: parseInt(x[2], 10),
                               sum: parseInt(x[3], 10), p50: x[4],
                               p90: x[5], p99: x[6], hist: x[7] });
  x = frame_dispatch_re.exec(m);
  if (x) frame_dispatch_all.push({ t: t, name: x[1], n: parseInt(x[2], 10),
                                   sum: parseInt(x[3], 10), p50: x[4],
                                   p90: x[5], p99: x[6], hist: x[7] });
  x = tile_take_re.exec(m);
  if (x) tile_take_all.push({ t: t, name: x[1], n: parseInt(x[2], 10),
                              sum: parseInt(x[3], 10), p50: x[4],
                              p90: x[5], p99: x[6], hist: x[7] });
  x = tile_phase_re.exec(m);
  if (x) tile_phase_all.push({ t: t, name: x[1], n: parseInt(x[2], 10),
                               sum: parseInt(x[3], 10), p50: x[4],
                               p90: x[5], p99: x[6], hist: x[7] });
}
// The markers come from `window.__rig_marks`, NOT from the console ring
// above. `C` keeps the last 1200 entries and the app logs frame telemetry
// every frame, so at 54-175 Hz it scrolls a loop marker out of view long
// before the next poll -- which made `loops_completed` a function of the
// app's log rate rather than of elapsed time, and therefore browser-
// correlated. `__rig_marks` collects only the two gesture sentences and is
// not a ring. The console scan is kept as a fallback so a page served
// without the marks buffer still reports what it can.
var MK = window.__rig_marks;
var marks_src = (MK && MK.length) ? MK : C;
for (var j = 0; j < marks_src.length; j++) {
  var mm = String(marks_src[j].msg || "");
  var mt = marks_src[j].t;
  var y = gesture_begin_re.exec(mm);
  if (y) begins.push({ t: mt, script: y[1] });
  y = gesture_loop_re.exec(mm);
  if (y) loops.push({ t: mt, script: y[1], frames: parseInt(y[2], 10) });
}
return { interact: interact, idle: idle, segments: segments, prep: prep,
         prep_geometry: prep_geometry,
         gpu: gpu, gpu_unavailable: gpu_unavailable, cadence: cadence,
         loop_state: loop_state, loop_state_all: loop_state_all,
         budget_state: budget_state, budget_state_all: budget_state_all,
         interact_all: interact_all, idle_all: idle_all,
         cadence_all: cadence_all,
         frame_segment_all: frame_segment_all, tile_take_all: tile_take_all,
         tile_phase_all: tile_phase_all, frame_prepare_all: frame_prepare_all,
         frame_post_all: frame_post_all,
         frame_dispatch_all: frame_dispatch_all,
         gesture_begins: begins, gesture_loops: loops,
         marks_total: (MK ? MK.length : -1),
         console_total: C.length };
"""


# ---- the Hist replica --------------------------------------------------
# The 42-slot histogram the interact/cadence lines embed, replicated from
# squallar-device-profile's hist.rs so a bin-diff answers the same
# conservative upper-edge percentiles the app itself would. Edge j (ns) is
# FIRST_OCTAVE[j % 4] << (j // 4); slot 0 is the under-62.5 us clamp whose
# upper bound is edge 0, slots 1..=40 are the geometric bins, slot 41 is the
# at-or-over-64 ms clamp, which has no upper edge and answers "over".

HIST_SLOTS = 42
HIST_FIRST_OCTAVE_NS = (62500, 74325, 88388, 105112)


def hist_edge_ns(i):
    return HIST_FIRST_OCTAVE_NS[i % 4] << (i // 4)


def hist_parse(s):
    """The `hist=` comma list -> 42 ints, or None on a malformed one."""
    try:
        counts = [int(x) for x in str(s).split(",")]
    except ValueError:
        return None
    return counts if len(counts) == HIST_SLOTS else None


def hist_diff(now, then):
    """Counts gained between two readings of one recorder; saturating, so a
    swapped pair reads as zeros rather than as garbage (same contract as
    Hist::diff)."""
    return [max(0, a - b) for a, b in zip(now, then)]


def hist_percentile_upper_us(counts, q):
    """Conservative q-quantile in whole microseconds: the upper edge (rounded
    up) of the slot holding the ceil(q*total)-th smallest sample. None on an
    empty histogram; 'over' for a sample in the over-ceiling clamp."""
    total = sum(counts)
    if total == 0:
        return None
    rank = min(max(int(-(-(q * total) // 1)), 1), total)
    seen = 0
    for slot, c in enumerate(counts):
        seen += c
        if seen >= rank:
            if slot == HIST_SLOTS - 1:
                return "over"
            return -(-hist_edge_ns(slot) // 1000)
    raise AssertionError("total counted a sample the walk did not reach")


def hist_window_max_us(counts):
    """The topmost occupied bin's conservative upper edge, us -- the closest
    thing a binned family has to a window max. "over" for the at-or-over
    64 ms clamp, whose upper edge does not exist; None for an empty diff."""
    top = None
    for slot, c in enumerate(counts):
        if c > 0:
            top = slot
    if top is None:
        return None
    if top == HIST_SLOTS - 1:
        return "over"
    return -(-hist_edge_ns(top) // 1000)


def hist_stats(counts):
    return {"n": sum(counts),
            "p50_us": hist_percentile_upper_us(counts, 0.50),
            "p90_us": hist_percentile_upper_us(counts, 0.90),
            "p99_us": hist_percentile_upper_us(counts, 0.99),
            "max_us": hist_window_max_us(counts)}


class FrameLineWatcher:
    """Accumulates frame-line readings across polls.

    The page-side console ring holds 1200 entries and evicts, so the reading
    that brackets the START of a gesture window can be gone from the ring by
    the time the window ends. Every interact/cadence reading seen at any poll
    is therefore kept here, keyed by its page-side timestamp; the marker
    lines likewise."""

    def __init__(self, session):
        self.session = session
        self.interact = {}
        self.idle = {}
        self.cadence = {}
        # The named families, one dict per name seen: "frame segment (pump)"
        # keys as "segment:pump", "tile take (vector)" as "take:vector". Kept
        # by name rather than in a fixed list because which tile-take families
        # appear is a property of the ARM (`put` is native-only; `sniffed` and
        # `restyle` need a plain-HTTP source and a theme flip), and a rig that
        # hard-coded them would report an absent arm as a broken one.
        self.named = {}
        self.begins = {}
        self.loops = {}
        self.last = {}

    def poll(self):
        sig = self.session.execute(FRAME_LINE_PROBE) or {}
        for r in sig.get("interact_all") or []:
            self.interact[(r.get("t"), r.get("n"))] = r
        for r in sig.get("idle_all") or []:
            self.idle[(r.get("t"), r.get("n"))] = r
        for r in sig.get("cadence_all") or []:
            self.cadence[(r.get("t"), r.get("n"))] = r
        for prefix, key in (("frame_segment_all", "segment"),
                            ("frame_prepare_all", "prepare"),
                            ("frame_post_all", "post"),
                            ("frame_dispatch_all", "dispatch"),
                            ("tile_take_all", "take"),
                            ("tile_phase_all", "phase")):
            for r in sig.get(prefix) or []:
                family = "%s:%s" % (key, r.get("name"))
                self.named.setdefault(family, {})[(r.get("t"), r.get("n"))] = r
        for r in sig.get("gesture_begins") or []:
            self.begins[(r.get("t"), r.get("script"))] = r
        for r in sig.get("gesture_loops") or []:
            self.loops[(r.get("t"), r.get("frames"))] = r
        self.last = sig
        return sig

    def named_families(self):
        """Every `segment:<name>` / `take:<name>` family a reading was seen
        for, in a stable order."""
        return sorted(self.named)

    def interact_n(self):
        """The newest cumulative interact-frame count, or None when the line
        has never been seen (not loud, or no reading yet) -- a different fact
        from n=0, which is a written line about an untouched window."""
        best = None
        for r in self.interact.values():
            if best is None or r["n"] > best:
                best = r["n"]
        return best

    def readings(self, family):
        by_name = {"interact": self.interact, "idle": self.idle,
                   "cadence": self.cadence}
        rs = list((by_name.get(family) or self.named.get(family) or {}).values())
        rs.sort(key=lambda r: (r.get("t") or 0, r.get("n") or 0))
        return rs


class RunningTotalsWatcher:
    """Accumulates the running-total lines a settle assertion differences.

    For `FrameLineWatcher`'s reason: the page-side console ring holds 1200
    entries and evicts, and the reading that brackets the START of a window
    can be gone by the time the window ends. Every `tile cache (<role>):`,
    `ground tiles:` and `basemap tiles:` match seen at any poll is kept here
    by its page-side timestamp.

    These three lines are written at most once per 2 s and ONLY when their
    ledger moved (`*_if_moved` on the Rust side), so SILENCE over a window is
    itself a zero delta for every counter on the line. That is what makes a
    running total the right shape to assert a settled viewport on: nothing
    has to be summed, and a reading the ring evicted is not a reading lost --
    the next one carries it."""

    def __init__(self, session):
        self.session = session
        self.tile_cache = {}
        self.ground = {}
        self.basemap = {}

    def poll(self):
        sig = self.session.execute(WORKER_SIGNAL_PROBE) or {}
        for r in sig.get("tile_cache_all") or []:
            self.tile_cache[(r.get("t"), r.get("role"))] = r
        for r in sig.get("ground_all") or []:
            self.ground[r.get("t")] = r
        for r in sig.get("basemap_all") or []:
            self.basemap[r.get("t")] = r
        return sig

    def readings(self, family, role=None):
        """Every reading of `family` (optionally of one `role`), oldest first."""
        source = {"tile_cache": self.tile_cache, "ground": self.ground,
                  "basemap": self.basemap}[family]
        rs = [r for r in source.values() if role is None or r.get("role") == role]
        rs.sort(key=lambda r: r.get("t") or 0)
        return rs


def tile_cache_settles(watcher, span_s, now_ms):
    """THE SETTLE ASSERTION: over the last `span_s` seconds of a static
    viewport, `tile cache (base): refetch after eviction`, `ground tiles:
    uploads` and `basemap tiles: vector` all moved by ZERO.

    Three denominators, never added and each asserted on its own: a cache
    ask the cache remembers evicting, a mesh buffer upload in the GPU store,
    and a vector body decoded. A viewport nobody is touching, whose tiles
    have all arrived, owes none of the three. A cache below the working set
    owes all three every frame -- it evicts a tile still on the glass, asks
    for it again, decodes it again, uploads it again -- which is the defect
    this leg exists to put a number on, and the reading `ground tiles:`
    alone cannot classify.

    The base of each difference is the last reading BEFORE the window, or
    zero when there is none: these are running totals from boot, so a first
    write inside the window is a move from zero. A family with no reading at
    all is an ERROR, not a zero -- either `squallar.raster_telemetry` is
    unseeded or the bundle predates the line -- and it fails the assertion
    rather than passing it vacuously. A family that wrote nothing inside the
    window is a zero delta by construction (see `RunningTotalsWatcher`), and
    the verdict says so as its basis.

    Pair this with --expect-frame-progress: a frame loop that DIED writes
    nothing either, and only the app's own frame counter tells the two apart.
    """
    window_start = now_ms - span_s * 1000.0
    out = {"ok": True, "window_s": span_s, "families": {}}
    checks = (
        ("tile cache (base): refetch after eviction",
         watcher.readings("tile_cache", role="base"), "refetch_after_eviction"),
        ("ground tiles: uploads", watcher.readings("ground"), "uploads"),
        ("basemap tiles: vector", watcher.readings("basemap"), "vector_tiles"),
    )
    for name, rs, field in checks:
        fam = {"readings": len(rs)}
        if not rs:
            fam.update(ok=False, delta=None, error=(
                "`%s` was never written: either squallar.raster_telemetry is "
                "unseeded, or the served bundle predates the line. NOT a zero "
                "reading" % name))
        else:
            inside = [r for r in rs if (r.get("t") or 0) >= window_start]
            before = [r for r in rs if (r.get("t") or 0) < window_start]
            if before:
                base, basis = before[-1][field], "last reading before the window"
            else:
                base, basis = 0, "boot: no reading before the window, running totals start at zero"
            if not inside:
                fam.update(ok=True, delta=0, base=base, last=base, in_window=0,
                           basis="silent: no line inside the window, and the "
                                 "line is written only when its ledger moves")
            else:
                last = inside[-1][field]
                delta = last - base
                fam.update(ok=(delta == 0), delta=delta, base=base, last=last,
                           in_window=len(inside), basis=basis)
                if delta != 0:
                    fam["error"] = (
                        "`%s` moved by %d over the last %.0f s of a static "
                        "viewport (%s -> %s, %d readings inside)"
                        % (name, delta, span_s, base, last, len(inside)))
        out["families"][name] = fam
        if not fam["ok"]:
            out["ok"] = False
    return out


def gesture_window_stats(watcher, quiet_settle_s=None, skip_loops=0):
    """The gesture-window bin-diff: percentiles over ONLY the frames between
    the first `gesture script ... begin` marker and the last
    `... loop complete` marker, for the interact and cadence families.

    Two cumulative histogram readings bracket the window: A = the last
    reading at or before the first marker (or an all-zero baseline when the
    script was already running at the first reading), B = the first reading
    at or after the last loop marker (or the newest reading, stated as the
    weaker basis). The diff is the window; this is what kills the spike's
    cumulative-from-boot p99 contamination.

    Returns None when no marker was ever seen -- an unarmed leg has no
    window, and inventing one from wall clock would put boot frames back into
    the tail.

    `quiet_settle_s` is the ONE exception, and it exists for scene E1: a leg
    that deliberately arms no gesture (a loop playing with nobody touching
    it) still has a figure of merit, and it is the idle family after boot has
    finished. The window is then `first reading + quiet_settle_s` to the
    newest reading, and the basis says so -- it is a WEAKER bracket than a
    marker pair, cut on wall clock rather than on the app's own signal, and
    a row carrying it must not be compared to a marker-bracketed one. It is
    used only when no marker was seen at all; an armed leg always prefers its
    markers."""
    begins = sorted(watcher.begins.values(), key=lambda r: r.get("t") or 0)
    loops = sorted(watcher.loops.values(), key=lambda r: r.get("t") or 0)
    if not begins and not loops:
        if quiet_settle_s is None:
            return None
        # The first histogram-bearing reading of any family anchors the
        # settle cut, so all three families are diffed over one window.
        firsts = [rs[0]["t"] for rs in
                  (watcher.readings(f) for f in
                   ("interact", "idle", "cadence")) if rs]
        if not firsts:
            return None
        return _window_stats(
            watcher, min(firsts) + float(quiet_settle_s) * 1000.0, None,
            {"script": "none (unarmed: loop playing, no gesture)",
             "loops_completed": 0,
             "basis": "quiet leg: first-reading+%gs settle to newest reading "
                      "(wall-clock cut, NOT a marker bracket -- never compare "
                      "to a gestured row)" % float(quiet_settle_s)})
    t0 = begins[0]["t"] if begins else None
    if loops:
        t1, basis = loops[-1]["t"], "first-begin-to-last-loop-marker"
    else:
        t1, basis = None, "first-begin-to-newest-reading (no loop completed)"
    # `begin` is logged ONCE, at the player's construction -- which is boot.
    # So the default bracket above starts before the app has settled, and the
    # boot burst is inside every window it cuts. `skip_loops` moves the left
    # edge to the Nth COMPLETED loop instead: a whole number of scripted
    # loops, none of them the first. It is a strictly narrower window on the
    # same readings, reported BESIDE the default rather than replacing it,
    # because every row taken before this existed used the default and the
    # two must stay tellable apart.
    script = begins[0].get("script") if begins else loops[-1].get("script")
    out = _window_stats(
        watcher, t0, t1,
        {"script": script, "loops_completed": len(loops), "basis": basis})
    if skip_loops and len(loops) > skip_loops:
        out["settled"] = _window_stats(
            watcher, loops[skip_loops - 1]["t"], t1,
            {"script": script,
             "loops_completed": len(loops) - skip_loops,
             "basis": "loop-%d-complete to last-loop-marker (whole scripted "
                      "loops, boot excluded)" % skip_loops})
    return out


def _window_stats(watcher, t0, t1, out):
    """Bin-diff every family between the bracket `t0`..`t1`. Split out of
    `gesture_window_stats` only so the quiet-leg bracket and the marker
    bracket cannot drift into two different diffs."""
    out = dict(out, t0=t0, t1=t1)
    # `idle` in a gesture window is the settle-burst family: the input-free
    # frames of the scripted quiet phases, which is where WO-8 moved the
    # post-gesture re-raster. Its max is the burst's worst frame.
    #
    # The named families come off the watcher rather than from a list, so an
    # arm that never produced a `tile take (put)` reading simply has no such
    # key -- an absence, not a zero, and the two must stay tellable apart.
    for family in ("interact", "idle", "cadence") + tuple(
            watcher.named_families()):
        rs = [r for r in watcher.readings(family)
              if hist_parse(r.get("hist")) is not None]
        if not rs:
            out[family] = {"error": "no histogram-bearing reading scraped"}
            continue
        a = None
        if t0 is not None:
            for r in rs:
                if r["t"] <= t0 and (a is None or r["t"] > a["t"]):
                    a = r
        b = None
        if t1 is not None:
            for r in rs:
                if r["t"] >= t1 and (b is None or r["t"] < b["t"]):
                    b = r
        if b is None:
            b = rs[-1]
        a_counts = (hist_parse(a["hist"]) if a is not None
                    else [0] * HIST_SLOTS)
        d = hist_diff(hist_parse(b["hist"]), a_counts)
        stats = hist_stats(d)
        stats["bracket_a_t"] = a["t"] if a is not None else None
        stats["bracket_b_t"] = b["t"]
        # The EXACT windowed mean, where the line carries a running sum.
        # Both `n` and `sum` subtract, so this is arithmetic and not an
        # estimate -- which matters because the bins are four per octave, one
        # bin apart is 0%-19%, and every true ratio between 1.68x and 2.38x
        # reads as exactly 2.00x off the percentiles alone. Read it AGAINST
        # p90/p99, never instead of them: a per-take cost is bimodal and its
        # mean describes none of its samples.
        stats["mean_us"] = _window_mean_us(a, b)
        # The diffed bins themselves, not just the five figures read off them.
        # A per-take cost is bimodal -- trivial tiles and content tiles are
        # different populations sharing one histogram -- and no percentile
        # shows that shape. Kept in the artifact so a distribution question
        # can be asked of a finished run without re-measuring it.
        stats["bins"] = d
        out[family] = stats
    return out


# Every windowed named family the SUMMARY prints, by key prefix. The window
# itself is built off `watcher.named_families()`, which is unfiltered -- so a
# prefix missing from here does not lose the data, it makes the data INVISIBLE
# to anyone reading stdout while the artifact quietly carries it. `prepare:`
# was missing from the day the prepare split landed, and `dispatch:` would
# have been missing from the day the dispatch split did.
#
# Keep this list equal to the set of families the app can emit. A family the
# arm never produced still has no key, so an absent arm stays an ABSENCE and
# not a zero -- that property is the dict's, not this list's.
WINDOW_FAMILY_PREFIXES = ("segment:", "prepare:", "post:", "dispatch:",
                          "take:", "phase:")


def watcher_named_in(gw):
    """The named-family keys a finished gesture-window dict carries, in a
    stable order. Read off the result rather than off a fixed list, so a
    family the arm never produced is an ABSENCE and not a zero."""
    return sorted(k for k in (gw or {})
                  if isinstance(k, str)
                  and k.startswith(WINDOW_FAMILY_PREFIXES))


def _window_mean_us(a, b):
    """(sum_b - sum_a) / (n_b - n_a) in whole us, or None when the line
    carries no running sum (the interact/idle/cadence families do not) or the
    window holds no samples."""
    if b is None or b.get("sum") is None:
        return None
    n = b.get("n", 0) - (a.get("n", 0) if a is not None else 0)
    if n <= 0:
        return None
    total = b["sum"] - (a.get("sum", 0) if a is not None else 0)
    if total < 0:
        return None
    return total // n


# ---- W3C actions gestures ----------------------------------------------
# Real input through the driver's own /actions endpoint (stdlib wire, no
# selenium): a pointer input source for drags and a wheel input source for
# scroll. Verified against geckodriver FIRST (Firefox governs); chromedriver
# accepts the same payloads. Some drivers refuse the wheel source
# ("unknown/unsupported input source"); the fallback is a synthesized
# WheelEvent via execute_script, and WHICH route ran is recorded per browser
# on the result -- a synthesized event skips the browser's input pipeline, so
# the two must never be pooled silently.

WHEEL_FALLBACK_SCRIPT = """
var x = arguments[0], y = arguments[1], dy = arguments[2], n = arguments[3];
var el = document.elementFromPoint(x, y) || document.body;
for (var i = 0; i < n; i++) {
  el.dispatchEvent(new WheelEvent('wheel', {
    deltaY: dy, deltaMode: 0, clientX: x, clientY: y,
    bubbles: true, cancelable: true, composed: true }));
}
return n;
"""


def perform_actions(session, actions, timeout=60.0):
    return session.cmd("POST", "/actions", {"actions": actions},
                       timeout=timeout)


def release_actions(session):
    try:
        session.cmd("DELETE", "/actions", timeout=30.0)
    except WebDriverError:
        pass


def w3c_pan(session, cx, cy, dx, dy, duration_ms=700, steps=14):
    """One press-drag-release through the pointer input source."""
    acts = [{"type": "pointerMove", "duration": 0, "x": int(cx), "y": int(cy)},
            {"type": "pointerDown", "button": 0}]
    for k in range(1, steps + 1):
        acts.append({"type": "pointerMove",
                     "duration": max(1, duration_ms // steps),
                     "x": int(cx + dx * k / steps),
                     "y": int(cy + dy * k / steps)})
    acts.append({"type": "pointerUp", "button": 0})
    perform_actions(session, [{"type": "pointer", "id": "rig-mouse",
                               "parameters": {"pointerType": "mouse"},
                               "actions": acts}])


def w3c_wheel(session, x, y, delta_y, notches=2, pause_ms=120):
    """Wheel notches through the wheel input source. Raises WebDriverError
    when the driver refuses the source; the caller owns the fallback."""
    acts = []
    for _ in range(notches):
        acts.append({"type": "scroll", "x": int(x), "y": int(y),
                     "deltaX": 0, "deltaY": int(delta_y),
                     "duration": 0, "origin": "viewport"})
        acts.append({"type": "pause", "duration": pause_ms})
    perform_actions(session, [{"type": "wheel", "id": "rig-wheel",
                               "actions": acts}])


def drive_w3c_gesture(session, kind, seconds, inner):
    """Drive `kind` ('pan', 'wheel' or 'pan+wheel') for ~`seconds` seconds
    around the viewport centre. Pans come in mirrored pairs and wheel legs
    zoom in exactly as much as they zoom back out, so the scene ends where it
    began. Returns what actually ran, wheel route included."""
    w, h = (inner or [1280, 900])[:2]
    cx, cy = w // 2, h // 2
    reach = max(40, min(w, h) // 5)
    out = {"kind": kind, "asked_seconds": seconds, "pans": 0,
           "wheel_notches": 0, "wheel_source": None}
    t0 = time.monotonic()
    flip = 1
    while time.monotonic() - t0 < seconds:
        if "pan" in kind:
            # The mirrored pair: the second stroke undoes the first.
            w3c_pan(session, cx, cy, flip * reach, flip * reach // 2)
            w3c_pan(session, cx, cy, -flip * reach, -flip * reach // 2)
            out["pans"] += 2
            flip = -flip
        if "wheel" in kind:
            for delta in (-120, 120):
                if out["wheel_source"] in (None, "w3c-wheel"):
                    try:
                        w3c_wheel(session, cx, cy, delta)
                        out["wheel_source"] = "w3c-wheel"
                    except WebDriverError as e:
                        out["wheel_source"] = (
                            "synthesized-WheelEvent (driver refused the "
                            "wheel input source: %.120s)" % e)
                        session.execute(WHEEL_FALLBACK_SCRIPT,
                                        [cx, cy, delta, 2])
                else:
                    session.execute(WHEEL_FALLBACK_SCRIPT, [cx, cy, delta, 2])
                out["wheel_notches"] += 2
    release_actions(session)
    out["ran_seconds"] = round(time.monotonic() - t0, 2)
    return out


# ---- generic console wait ----------------------------------------------

CONSOLE_MATCH_PROBE = """
var re = new RegExp(arguments[0]);
var C = window.__rig_console || [];
for (var i = C.length - 1; i >= 0; i--) {
  var m = String(C[i].msg || "");
  if (re.test(m)) return { matched: true, t: C[i].t, index: i,
                           msg: m.slice(0, 400) };
}
return { matched: false, scanned: C.length };
"""


def wait_console_regex(session, pattern, timeout=60.0, interval=0.5):
    """Poll the console ring until a line matches `pattern` (a JS regex
    source string). The generic wait-until-console-line primitive: readiness
    for anything the app announces, without a new probe per sentence."""
    t0 = time.monotonic()
    last = None
    while time.monotonic() - t0 < timeout:
        last = session.execute(CONSOLE_MATCH_PROBE, [pattern]) or {}
        if last.get("matched"):
            out = dict(last)
            out.update({"ok": True, "pattern": pattern,
                        "waited_s": round(time.monotonic() - t0, 2)})
            return out
        time.sleep(interval)
    return {"ok": False, "pattern": pattern,
            "waited_s": round(time.monotonic() - t0, 2),
            "error": "no console line matched /%s/ within %.0fs (scanned %s "
                     "ring entries at the last poll)"
                     % (pattern, timeout, (last or {}).get("scanned"))}


# Which backend the APP settled on, read out of its own startup log.
#
# This is the only observable answer, and it is not the same question the
# WEBGPU_PROBE below asks. That one says what the BROWSER can do; this says what
# the build DID -- the choice is made inside `App::create_instance` by a
# `requestAdapter()` nothing page-side repeats, and once made, WebGPU and WebGL2
# draw the same picture at different ceilings. A run that quotes a cap without
# naming the API that produced it is quoting an adapter it did not identify.
#
# Two lines, both `log::info!` from `squallar_app::app_state`:
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
    """Poll until #squallar-status is removed (booted) or reports failure.
    Returns (final_probe, seconds_waited, timed_out)."""
    t0 = time.monotonic()
    probe = None
    while time.monotonic() - t0 < timeout:
        probe = session.execute(BOOT_PROBE)
        if probe and (probe.get("booted") or probe.get("failed")):
            return probe, time.monotonic() - t0, False
        time.sleep(interval)
    return probe, time.monotonic() - t0, True


def _buffer_size(result):
    """The canvas DRAWING BUFFER this leg actually rendered at, as `WxH`.

    The buffer and not `clientWidth`: CSS pixels are what the page was laid out
    in, device pixels are what was rasterized, and on a DPR that is not 1 they
    are different numbers. Every per-leg figure -- raster bytes, texture
    uploads, frame times -- is a figure PER THIS SIZE, so it travels in the
    verdict rather than being recoverable only by digging through the probe.
    """
    b = (result.get("canvas_final")
         or (result.get("boot") or {}).get("probe") or {})
    if not b or not b.get("bufferWidth"):
        return None
    return "%sx%s" % (b.get("bufferWidth"), b.get("bufferHeight"))


def fit_canvas(session, target, window, attempts=5):
    """Resize the browser window until the canvas DRAWING BUFFER is `target`.

    The window is not the viewport. Firefox and Chromium eat different amounts
    of it for chrome -- measured on this box at one `--window 1280x900`,
    firefox gave a 1280x779 canvas and chromium a 1248x714, a 12% difference
    in pixels between two rows that were being read side by side. And the
    native window is a third size again. Pixels are what the overlay picture
    is sized from, so two legs at two canvas sizes are two measurements.

    Chrome height is constant for a session, so the correction converges in
    one step; the loop exists for the window manager that rounds. Returns what
    was asked, what was got, and whether they agree -- an unmet target is
    reported, never silently accepted.
    """
    got = session.execute(BOOT_PROBE) or {}
    w, h = window
    for _ in range(attempts):
        have = (got.get("bufferWidth", 0), got.get("bufferHeight", 0))
        if have == target:
            break
        if have == (0, 0):
            break
        w += target[0] - have[0]
        h += target[1] - have[1]
        session.set_window_rect(w, h)
        time.sleep(0.6)
        got = session.execute(BOOT_PROBE) or {}
    have = (got.get("bufferWidth", 0), got.get("bufferHeight", 0))
    return {"asked": "%dx%d" % target, "got": "%dx%d" % have,
            "window": "%dx%d" % (w, h), "met": have == target}


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
    worker that fell back to `squallarRayonSerialPool` still attaches, still
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


def wait_zero_copy_replies(session, timeout=180.0, interval=2.0):
    """WS3c: replies reach the page WITHOUT being copied out of the worker.

    Two conditions, and the second is what stops this passing vacuously:

      * `out_copied == 0` -- no reply byte arrived in a buffer the worker had
        copied out of its own linear memory.
      * `out_moved > 0`  -- and some reply bytes actually arrived. A page that
        never got an answer trivially never got a copied one, so without this
        the assertion would go green on a transport that had stopped working
        entirely.

    Both numbers are the PAGE's own observation, not the worker's report. The
    page classifies each arriving buffer with `shared_loan::is_foreign_shared`:
    a lent reply is a view whose backing object is a SharedArrayBuffer that is
    NOT this page's memory, and a copied one is a transferred plain
    ArrayBuffer. So a transport that quietly reverted to copying cannot satisfy
    it, and neither can one that forged a view out of the page's own heap.

    The negative control is a real production configuration rather than a
    tamper: served without COOP/COEP the agent cluster is not cross-origin
    isolated, `shared_loan::can_lend` is false, every message takes the copying
    wire and `out_copied` climbs with `out_moved`. Run `serve.py` without
    `--coep` and this must go red. That is the same deployment GitHub Pages
    gets, so the arm is one the app has to keep working -- it is asserted to be
    SLOWER here, never broken.

    Accumulates across polls the way `wait_rayon_pool` does: the page-side
    console ring evicts, and the totals are cumulative, so the newest line seen
    at any poll is the best answer and a later poll can only improve it."""
    t0 = time.monotonic()
    best = None
    while time.monotonic() - t0 < timeout:
        sig = session.execute(WORKER_SIGNAL_PROBE) or {}
        seen = sig.get("transport")
        if isinstance(seen, dict) and isinstance(seen.get("out_moved"), int):
            if best is None or seen["out_moved"] >= best["out_moved"]:
                best = seen
        if best is not None and best["out_moved"] > 0 and best["out_copied"] == 0:
            out = dict(best)
            out.update({"ok": True, "waited_s": round(time.monotonic() - t0, 2)})
            return out
        time.sleep(interval)
    out = dict(best or {})
    out.update({
        "ok": False,
        "waited_s": round(time.monotonic() - t0, 2),
        "error": "replies did not arrive zero-copy within %.0fs: %s (wanted "
                 "out_moved>0 and out_copied==0; a transport that reverted to "
                 "copying still attaches, still answers jobs and still passes "
                 "every other Tier-2 assertion)" % (timeout, best),
    })
    return out


def wait_overlay_rasters(session, timeout=180.0, interval=2.0):
    """WS2 baseline: the whole-picture overlay path really ran in THIS browser.

    Until this existed nothing in the rig read the overlay pipeline at all: a
    4/4 PASS said the build was unbroken and said nothing whatever about
    overlay uploads. Every tile-grid figure produced for WS2 so far is a policy
    simulation over a model of this path, and this is what makes the real thing
    observable per browser.

    Six conjuncts, and each can fail on its own. That is the point: a gate
    with one conjunct on a byte total goes green on a page that never enabled a
    texture layer, because `0 B` is what both the working-but-idle and the
    never-ran cases report. The lesson is WS3b's (a worker that fell back to
    one thread still attached, still answered jobs, and passed every assertion
    the rig had) and WS3c's (`out_copied == 0` needs `out_moved > 0` beside
    it).

    THE SIXTH WAS ADDED 2026-08-31 AND THE FIVE BEFORE IT COULD NOT SEE WHAT IT
    SEES. `pictures` and `picture_bytes` count the RGBA buffer handed to egui
    whatever is in it, so a texture layer emitting a fully transparent pixmap
    satisfied all five over a map painting nothing -- measured on a
    deliberately emptied raster: 6 dispatched, 6 arrived, 6 pictures. Anyone
    reasoning "the rig would catch it if the layer stopped drawing" was wrong
    for as long as there were five.

      * `dispatched > 0`   -- something asked for an overlay raster. THE
                              FLOOR. Zero means no enabled texture layer had
                              data, and every figure below is then trivially
                              zero for a reason that is not about uploads.
      * `pictures > 0`     -- rasters came back and their pixels were handed
                              to egui. `dispatched > 0` with this at zero is a
                              dispatch whose answers never arrive.
      * `picture_bytes > 0`-- and the pictures had pixels in them. Says
                              nothing about what those pixels were: this is
                              the size of the buffer.
      * `inked > 0`        -- and at least one of those pictures would have
                              CHANGED the frame it was drawn on. egui's
                              Color32 is premultiplied, so a pixel that
                              contributes nothing is zero in all four bytes
                              and "some byte is non-zero" is exact rather
                              than a heuristic. `inked < pictures` is layers
                              rasterizing blank and is reported, not gated:
                              `NwsAlerts` legitimately paints nothing when no
                              alert is in view, so an equality here would red
                              on quiet weather.
      * `shown + promoted > 0` -- and at least one reached the screen. A page
                              that uploaded and then dropped everything fails
                              here and passes the three above.
      * `arrived == pictures + dropped` -- the arrival balance, an identity
                              over the path rather than a threshold. It breaks
                              the moment an arrival leaves by an exit neither
                              counter names, which is how a byte figure
                              quietly starts describing a subset.

    NEGATIVE CONTROL, and it is a real configuration rather than a tamper: the
    every-texture-layer-off seed written out beside `SEED_LS` in run_tier2.sh
    -- the state a user who cleared their layer stack is in. RE-RUN
    2026-08-31, chromium: this goes red and every other Tier-2 assertion stays
    green, which is precisely the hole it closes.

    **It reddens by ABSENCE, not by zeroes, and the old text here said
    otherwise.** `report_raster_telemetry` writes nothing on a tick where
    `totals_if_moved()` moved nothing, so with every texture layer off the
    `overlay rasters:` line is never emitted at all and the reading is `None`
    rather than a row of zeroes. That matters because it is the SAME null a
    rig/bundle skew produces, and telling those apart is what
    `rasters_unparsed` is for.

    THE OTHER HALF OF THE CONTROL, measured the same day: the seed reduced to
    `RadarCoverage` alone -- every weather-fed texture layer explicitly off --
    reaches 2 dispatched / 2 pictures / 16512000 B / **2 inked** and PASSES.
    So no conjunct here, `inked` included, depends on a live fetch landing:
    the compiled-in site table carries all six on a box with no weather. A red
    on this gate is therefore never "the data never came".

    **It is NOT "remove the seeded layer".** That was the first control tried
    and it does not work: `NwsAlerts` and `SpcDiscussions` are on by default
    and are both texture layers, so with `RadarCoverage` gone they still rasterize
    whenever the live feeds have anything in them. Measured 2026-08-22 with the
    key removed, chromium still reached 2 dispatched / 2 pictures /
    16512000 B. Which is also why the seed exists at all -- not to supply the
    only texture overlay, but to supply the only one that does not depend on
    the weather.

    Accumulates across polls the way `wait_rayon_pool` and
    `wait_zero_copy_replies` do: the page-side console ring evicts, the totals
    are cumulative, and the newest line seen at any poll is the whole answer.

    **`waited_s` IS NOT "how long the app took to produce its first overlay
    rasters", and it must never be quoted as one.** Two things stop it. It is
    quantised to `interval`, which is 2 s, so it can only ever land near 0,
    2, 4 …; and its zero is the start of THIS wait, which runs after the boot,
    canvas, worker-round-trip, rayon-pool and zero-copy waits, each of which
    gives the app a variable head start before the clock here begins. A run
    reading 0.10 and a run reading 2.26 may differ by milliseconds of app
    timing that straddled the first poll, or by nothing at all in the app.

    MEASURED 2026-08-31, chromium, this leg re-run with `interval` cut to
    0.1 s: eight runs landed in 0.61-1.33 s with **identical** counters every
    time (6 dispatched / 6 arrived / 6 pictures / 4 inked / 3 shown /
    3 promoted). At the shipped 2 s interval the same behaviour reports as
    either ~0.1 s or ~2.2 s, and the accompanying `dispatched` as either a
    part-accumulated 3 or a settled 6. A "23x spread in first-raster arrival"
    read off the 2 s instrument is therefore an artefact of its own
    quantisation and of where its zero is, and the underlying spread it was
    read from is 2.2x. Cut the interval before believing any figure here.
    """
    t0 = time.monotonic()
    best = None
    unparsed = None
    polls = 0
    first = None
    last = None
    while time.monotonic() - t0 < timeout:
        sig = session.execute(WORKER_SIGNAL_PROBE) or {}
        polls += 1
        seen = sig.get("rasters")
        if isinstance(seen, dict) and isinstance(seen.get("dispatched"), int):
            if first is None:
                first = seen
            last = seen
            if best is None or seen["dispatched"] >= best["dispatched"]:
                best = seen
        if best is not None and _overlay_rasters_ok(best):
            out = dict(best)
            out.update({"ok": True, "waited_s": round(time.monotonic() - t0, 2),
                        "polls": polls})
            return out
        unparsed = sig.get("rasters_unparsed") or unparsed
        # **Returns on the first skew reading instead of waiting out the
        # clock.** A telemetry line's field list cannot change while the page
        # runs, so every remaining poll would read exactly this one; the wait
        # was spending the full timeout, twice with the quarantine retry, to
        # arrive at a strictly worse answer (411 s measured, against 28-31 s
        # for a green leg). Guarded on `best is None` so a bundle this rig CAN
        # read never takes this exit — a parsed reading means there is no skew
        # to report, whatever else is wrong with the figures.
        if best is None and unparsed:
            return {
                "ok": False,
                "waited_s": round(time.monotonic() - t0, 2),
                "polls": polls,
                "skew": True,
                "unparsed_line": unparsed[:400],
                "error": _telemetry_skew_error(
                    "overlay rasters", unparsed[:400],
                    "dispatched, arrived, pictures of B, inked, shown, "
                    "promoted, dropped, superseded, cancelled (`inked` "
                    "joined the line at 79bad6c7, 2026-08-31)"),
            }
        time.sleep(interval)
    out = dict(best or {})
    if best is None:
        # No reading and no skew: the sentence genuinely never appeared.
        out.update({
            "ok": False,
            "waited_s": round(time.monotonic() - t0, 2),
            "polls": polls,
            "error": "no `overlay rasters:` line reached the console ring "
                     "within %.0fs, in any shape this rig can or cannot "
                     "parse. That is NOT the same fact as zeroed counters: "
                     "either squallar.raster_telemetry is unseeded (the line "
                     "is `debug` without it and never reaches the ring), or "
                     "the app never wrote one because no overlay raster ever "
                     "moved. Check the seed before reading it as the second."
                     % timeout,
        })
        return out
    unmet = _overlay_rasters_unmet(best)
    # **Slow is not stuck, and the failure has to say which.** A reading that
    # was still changing when the clock expired is a path that is RUNNING and
    # did not finish in time -- "nothing has landed yet", a starved or
    # throttled box -- and the repair is a different one from a reading that
    # never moved across every poll, which is a path that stopped. The old
    # text could not tell those apart and neither could its reader.
    moved = first is not None and last is not None and first != last
    progress = (
        "the reading was STILL CHANGING when the clock ran out (first %s, "
        "last %s, over %d polls) -- the path is running and did not finish "
        "in time, which is a slow or starved box rather than a renderer that "
        "stopped; re-run before treating it as a defect, and check the box's "
        "load" % (first, last, polls)
        if moved else
        "the reading did NOT move across %d polls spanning %.0fs -- the path "
        "is stuck, not slow, so a longer timeout would not have helped and "
        "widening one would only make this fail less often"
        % (polls, timeout))
    out.update({
        "ok": False,
        "waited_s": round(time.monotonic() - t0, 2),
        "polls": polls,
        "moved_while_waiting": moved,
        "unmet": unmet,
        "error": "the overlay raster path did not complete within %.0fs. The "
                 "line parsed, so this is the app's own reading and not a "
                 "rig/bundle skew: %s. The conjunct%s it fails %s: %s. And %s"
                 % (timeout, best,
                    "" if len(unmet) == 1 else "s",
                    "is" if len(unmet) == 1 else "are",
                    "; ".join(unmet), progress),
    })
    return out


def wait_seed_applied(session, timeout=180.0, interval=2.0):
    """The scene seed was written by THIS page and read by THIS app.

    Four conjuncts, and the order matters: the first two are strict positives
    and the last two are absences that only mean something behind them.

      * `squallar.ui in seeded` -- PAGE_PRELUDE ran and wrote the config key.
                            Fails outright on a leg pointed at /index.html,
                            which is the shape that shipped: no prelude, no
                            window.__rig, no seed, and the app picks a site
                            off the machine's timezone.
      * `loud > 0`        -- a `loop state:` line reached the console ring.
                            report_frame_telemetry writes it every period
                            regardless of what moved, gated only on
                            squallar.frame_telemetry -- a key from the same
                            seed object. So the app READ this seed. THIS IS
                            THE NON-VACUITY FLOOR: without it, a page that
                            logged nothing satisfies both absences below, and
                            a page with no prelude has no ring to log into.
      * `fallback == []`  -- no "opening on <site>, nearest to timezone
                            <zone>". That line is reachable only where
                            gui.load_ui_config returned false, so its presence
                            is the seed being absent or unusable, stated by
                            the app itself.
      * `refused == []`   -- no "Failed to parse config", no "config names a
                            site no radar could be called". A seed can arrive,
                            parse as JSON, and still be thrown away.

    WHAT IT DOES NOT CLAIM, and the omission is deliberate. Nothing the app
    exposes reports the pane count, the site set or the enabled layers after
    boot -- `loop state:` counts only ANIMATING panes, so it reads 0 panes for
    a six-pane scene that is not looping and is useless as a layout check. So
    this proves "a seed was written and this build accepted it", and the HOST
    side proves "that literal describes the claimed scene"
    (`ui_config::rig_seed_tests` for run_tier2.sh,
    `ui_config::measure_seed_tests` for all seven of run_measure.sh's). The
    pair is the chain; neither half is the claim.

    NEGATIVE CONTROL, and it is the defect itself rather than a tamper: point
    --url at /index.html. `seeded` reads null, this goes red naming that, and
    every other Tier-2 assertion stays green -- the app boots, paints, attaches
    its worker and rasters overlays on whatever scene it chose for itself.

    Polls because the fallback line is written during boot and `loop state:`
    only after the first telemetry period, so a single early read would see
    neither and pass on both counts.
    """
    t0 = time.monotonic()
    best = None
    while time.monotonic() - t0 < timeout:
        seen = session.execute(SEED_APPLIED_PROBE) or {}
        if isinstance(seen, dict):
            # The newest reading is the whole answer: `seeded` never changes
            # after the prelude, and both `loud` and the two lists are
            # cumulative over a ring that only grows within a leg.
            best = seen
        if best is not None and _seed_applied_ok(best):
            out = dict(best)
            out.update({"ok": True, "waited_s": round(time.monotonic() - t0, 2)})
            return out
        time.sleep(interval)
    out = dict(best or {})
    seeded = (best or {}).get("seeded")
    if not isinstance(seeded, list):
        why = ("window.__rig.seeded is %r: PAGE_PRELUDE never ran, so nothing "
               "was written to localStorage at all. The usual cause is a --url "
               "pointing at /index.html; serve.py only injects the prelude on "
               "the /index-rig.html route" % (seeded,))
    elif UI_CONFIG_SEED_KEY not in seeded:
        why = ("the prelude wrote %r and none of them is %r, so the scene seed "
               "is not among the keys this leg set"
               % (seeded, UI_CONFIG_SEED_KEY))
    elif not (best or {}).get("loud"):
        why = ("the prelude wrote the seed and no `loop state:` line ever "
               "reached the console ring, so nothing proves the app read it. "
               "Either squallar.frame_telemetry is missing from the seed or "
               "the app never got far enough to write a telemetry period")
    elif (best or {}).get("fallback"):
        why = ("the app logged %r: gui.load_ui_config returned false and this "
               "leg is measuring a site derived from the machine's timezone, "
               "not the seeded scene" % ((best or {}).get("fallback"),))
    else:
        why = ("the app logged %r: the seed arrived and was rejected"
               % ((best or {}).get("refused"),))
    out.update({
        "ok": False,
        "waited_s": round(time.monotonic() - t0, 2),
        "error": "the scene seed was not applied within %.0fs: %s" % (timeout, why),
    })
    return out


# The localStorage name of the config key, as squallar_web::kv::storage_key
# spells it. Pinned from Rust by `the_rig_gates_on_the_key_it_seeds`.
UI_CONFIG_SEED_KEY = "squallar.ui"


def _seed_applied_ok(r):
    """The four conjuncts of `wait_seed_applied`, over one reading."""
    seeded = r.get("seeded")
    return (isinstance(seeded, list)
            and UI_CONFIG_SEED_KEY in seeded
            and r.get("loud", 0) > 0
            and not r.get("fallback")
            and not r.get("refused"))


def wait_basemap_tiles(session, timeout=180.0, interval=2.0):
    """The self-hosted VECTOR BASEMAP really decoded tiles in THIS browser.

    THE HOLE THIS CLOSES, and it is not hypothetical. A `usize`->`u64` offset
    widening in the vendored PMTiles reader made the basemap serve ZERO tiles
    in a browser for as long as that build shipped, and this rig passed every
    leg throughout. Nothing here read the basemap: a page with no ground under
    the map still boots, still reports a non-blank canvas (the overlays paint),
    still attaches its worker, still answers jobs off the frame, and still
    satisfies all six conjuncts of `--expect-overlay-rasters` -- the basemap
    is not in that dispatch, so its total is untouched by the basemap being
    dead. The gate was green and the map was empty.

    ONE CONJUNCT, deliberately, and it is a strict positive on the exact thing
    the defect destroyed:

      * `vector_tiles > 0` -- at least one MVT body out of the self-hosted
                              archive was fetched, range-read and decoded into
                              a tile. Not "the archive opened", not "a request
                              was made": decoded.

    A single conjunct is normally the shape that goes green vacuously (see
    `wait_overlay_rasters`, which needs five because a byte total reads zero in
    both the working-but-idle and the never-ran cases). It is safe here because
    this reading has no idle state to be confused with a dead one: the basemap
    is always on -- `BasemapTiles` ships `default_enabled() == true` -- and it
    is the one layer that draws on every frame regardless of the weather, so a
    live page ALWAYS decodes tiles. There is no configuration in which zero is
    the correct answer.

    Both failure shapes are red and they are distinguishable in the reading:
    `null` means the `basemap tiles:` line was never written at all (either the
    telemetry key was not seeded or not one archive body ever decoded), while
    `0 vector` means the line was written and the counter never moved. The
    error names which one it saw.

    `raster_tiles` (the terrain hillshade) and `sniffed_tiles` are reported
    beside it and NEVER gated on: terrain is a layer a user may have off, so
    zero there is legitimate, and `sniffed` is expected to be zero on every
    archive this app opens -- a non-zero reading there is a finding to look at,
    not a pass/fail.

    NEGATIVE CONTROL: revert the `usize`->`u64` offset widening in
    `vendor/pmtiles` (the shipped defect itself). Measured behaviour of that
    build is zero basemap tiles in both browsers with every other Tier-2
    assertion green, which is the property -- this goes red, it goes red for
    the reason it names, and it disturbs nothing else.

    Accumulates across polls exactly as `wait_overlay_rasters` does: the
    page-side console ring evicts, the totals are cumulative, and the newest
    line seen at any poll is the whole answer.
    """
    t0 = time.monotonic()
    best = None
    unparsed = None
    while time.monotonic() - t0 < timeout:
        sig = session.execute(WORKER_SIGNAL_PROBE) or {}
        seen = sig.get("basemap")
        if isinstance(seen, dict) and isinstance(seen.get("vector_tiles"), int):
            if best is None or seen["vector_tiles"] >= best["vector_tiles"]:
                best = seen
        if best is not None and best.get("vector_tiles", 0) > 0:
            out = dict(best)
            out.update({"ok": True, "waited_s": round(time.monotonic() - t0, 2)})
            return out
        # The same skew exit `wait_overlay_rasters` takes, for the same
        # reason: this gate's `null` reads as "the basemap decoded nothing",
        # which is the exact shipped defect it was built to catch, so a
        # field-list change here would frame a stale bundle as that defect
        # returning. It has not happened to this line yet; the reading costs
        # nothing and the failure mode is already proven on its neighbour.
        unparsed = sig.get("basemap_unparsed") or unparsed
        if best is None and unparsed:
            return {
                "ok": False,
                "waited_s": round(time.monotonic() - t0, 2),
                "skew": True,
                "unparsed_line": unparsed[:400],
                "error": _telemetry_skew_error(
                    "basemap tiles", unparsed[:400],
                    "vector, raster, sniffed"),
            }
        time.sleep(interval)
    out = dict(best or {})
    out.update({
        "ok": False,
        "waited_s": round(time.monotonic() - t0, 2),
        "error": ("no vector basemap tile decoded within %.0fs: %s (wanted "
                  "vector_tiles>0; %s). A basemap that decodes nothing passes "
                  "boot, canvas, the worker wire and every conjunct of "
                  "--expect-overlay-rasters -- which is how one shipped"
                  % (timeout, best,
                     "the `basemap tiles:` line was never written at all, so "
                     "either squallar.raster_telemetry is unseeded or not one "
                     "archive body ever decoded"
                     if best is None else
                     "the line was written and the vector counter never moved")),
    })
    return out


def _overlay_rasters_ok(r):
    """The six conjuncts of `wait_overlay_rasters`, over one reading."""
    return not _overlay_rasters_unmet(r)


def _overlay_rasters_unmet(r):
    """Which of the six conjuncts this reading fails, in order.

    The failure text names these rather than restating all six and leaving
    the reader to diff them against the figures — `inked == 0` beside
    `pictures > 0` is "every layer rasterized blank", and `dispatched == 0`
    is "no enabled texture layer had data". Those are different bugs in
    different crates, and the old message described them identically.
    """
    unmet = []
    if r.get("dispatched", 0) <= 0:
        unmet.append("dispatched>0 (no enabled texture layer had data to "
                     "raster; every figure below is then trivially zero)")
    if r.get("pictures", 0) <= 0:
        unmet.append("pictures>0 (dispatched, but no raster's pixels ever "
                     "reached egui)")
    if r.get("picture_bytes", 0) <= 0:
        unmet.append("picture_bytes>0 (pictures arrived with no pixels in "
                     "them)")
    if r.get("inked", 0) <= 0:
        unmet.append("inked>0 (pictures arrived and EVERY ONE was blank — "
                     "premultiplied zero in all four bytes, so the layers "
                     "rasterized nothing that would change a frame)")
    if (r.get("shown", 0) + r.get("promoted", 0)) <= 0:
        unmet.append("shown+promoted>0 (rasters were uploaded and none "
                     "reached the screen)")
    if r.get("arrived", -1) != r.get("pictures", 0) + r.get("dropped", 0):
        unmet.append("arrived==pictures+dropped (the arrival balance broke: "
                     "an arrival left by an exit neither counter names, so "
                     "the byte figure now describes a subset)")
    return unmet


def _telemetry_skew_error(what, line, wants):
    """A telemetry line the app wrote and this rig could not parse.

    **This is never a rendering fault and the text must not read like one.**
    Every strict pattern in `WORKER_SIGNAL_PROBE` names a field list, and
    that list belongs to the BUNDLE being driven, not to this file: `inked`
    joined the raster line at 79bad6c7 on 2026-08-31. A rig one commit newer
    than the `squallar-web/pkg` it is pointed at reads null, and null used to
    print as "the line was never written: no overlay raster ever moved" —
    a sentence that is flatly false about a page writing the line every
    frame, and that sent two lanes hunting a regression that did not exist.
    """
    return ("RIG/BUNDLE SKEW — not a rendering fault, and not a network one. "
            "The app WROTE its `%s` telemetry line and THIS RIG COULD NOT "
            "PARSE IT, so every figure it carries reads as absent. Seen: %r. "
            "This rig's pattern wants %s. A telemetry line's field list is a "
            "property of the bundle, so the served squallar-web/pkg is an "
            "older build than this drive.py speaks — the usual cause is "
            "`run_tier2.sh --skip-build` over a pkg/ built before the field "
            "landed, or a worktree whose pkg/ was never rebuilt. REBUILD THE "
            "BUNDLE (run run_tier2.sh without --skip-build) and re-run before "
            "reading this as a regression: this says nothing whatever about "
            "whether the path ran, only that the two halves disagree about "
            "how it reports." % (what, line, wants))


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


# The desktop Firefox capability payload, pinned as the SERIALISED STRING it
# goes onto the wire as -- shape, contents and key order. Its whole job is to
# be the additive proof for the Android work: `firefox_capabilities` now has
# an Android arm, and if adding that arm moved so much as a key on the desktop
# arm, every Firefox figure this rig has ever taken was taken through a
# different browser configuration than the next one will be.
#
# Recorded 2026-08-31 from the builder as it stood BEFORE the Android
# parameters were added. If this ever has to change, the change is a
# deliberate re-pin and every desktop Firefox row predating it is a row from a
# different configuration.
PINNED_DESKTOP_FIREFOX_CAPS = (
    '{"capabilities": {"alwaysMatch": {"browserName": "firefox", '
    '"acceptInsecureCerts": true, "moz:firefoxOptions": '
    '{"binary": "/usr/bin/firefox", '
    '"args": ["-headless", "-width", "1280", "-height", "900"], '
    '"prefs": {"browser.shell.checkDefaultBrowser": false, '
    '"datareporting.policy.dataSubmissionEnabled": false, '
    '"app.update.disabledForTesting": true, '
    '"browser.sessionstore.resume_from_crash": false, '
    '"privacy.reduceTimerPrecision": false}}}}}')

# The Blink Android payload, pinned the same way and for the same reason: the
# dict used to be an inline literal inside `launch()` and is now a function,
# and a pure move has to be provable rather than asserted. Every Android
# figure this campaign holds came out of this exact shape.
PINNED_CHROMIUM_ANDROID_CAPS = (
    '{"capabilities": {"alwaysMatch": {"browserName": "chrome", '
    '"acceptInsecureCerts": true, '
    '"goog:chromeOptions": {"androidPackage": "com.android.chrome", '
    '"androidKeepAppDataDir": true}, '
    '"goog:loggingPrefs": {"browser": "ALL"}}}}')


def selftest_android():
    """The Android mode's only host-side gate.

    THE MODE CANNOT BE SMOKE-TESTED WITHOUT A PHONE, so what is checkable from
    this box is the argument validation and the capability shape -- and those
    are exactly where the defect this closes lived: `--android` refused
    Firefox outright, so every Android figure the campaign holds is Blink's,
    reported as "web".

    Every check below is a COUNT or an IDENTITY. Nothing here times anything.
    """
    fails = []

    def check(cond, msg):
        if not cond:
            fails.append(msg)

    # ---- the defect itself: an Android mode that is one engine ----------
    #
    # The assertion that would have gone red on the tree before this landed.
    # Stated as "firefox is accepted", not as "the accept-list contains
    # firefox": a list the validator no longer consults would satisfy the
    # second and not the first.
    # Asked with an explicitly safe package, so what is under test is the
    # ENGINE and not the daily-driver guard below.
    # One placeholder used to serve both engines here. That stopped being
    # possible when the chromium arm gained a by-name allow-list: a Gecko-
    # shaped package is now correctly refused for Blink, and reusing it would
    # have made this ENGINE check fail for a PACKAGE reason -- which is the
    # confusion the "explicitly safe package" note above exists to avoid. So
    # each engine is asked with a package its own guard permits.
    SAFE_FOR = {"chromium": ANDROID_CHROMIUM_ALLOWED_PACKAGES[0],
                "firefox": "org.example.testbrowser",
                "safari": "org.example.testbrowser"}
    SAFE = SAFE_FOR["firefox"]
    check(validate_android_args("firefox", SAFE, None) is None,
          "validate_android_args refuses --browser firefox with --android. "
          "That is the Blink-only Android column this work exists to end: "
          "Firefox governs the web target and runs ~2x Chromium's service "
          "time on the desktop, so an Android figure taken only on Blink "
          "reports the engine that tends to win and calls it web")
    check(validate_android_args("chromium", SAFE_FOR["chromium"], None) is None,
          "validate_android_args refuses --browser chromium with --android, "
          "which is the path every Android figure already taken came through")
    accepted = [b for b in ("chromium", "firefox", "safari")
                if validate_android_args(b, SAFE_FOR[b], None) is None]
    check(accepted == ["chromium", "firefox"],
          "the set of engines --android drives is %r, not both and only both. "
          "A THIRD engine appearing here without a launch() branch would fail "
          "on the device; a missing one is a silently single-engine column"
          % (accepted,))
    check(validate_android_args("safari", SAFE, None) is not None,
          "--android accepts --browser safari, which has no Android path at "
          "all: safaridriver drives iOS, not Android, and the leg would die "
          "at session creation")

    # ---- adb, and the rest of the refusals ------------------------------
    check(validate_android_args("firefox", None, None, adb_present=False)
          is not None,
          "--android no longer requires adb on PATH, so `adb reverse` fails "
          "after the session is up and 127.0.0.1 on the phone is the phone")

    # geckodriver's own package regex, exercised on both sides. The invalid
    # cases are the ones a human actually types.
    for bad in ("org.mozilla.fire-fox", "firefox", "org..mozilla", ""):
        check(validate_android_args("firefox", bad, None) is not None,
              "--android-package %r is accepted for firefox; geckodriver "
              "answers 'Not a valid androidPackage name' on the device "
              "instead" % (bad,))
    for good in GECKODRIVER_KNOWN_PACKAGES:
        check(ANDROID_PACKAGE_RE.match(good),
              "%r is in this rig's known-package list but fails geckodriver's "
              "own androidPackage regex, so the rig would refuse a package it "
              "advertises" % (good,))
        check(validate_android_args("firefox", good, None,
                                    allow_daily_driver=True) is None,
              "--android-package %r refused for firefox on grounds other than "
              "the daily-driver guard" % (good,))

    check(validate_android_args("firefox", None, "org.mozilla.fenix/Act")
          is not None,
          "--android-activity is allowed to contain '/', which geckodriver "
          "refuses ('androidActivity should not contain /') because it "
          "composes `am start -n <package>/<activity>` itself")
    check(validate_android_args("firefox", None, "IntentReceiverActivity")
          is None,
          "a plain --android-activity is refused for firefox")
    check(validate_android_args("chromium", None, "Whatever") is not None,
          "--android-activity is accepted for chromium, whose Android path "
          "does not take one -- it would be silently dropped")

    # ---- the two engines default to their own browser -------------------
    check(ANDROID_DEFAULT_PACKAGE["chromium"] == "com.android.chrome",
          "the chromium Android default package moved off com.android.chrome; "
          "every Blink Android figure already taken is a figure for that "
          "package")
    check(ANDROID_DEFAULT_PACKAGE["firefox"] == "org.mozilla.firefox_beta",
          "the firefox Android default package is no longer Beta. The default "
          "is what runs when nobody thought about it, and the driver wipes "
          "whatever it drives -- defaulting to release Firefox is defaulting "
          "to deleting somebody's tabs, logins and bookmarks")
    check(sorted(ANDROID_DEFAULT_PACKAGE) == sorted(ANDROID_BROWSERS),
          "an engine --android accepts has no default package, so it would "
          "KeyError in launch() instead of driving")

    # ---- the rig refuses to wipe somebody's browser ---------------------
    #
    # THE MOST IMPORTANT CHECKS IN THIS FILE. Both drivers run
    # `pm clear <package>` before every session -- MEASURED on geckodriver
    # 0.37.1, which answered "Success" against a real phone and left the user
    # at Firefox's first-run wizard with their tabs, logins and bookmarks
    # gone. Nothing in the rig can stop the clear, so the only defence is
    # refusing the package, and a refusal that can be forgotten is not one.
    for daily in ANDROID_DAILY_DRIVER_PACKAGES:
        for br in ANDROID_BROWSERS:
            err = validate_android_args(br, daily, None)
            check(err is not None and "REFUS" in err,
                  "--android-package %s is accepted for %s without "
                  "--android-allow-daily-driver. That package is somebody's "
                  "daily browser and the driver deletes its data before the "
                  "session starts" % (daily, br))
            check(validate_android_args(br, daily, None,
                                        allow_daily_driver=True) is None,
                  "--android-allow-daily-driver does not actually unlock %s "
                  "for %s, so the escape hatch is unusable and somebody will "
                  "delete the guard instead" % (daily, br))
    # The DEFAULT is the case that matters: it is what runs when nobody chose,
    # and nobody choosing is how two browsers were wiped in one day.
    check(validate_android_args("chromium", None, None) is not None,
          "--android --browser chromium with NO package now silently defaults "
          "to com.android.chrome and wipes it. The default must be refused "
          "too, or the guard only protects people who were already thinking "
          "about it")
    check(validate_android_args("firefox", None, None) is None,
          "the default firefox Android package is refused, so the safe path "
          "is the one that needs a flag -- which inverts the guard")
    for safe in ("org.mozilla.firefox_beta", "org.mozilla.fenix"):
        check(validate_android_args("firefox", safe, None) is None,
              "%s is refused. It is a SEPARATE INSTALL with separate storage "
              "and is the whole recommendation; refusing it leaves no safe "
              "package at all" % safe)
        check(safe not in ANDROID_DAILY_DRIVER_PACKAGES,
              "%s is on the daily-driver list, which would leave the rig with "
              "nothing it is allowed to drive" % safe)

    # ---- onboarding dismissal, and the case it must FAIL ----------------
    #
    # THE NON-TRIVIALITY FLOOR IS THE POINT. A dismissal that "succeeds" by
    # doing nothing would navigate into the wizard and produce a row for an
    # app that never ran -- a measurement of the onboarding screen, reported
    # as a measurement of squallar. So the case with nothing to tap AND no
    # browser has to raise, and that is checked before anything else here.
    PKG = "org.mozilla.firefox_beta"

    def _node(**a):
        return "<node " + " ".join('%s="%s"' % (k.replace("_", "-"), v)
                                   for k, v in a.items()) + "/>"

    def _hier(*nodes):
        return "<?xml version='1.0'?><hierarchy rotation='0'>%s</hierarchy>" \
               % "".join(nodes)

    onboarding_xml = _hier(_node(
        package=PKG, resource_id="%s:id/primary_button" % PKG,
        text="Start browsing", clickable="true", enabled="true",
        bounds="[100,1800][980,1950]"))
    ready_xml = _hier(_node(
        package=PKG,
        resource_id="%s:id/mozac_browser_toolbar_url_view" % PKG,
        text="", clickable="true", enabled="true", bounds="[0,100][1080,220]"))
    # Neither a wizard nor a browser: a lock screen, a crash dialog, a launcher.
    stuck_xml = _hier(_node(
        package="com.android.systemui", resource_id="com.android.systemui:id/x",
        text="", clickable="false", enabled="true", bounds="[0,0][1080,2400]"))

    st = android_parse_hierarchy(stuck_xml, PKG)
    check(st["reachable"] is False and st["targets"] == [],
          "a hierarchy that is neither the wizard nor the browser is being "
          "classified as actionable or as reachable; it is neither")
    raised = None
    try:
        android_dismiss_onboarding(PKG, _dump=lambda: stuck_xml,
                                   _tap=lambda x, y: None)
    except WebDriverError as e:
        raised = str(e)
    check(raised is not None and "did not converge" in raised,
          "THE FLOOR IS GONE: onboarding dismissal returns success when there "
          "is nothing to dismiss and no browser on screen. It would then "
          "navigate into the wizard and every figure on the row would "
          "describe the onboarding screen rather than the app")
    # And it must fail the same way when the dump is unusable, rather than
    # treating an unparseable hierarchy as "nothing to do".
    raised = None
    try:
        android_dismiss_onboarding(PKG, _dump=lambda: "not xml at all",
                                   _tap=lambda x, y: None)
    except WebDriverError as e:
        raised = str(e)
    check(raised is not None,
          "an unparseable uiautomator dump is treated as a clean screen, so a "
          "broken instrument reads as a dismissed wizard")

    # Already at the browser: zero taps, and it must not invent one.
    taps = []
    got = android_dismiss_onboarding(PKG, _dump=lambda: ready_xml,
                                     _tap=lambda x, y: taps.append((x, y)))
    check(got["ok"] and got["rounds"] == 0 and taps == [],
          "the dismissal taps something when the browser is ALREADY on "
          "screen (%r); a stray tap on a live page is an input event inside "
          "somebody's measurement window" % (taps,))

    # The wizard, then the browser: exactly one tap, at the node's own centre.
    seq = [onboarding_xml, ready_xml]
    taps = []
    got = android_dismiss_onboarding(
        PKG, _dump=lambda: seq.pop(0), _tap=lambda x, y: taps.append((x, y)))
    check(got["ok"] and taps == [(540, 1875)],
          "the dismissal did not tap the onboarding button's own centre: "
          "expected one tap at (540, 1875) computed from bounds "
          "[100,1800][980,1950], got %r. A tap placed anywhere else is a "
          "guess, and a guess breaks on a different density, theme, locale "
          "or fold" % (taps,))
    # Ordering is only testable when BOTH kinds are on the same screen -- a
    # single id-matched node would satisfy any ordering, which is how the
    # first version of this check passed its own tamper.
    both_xml = _hier(
        _node(package=PKG, resource_id="%s:id/nope" % PKG, text="Get started",
              clickable="true", enabled="true", bounds="[0,0][100,100]"),
        _node(package=PKG, resource_id="%s:id/primary_button" % PKG,
              text="", clickable="true", enabled="true",
              bounds="[100,1800][980,1950]"))
    both = android_parse_hierarchy(both_xml, PKG)
    check(both["targets"] and
          both["targets"][0]["matched"].startswith("onboarding-id:"),
          "with an id-matched control and an English-text-matched control on "
          "the same screen, the TEXT one is being tapped first (%r). Identity "
          "beats a language guess: the text list is a fallback for builds "
          "with no ids, and preferring it makes every non-English device tap "
          "the wrong thing"
          % ([t["matched"] for t in both["targets"]],))
    # And a permission dialog is in FRONT of the wizard, so it outranks both.
    perm_xml = _hier(
        _node(package=PKG, resource_id="%s:id/primary_button" % PKG, text="",
              clickable="true", enabled="true", bounds="[100,1800][980,1950]"),
        _node(package="com.android.permissioncontroller",
              resource_id="com.android.permissioncontroller:id/"
                          "permission_deny_button",
              text="Don't allow", clickable="true", enabled="true",
              bounds="[60,1700][500,1800]"))
    perm = android_parse_hierarchy(perm_xml, PKG)
    check(perm["targets"] and
          perm["targets"][0]["matched"].startswith("system-decline:"),
          "a system dialog sitting in front of the wizard is not "
          "handled first (%r); taps aimed at the wizard behind it land on the "
          "dialog and the flow never advances"
          % ([t["matched"] for t in perm["targets"]],))

    # The bound is real: a wizard that never ends stops, it does not spin.
    raised = None
    try:
        android_dismiss_onboarding(PKG, max_rounds=3,
                                   _dump=lambda: onboarding_xml,
                                   _tap=lambda x, y: None)
    except WebDriverError as e:
        raised = str(e)
    check(raised is not None and "bound" in raised,
          "an onboarding flow that never completes loops forever instead of "
          "failing; the leg would hang rather than report")

    # ---- the shape Fenix 155 actually has -------------------------------
    #
    # NOT INVENTED. This fixture is trimmed from a real `uiautomator dump` of
    # Firefox Beta 155's first-run screen on the SM-F966U: the button carries
    # no resource-id and no text of its own, the visible "Continue" is a
    # separate non-clickable child, and three OTHER clickable nodes on the
    # same screen hold long legal content-descs. A parser that reads only a
    # node's own text finds nothing here -- which is what the first version
    # did, and it failed on the device.
    fenix155 = (
        "<?xml version='1.0'?><hierarchy rotation='0'>"
        "<node package='{p}' resource-id='{p}:id/rootContainer'"
        " clickable='false' text='' content-desc='' bounds='[0,0][1968,2184]'>"
        "<node package='{p}' text='Welcome to Firefox' clickable='false'"
        " enabled='true' bounds='[717,977][1220,1046]'/>"
        "<node package='{p}' text='' content-desc='By continuing, you agree to"
        " the Firefox Terms of Use' clickable='true' enabled='true'"
        " bounds='[470,1268][1380,1393]'/>"
        "<node package='{p}' text='' content-desc='Firefox cares about your"
        " privacy' clickable='true' enabled='true'"
        " bounds='[470,1393][1466,1519]'/>"
        "<node package='{p}' clickable='true' enabled='true' text=''"
        " content-desc='' bounds='[459,1713][1509,1839]'>"
        "<node package='{p}' text='Continue' clickable='false' enabled='true'"
        " bounds='[895,1750][1074,1802]'/>"
        "</node></node></hierarchy>").format(p=PKG)
    f = android_parse_hierarchy(fenix155, PKG)
    check(f["reachable"] is False,
          "the Fenix 155 onboarding screen is being read as 'the browser is "
          "reachable'; the leg would navigate straight into the wizard")
    check(f["targets"],
          "NOTHING MATCHES on the real Fenix 155 onboarding screen. Its "
          "buttons carry no resource-id and no text of their own -- the label "
          "is a non-clickable CHILD -- so a parser that reads only each "
          "node's own text finds nothing to tap and the leg cannot start")
    if f["targets"]:
        top = f["targets"][0]
        check(top["center"] == (984, 1776),
              "the wrong control is first on the real Fenix 155 screen: "
              "expected the Continue button's centre (984, 1776) from bounds "
              "[459,1713][1509,1839], got %r via %r. The other clickable "
              "nodes on that screen are the Terms and privacy links -- "
              "tapping one opens a legal notice and the wizard never advances"
              % (top["center"], top["matched"]))
        check("continue" in top["matched"],
              "the first target on the real screen was matched as %r rather "
              "than by the button's own label" % top["matched"])
    # A container that merely ENCLOSES the button also collects its label.
    # The tight box is the control; the enclosing one is the screen.
    nested = (
        "<?xml version='1.0'?><hierarchy rotation='0'>"
        "<node package='{p}' clickable='true' enabled='true' text=''"
        " content-desc='' bounds='[0,0][1968,2184]'>"
        "<node package='{p}' clickable='true' enabled='true' text=''"
        " content-desc='' bounds='[459,1713][1509,1839]'>"
        "<node package='{p}' text='Continue' clickable='false' enabled='true'"
        " bounds='[895,1750][1074,1802]'/>"
        "</node></node></hierarchy>").format(p=PKG)
    nst = android_parse_hierarchy(nested, PKG)
    check(nst["targets"] and nst["targets"][0]["center"] == (984, 1776),
          "a full-screen container that merely encloses the button is being "
          "tapped instead of the button (%r). Its centre is the middle of the "
          "screen, which is not a control"
          % ([t["center"] for t in nst["targets"]],))

    # ---- the rig never changes the device -------------------------------
    #
    # MEASURED, trimmed from a real dump: after the welcome screen Fenix
    # raises the PLATFORM default-browser chooser, which lists the user's
    # actual Chrome as current default and their release Firefox beside it.
    # Its positive button is "Set as default" (android:id/button1) and its
    # negative is "Cancel" (android:id/button2). Tapping the positive one
    # would repoint the whole phone's browser association -- a settings change
    # nobody asked for, on somebody's personal device.
    default_chooser = (
        "<?xml version='1.0'?><hierarchy rotation='0'>"
        "<node package='com.google.android.permissioncontroller'"
        " resource-id='com.android.permissioncontroller:id/title'"
        " clickable='false' text='Set Firefox Beta as your default browser'"
        " bounds='[522,1203][1446,1339]'/>"
        "<node package='com.google.android.permissioncontroller'"
        " resource-id='android:id/button2' clickable='true' enabled='true'"
        " text='Cancel' bounds='[522,1976][903,2071]'/>"
        "<node package='com.google.android.permissioncontroller'"
        " resource-id='android:id/button1' clickable='true' enabled='true'"
        " text='Set as default' bounds='[906,1976][1446,2071]'/>"
        "</hierarchy>")
    dc = android_parse_hierarchy(default_chooser, PKG)
    picked = [t["center"] for t in dc["targets"]]
    check((712, 2023) in picked,
          "the default-browser chooser's Cancel button (centre (712, 2023) "
          "from bounds [522,1976][903,2071]) is not a target, so the flow "
          "stalls in front of a system dialog it cannot dismiss. Targets: %r"
          % (dc["targets"],))
    check((1176, 2023) not in picked,
          "THE RIG IS ABOUT TO CHANGE THE USER'S DEVICE: 'Set as default' at "
          "(1176, 2023) is a tap candidate. That button repoints the phone's "
          "default browser away from whatever the owner chose. It must be "
          "refused however it matches, not merely ranked below Cancel")
    check(any("set as default" in (r.get("text") or "").lower()
              for r in (dc.get("refused") or [])),
          "'Set as default' was dropped silently rather than RECORDED as "
          "refused; a control the rig declines to touch is a fact the row "
          "should carry, not an absence")
    check(dc["targets"] and dc["targets"][0]["center"] == (712, 2023),
          "Cancel is not the FIRST target on the default-browser chooser, so "
          "some other control on a system dialog gets tapped first")

    # ---- loaded is not displayed ----------------------------------------
    #
    # The exact reading taken off the device when Fenix held its home screen
    # in front of the content view: the page had BOOTED, coi was true and the
    # rayon pool had 8 threads, and the canvas was the untouched HTML default.
    # Every existing check passed on that state.
    never_shown = {"booted": True, "hasCanvas": True, "readyState": "complete",
                   "clientWidth": 0, "clientHeight": 0,
                   "bufferWidth": 300, "bufferHeight": 150}
    displayed = {"booted": True, "hasCanvas": True, "readyState": "complete",
                 "clientWidth": 852, "clientHeight": 818,
                 "bufferWidth": 2557, "bufferHeight": 2453}
    raised = None
    try:
        verify_android_content_view(None, timeout=0.4,
                                    probe=lambda: never_shown)
    except WebDriverError as e:
        raised = str(e)
    check(raised is not None and "never DISPLAYED" in raised,
          "a page that loaded but was NEVER PUT ON SCREEN is accepted. That "
          "exact state was measured on Fenix 155 -- booted true, coi true, 8 "
          "rayon threads, canvas 0x0 at the default 300x150 buffer -- and it "
          "passed every other check this rig has. The whole leg's frame "
          "figures would describe a viewport nobody was shown")
    got = verify_android_content_view(None, timeout=5.0,
                                      probe=lambda: displayed)
    check(got["ok"] and got["client"] == [852, 818],
          "a genuinely displayed content view is being rejected (%r), which "
          "would fail every Android leg instead of only the broken ones"
          % (got,))
    # The buffer floor matters on its own: a canvas can report a css size
    # while its drawing buffer is still the untouched 300x150 default.
    raised = None
    try:
        verify_android_content_view(
            None, timeout=0.4,
            probe=lambda: dict(never_shown, clientWidth=852, clientHeight=818))
    except WebDriverError as e:
        raised = str(e)
    check(raised is not None,
          "a canvas with a css size but the DEFAULT 300x150 drawing buffer is "
          "accepted as displayed; the buffer is what is rasterized and what "
          "every byte figure on the row is a figure for")

    # ---- "jump back in": the last step, and the user's own route ---------
    #
    # Trimmed from a real dump of the state Fenix leaves after navigate: the
    # page is loaded, the home screen is in front of it, and the recent-tabs
    # card carries OUR url. NOT ONE of these nodes sets clickable="true" --
    # they are Compose semantics nodes -- so the filter that finds every
    # onboarding button finds none of them. That is the trap this fixture
    # exists to keep sprung.
    home_after_nav = (
        "<?xml version='1.0'?><hierarchy rotation='0'>"
        "<node package='{p}' resource-id='{p}:id/homepageView' clickable='false'"
        " text='' bounds='[0,0][1968,2184]'/>"
        "<node package='{p}' resource-id='private.browsing.homepage.button'"
        " clickable='true' enabled='true' text='' content-desc='Private browsing'"
        " bounds='[42,300][194,426]'/>"
        "<node package='{p}' resource-id='' clickable='false'"
        " text='Jump back in' bounds='[42,1123][1596,1195]'/>"
        "<node package='{p}' resource-id='recent.tabs' clickable='false'"
        " text='' bounds='[42,1264][1926,1496]'/>"
        "<node package='{p}' resource-id='recent.tab.title' clickable='false'"
        " text='Squallar' bounds='[358,1307][525,1381]'/>"
        "<node package='{p}' resource-id='recent.tab.url' clickable='false'"
        " text='http://127.0.0.1:33319/index' bounds='[426,1381][1075,1454]'/>"
        "<node package='{p}' resource-id='ADDRESSBAR_TABS_COUNTER'"
        " clickable='true' enabled='true' text='' bounds='[1653,110][1779,236]'/>"
        "</hierarchy>").format(p=PKG)
    tabhit = android_find_loaded_tab(home_after_nav, "127.0.0.1:33319")
    check(tabhit is not None,
          "the 'Jump back in' recent-tab card is not found on the real "
          "post-navigate home screen. None of its nodes set "
          "clickable='true' -- they are Compose semantics nodes -- so any "
          "lookup that filters on clickable finds nothing and the leg can "
          "never be shown the page it loaded")
    if tabhit:
        check(tabhit["url_verified"] is True,
              "the recent tab was found but NOT verified against the served "
              "url, so the rig would happily surface somebody else's tab and "
              "measure whatever it contains (%r)" % (tabhit,))
        check(tabhit["center"] == (750, 1417),
              "the tap is not on the url line of the card carrying our own "
              "url: expected (750, 1417) from bounds [426,1381][1075,1454], "
              "got %r" % (tabhit["center"],))
    # A different tab must NOT be url-verified -- that flag is the whole
    # difference between surfacing our page and surfacing a stranger's.
    other = android_find_loaded_tab(home_after_nav, "127.0.0.1:99999")
    check(other is not None and other["url_verified"] is False,
          "a recent tab whose url does not match the served one is being "
          "reported as verified (%r); the flag would then mean nothing"
          % (other,))
    # No card at all -> None, so the caller raises rather than tapping blind.
    check(android_find_loaded_tab(
        "<?xml version='1.0'?><hierarchy><node resource-id='x' "
        "bounds='[0,0][10,10]'/></hierarchy>", "127.0.0.1:1") is None,
          "a home screen with no recent-tab card returns a tap target anyway; "
          "the rig would tap a coordinate it did not derive from anything")
    raised = None
    try:
        android_surface_loaded_tab(
            "http://127.0.0.1:1/x",
            _dump=lambda: "<?xml version='1.0'?><hierarchy/>",
            _tap=lambda x, y: None)
    except WebDriverError as e:
        raised = str(e)
    check(raised is not None and "never displayed" in raised.lower(),
          "surfacing a tab that is not there SUCCEEDS, so a leg whose page "
          "was never shown goes on to print a full set of frame figures for "
          "a viewport nobody saw")
    taps = []
    surf = android_surface_loaded_tab("http://127.0.0.1:33319/index-rig.html",
                                      _dump=lambda: home_after_nav,
                                      _tap=lambda x, y: taps.append((x, y)))
    check(surf["ok"] and taps == [(750, 1417)] and surf["url_verified"],
          "surfacing the loaded tab did not tap the url-verified card's own "
          "centre: %r" % (taps,))
    # And the private-browsing button, which is the one clickable control
    # sitting right beside it, must never be a candidate anywhere.
    hp = android_parse_hierarchy(home_after_nav, PKG)
    check(all("private" not in (t.get("text") or "").lower()
              for t in hp["targets"]),
          "the private-browsing button is a tap candidate. A private window "
          "has a different storage partition and no persistence -- it is a "
          "different browser, and it would be measured under this one's name")

    # Bounds arithmetic, including the inputs that must be refused.
    check(_uia_bounds_center("[0,100][200,300]") == (100, 200),
          "bounds centre arithmetic is wrong")
    for bad in ("[0,0][0,0]", "[10,10][5,5]", "garbage", "", None):
        check(_uia_bounds_center(bad) is None,
              "%r is accepted as a tappable bounds; a zero or inverted "
              "rectangle is not a place" % (bad,))

    # ---- the Gecko capability shape -------------------------------------
    ff = firefox_capabilities(None, (1280, 900), headless=False,
                              android_package="org.mozilla.firefox")
    opts = ff["capabilities"]["alwaysMatch"]["moz:firefoxOptions"]
    check(opts.get("androidPackage") == "org.mozilla.firefox",
          "the firefox Android capabilities carry no androidPackage, so "
          "geckodriver would look for a desktop binary")
    # THE ONE THAT FAILS ON THE PHONE. Read verbatim out of the pinned
    # geckodriver 0.37.1: "androidPackage and binary are mutual exclusive".
    check("binary" not in opts,
          "the firefox Android capabilities still carry `binary`; the pinned "
          "geckodriver rejects the session with 'androidPackage and binary "
          "are mutual exclusive' and the leg dies three minutes in, on the "
          "device, having measured nothing")
    check(opts.get("args") == [],
          "the firefox Android capabilities carry window/headless args (%r). "
          "A phone has neither a window size nor a headless mode, and "
          "-headless on Android is not a thing geckodriver passes through"
          % (opts.get("args"),))
    check(opts.get("prefs", {}).get("privacy.reduceTimerPrecision") is False,
          "the firefox Android capabilities lost the timer-precision "
          "override, so every rAF delta comes back quantised to 1 ms and the "
          "percentiles are integers -- unusable, and silently so")
    check("androidActivity" not in opts and "androidDeviceSerial" not in opts,
          "the firefox Android capabilities carry an activity or a serial "
          "that nobody asked for; geckodriver's own default activity is what "
          "a release Firefox needs")
    ff2 = firefox_capabilities(None, (1280, 900), headless=False,
                               android_package="org.example.custom",
                               android_activity="MyActivity",
                               android_serial="RFCY61M5WAT")
    opts2 = ff2["capabilities"]["alwaysMatch"]["moz:firefoxOptions"]
    check(opts2.get("androidActivity") == "MyActivity"
          and opts2.get("androidDeviceSerial") == "RFCY61M5WAT",
          "--android-activity / --adb-serial do not reach "
          "moz:firefoxOptions, so a multi-device host drives whichever phone "
          "adb happens to list first")

    # ---- the additive proof ---------------------------------------------
    got = json.dumps(firefox_capabilities("/usr/bin/firefox", (1280, 900)))
    check(got == PINNED_DESKTOP_FIREFOX_CAPS,
          "the DESKTOP firefox capability payload changed shape:\n"
          "  pinned %s\n  got    %s\nAdding the Android arm was supposed to "
          "be additive; a moved key here means every desktop Firefox row this "
          "rig has taken was taken through a different configuration than the "
          "next one will be" % (PINNED_DESKTOP_FIREFOX_CAPS, got))
    # ---- Blink on Android does not wipe the browser -------------------
    #
    # `androidKeepAppDataDir` is the capability that stops chromedriver's
    # `pm clear` -- the literal string sits beside `|shell:` in the driver
    # binary. Without it, every Blink Android row is taken on a cold profile,
    # cold HTTP cache and cold service worker, which is an unstated
    # denominator and not what any user's browser looks like.
    # Spelled against the REAL authorised package rather than a stand-in:
    # com.chrome.beta is the one package this path may drive, so it is the one
    # the capability shape has to be right for.
    cao = (chromium_android_capabilities(ANDROID_CHROMIUM_ALLOWED_PACKAGES[0])
           ["capabilities"]["alwaysMatch"]["goog:chromeOptions"])
    check(cao["androidPackage"] == "com.chrome.beta",
          "the chromium Android capability builder no longer puts the "
          "requested package in androidPackage, so the rig would drive "
          "whatever chromedriver defaults to rather than what it was asked "
          "for -- and what it was asked for is the only package authorised")
    check(cao.get("androidKeepAppDataDir") is True,
          "the chromium Android capabilities no longer carry "
          "androidKeepAppDataDir, so chromedriver wipes the browser it drives "
          "before every session. That cost this project's user their Chrome "
          "once, and it silently makes every row a cold-profile row")
    check("androidUseRunningApp" not in cao,
          "androidUseRunningApp is on by DEFAULT. It attaches to a running "
          "app and never starts one, so a default-on capability turns every "
          "leg on a closed browser into an unexplained session failure")
    cao2 = (chromium_android_capabilities("com.chrome.beta",
                                          use_running_app=True)
            ["capabilities"]["alwaysMatch"]["goog:chromeOptions"])
    check(cao2.get("androidUseRunningApp") is True
          and cao2.get("androidKeepAppDataDir") is True,
          "--android-use-running-app does not reach goog:chromeOptions, or "
          "turns off the keep-data capability while doing it")
    # (that the rig LAUNCHES the package before attaching is a cross-file
    # property and is pinned from the Rust side, in rig_android_engines.rs)
    #
    # ---- and the OTHER branch actually exists ---------------------------
    #
    # `profile_state` shipped as a conditional whose test was the constant
    # true, so it could not take its other branch: a cleared-profile row would
    # have described itself as a preserved one. The opt-out is what makes the
    # field able to be wrong, which is what makes it worth printing.
    cao3 = (chromium_android_capabilities("com.chrome.beta",
                                          keep_app_data=False)
            ["capabilities"]["alwaysMatch"]["goog:chromeOptions"])
    check("androidKeepAppDataDir" not in cao3,
          "--android-clear-app-data still sends androidKeepAppDataDir, so the "
          "run that is supposed to start from a cleared profile starts from a "
          "preserved one and the row's profile_state is a lie in the safe-"
          "sounding direction")

    # ---- the Blink Android path drives ONE package ----------------------
    #
    # Before 2026-08-31 the chromium arm refused only the names on the
    # daily-driver list, which meant com.chrome.dev, com.chrome.canary and
    # every typo were all accepted -- a far wider permission than anybody
    # granted, on a path whose driver deletes the app's data every session.
    check(validate_android_args("chromium", "com.chrome.beta", None) is None,
          "com.chrome.beta is refused for chromium. It is the ONE package the "
          "user installed for measurement and authorised the rig to wipe; "
          "refusing it leaves the Blink Android path with nothing it may "
          "drive at all, which is the state that blocked it before")
    for stranger in ("com.chrome.dev", "com.chrome.canary",
                     "com.example.whatever"):
        err = validate_android_args("chromium", stranger, None)
        check(err is not None and "REFUS" in err,
              "--android-package %s is accepted for chromium. The "
              "authorisation to let chromedriver wipe a browser covers "
              "com.chrome.beta and nothing else, and an unrecognised Chrome "
              "package is somebody's browser until proven otherwise"
              % (stranger,))
    # The release-Chrome refusal is NOT the allow-list's doing and must keep
    # its own message: "not on a list" does not tell somebody what it cost.
    err = validate_android_args("chromium", "com.android.chrome", None)
    check(err is not None and "daily browser" in err,
          "com.android.chrome no longer gets its own daily-driver refusal -- "
          "it is falling through to the generic allow-list message, which "
          "does not say that driving it deletes somebody's tabs, logins and "
          "history, or that it already happened once")
    # The deliberate wipe is fenced by NAME, so the two flags cannot combine
    # into a wipe of a package nobody authorised.
    err = validate_android_args("chromium", "com.android.chrome", None,
                                allow_daily_driver=True, clear_app_data=True)
    check(err is not None and "REFUSING --android-clear-app-data" in err,
          "--android-allow-daily-driver + --android-clear-app-data aims a "
          "DELIBERATE wipe at release Chrome. The escape hatch unlocks "
          "driving a package; it must not unlock deleting one that was never "
          "authorised")
    check(validate_android_args("chromium", "com.chrome.beta", None,
                                clear_app_data=True) is None,
          "--android-clear-app-data is refused for com.chrome.beta, which is "
          "the one package it is authorised for -- so the measurement posture "
          "the campaign asked for cannot be run at all")

    got = json.dumps(chromium_android_capabilities("com.android.chrome"))
    check(got == PINNED_CHROMIUM_ANDROID_CAPS,
          "the chromium Android capability payload changed when it was moved "
          "out of launch() into a function:\n  pinned %s\n  got    %s"
          % (PINNED_CHROMIUM_ANDROID_CAPS, got))
    return fails


def selftest_adapters():
    """The hardware floor over both adapter probes, offline.

    The case that matters is the WebGPU arm: no WebGL context at all beside a
    real WebGPU adapter must clear the floor via `webgpu`; and every way the
    floor used to fail must still fail -- software WebGL with no WebGPU
    adapter, no WebGL with `requestAdapter -> null`, and a WebGPU adapter
    that is itself software (the fallback flag, or a SwiftShader name)."""
    fails = []

    def check(cond, msg):
        if not cond:
            fails.append("adapters: " + msg)

    gl_hw = {"webgl": "webgl2", "gl_vendor": "NVIDIA Corporation",
             "gl_renderer": "NVIDIA GeForce RTX 3090/PCIe/SSE2"}
    gl_sw = {"webgl": "webgl2", "gl_vendor": "Google Inc. (Google)",
             "gl_renderer": "ANGLE (Google, Vulkan 1.3.0 (SwiftShader Device "
                            "(Subzero) (0x0000C0DE)), SwiftShader driver)"}
    gl_none = {"webgl": None}
    wg_hw = {"gpu_object": True, "adapter": True,
             "adapter_info": {"vendor": "nvidia", "architecture": "ampere",
                              "device": "", "description": ""},
             "adapter_is_fallback": False,
             "adapter_limits": {"maxTextureDimension2D": 16384}}
    wg_null = {"gpu_object": True, "adapter": None,
               "error": "requestAdapter timed out after 15000 ms"}
    wg_absent = {"gpu_object": False, "adapter": None}
    wg_fallback = dict(wg_hw, adapter_is_fallback=True)
    wg_swift = dict(wg_hw, adapter_is_fallback=None,
                    adapter_info={"vendor": "google",
                                  "architecture": "swiftshader",
                                  "device": "", "description": ""})
    wg_probe_err = {"probe_error": "script timeout"}

    def floor(env, wg):
        return hardware_floor(classify_adapter(env),
                              classify_webgpu_adapter(wg))

    check(classify_adapter(gl_hw)["class"] == "hardware",
          "WebGL nvidia not hardware")
    check(classify_adapter(gl_sw)["class"] == "software",
          "WebGL SwiftShader not software")
    check(classify_adapter(gl_none)["class"] == "none",
          "no WebGL context not none")
    check(classify_webgpu_adapter(wg_hw)["class"] == "hardware",
          "WebGPU nvidia not hardware")
    check(classify_webgpu_adapter(wg_fallback)["class"] == "software",
          "fallback WebGPU adapter not software")
    check(classify_webgpu_adapter(wg_swift)["class"] == "software",
          "SwiftShader WebGPU adapter not software")
    check(classify_webgpu_adapter(wg_null)["class"] == "none",
          "requestAdapter -> null not none")
    check(classify_webgpu_adapter(wg_absent)["class"] == "none",
          "no navigator.gpu not none")
    check(classify_webgpu_adapter(wg_probe_err)["class"] == "none",
          "probe error not none")
    check(classify_webgpu_adapter(None)["class"] == "none",
          "missing probe not none")

    # The WebGL2 arm, unchanged: a hardware WebGL renderer clears the floor
    # whatever WebGPU says; a software or absent one beside no WebGPU adapter
    # does not.
    check(floor(gl_hw, wg_absent) == "webgl",
          "WebGL hardware alone should clear via webgl")
    check(floor(gl_hw, wg_hw) == "webgl", "both hardware should name webgl")
    check(floor(gl_sw, wg_absent) is None,
          "software WebGL, no WebGPU must fail")
    check(floor(gl_sw, wg_null) is None,
          "software WebGL, requestAdapter null must fail")
    check(floor(gl_none, wg_absent) is None,
          "no WebGL, no navigator.gpu must fail")
    check(floor(gl_none, wg_null) is None,
          "no WebGL, requestAdapter null must fail")
    check(floor(gl_none, wg_probe_err) is None,
          "no WebGL, WebGPU probe error must fail")
    # The WebGPU arm: the page has no WebGL2 context by design and a real
    # adapter answers requestAdapter().
    check(floor(gl_none, wg_hw) == "webgpu",
          "no WebGL beside a hardware WebGPU adapter should clear via webgpu")
    # ...and a software WebGPU adapter is not a way past the floor.
    check(floor(gl_none, wg_fallback) is None,
          "fallback WebGPU adapter must not clear the floor")
    check(floor(gl_none, wg_swift) is None,
          "SwiftShader WebGPU adapter must not clear the floor")
    check(floor(gl_sw, wg_fallback) is None,
          "software WebGL + fallback WebGPU must fail")

    # The failure message names both adapters it looked at.
    lbl_gl = adapter_label(classify_adapter(gl_none))
    lbl_wg = webgpu_adapter_label(classify_webgpu_adapter(wg_null))
    check(lbl_gl.startswith("none("), "WebGL none label: %r" % lbl_gl)
    check("requestAdapter -> null" in lbl_wg, "WebGPU null label: %r" % lbl_wg)
    lbl_hw = webgpu_adapter_label(classify_webgpu_adapter(wg_hw))
    check(lbl_hw == "hardware:nvidia (maxTex2D=16384)",
          "WebGPU hardware label: %r" % lbl_hw)
    lbl_sw = webgpu_adapter_label(classify_webgpu_adapter(wg_swift))
    check(lbl_sw == "software:google (maxTex2D=16384) [swiftshader]",
          "WebGPU software label: %r" % lbl_sw)
    return fails


def selftest():
    failures = []
    failures += selftest_android()
    failures += selftest_adapters()
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
    icons_dir = "/home/reddragon/projects/squallar/squallar-web/icons"
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
        # Beside the tag, because it is the tag's missing half. The tag says
        # WHICH leg this is; the run id says WHICH RUN OF IT, and only the
        # second one can distinguish this file from the one the last run left
        # behind under the same name.
        "run_id": args.run_id,
        # First field after the tag on purpose. Every cap below is a property
        # of the adapter this arm reached, and a reader who does not know
        # which arm ran cannot use any of them.
        "arm": args.arm,
        "invocation": sys.argv, "started_utc": time.strftime(
            "%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "stages": [], "gotchas": [],
        # HOST LOAD BELONGS IN THE ARTIFACT, not in whatever shell wrapper
        # happened to run the leg. `native_row.py` has always carried
        # loadavg_start/end/max; the web path carried nothing, so a web row
        # could not say afterwards whether the box was busy underneath it --
        # and a sibling lane lost a whole scene's rows to load variance it
        # could not attribute, precisely because the samples lived only in a
        # terminal it had since closed. Sampled here rather than by the
        # caller so EVERY web leg has it, including ones run by hand.
        "host_loadavg": {"start": _loadavg(), "end": None, "max": None,
                         "samples": 1},
    }

    def note_load():
        """Sample host load into the running max. Called wherever the leg
        already pauses, so it costs nothing extra."""
        hl = result["host_loadavg"]
        v = _loadavg()
        if v is None:
            return
        hl["end"] = v
        hl["samples"] += 1
        hl["max"] = v if hl["max"] is None else max(hl["max"], v)
        if hl["start"] is not None:
            hl["max"] = max(hl["max"], hl["start"])

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
        if args.android and args.browser == "firefox":
            # BEFORE the app launches, so the first page load already has it.
            # Fenced to non-daily-driver packages inside the function.
            result["location_grant"] = android_grant_location(
                args.android_package, args.adb_serial)
            stage("grant-location", **{
                k: v for k, v in result["location_grant"].items()
                if k != "package"})

        stage("launch-driver")
        driver, session, info = launch(
            args.browser, out_dir, tag, driver_path=args.driver,
            binary=args.binary, window=window, headless=not args.headed,
            tmp_root=tmp_root, ff_prefs=parse_kv(args.ff_pref, json_values=True),
            extra_env=parse_kv(args.env), ff_mode=args.ff_mode, arm=args.arm,
            display=args.display, chromium_args=args.chromium_arg,
            android=args.android, android_package=args.android_package,
            android_activity=args.android_activity,
            android_serial=args.adb_serial,
            android_use_running_app=args.android_use_running_app,
            android_keep_app_data=not args.android_clear_app_data)
        if args.android:
            # Before navigate, or 127.0.0.1 on the phone is the phone.
            port_ = urllib.parse.urlsplit(args.url).port
            if port_ is None:
                port_ = 443 if args.url.startswith("https") else 80
            stage("adb-reverse", port=port_)
            result["adb_reverse"] = adb_reverse(port_, args.adb_serial)
            # Both ends of the leg, so the delta is a column rather than a
            # guess. Read-only; see android_device_state.
            result["device_before"] = android_device_state(args.adb_serial)
            stage("device-before", **result["device_before"])
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
        # A WINDOW IS A DESKTOP CONCEPT. On a phone the browser owns the whole
        # display, there is nothing to resize, and asking anyway is the rig
        # requesting something that cannot mean anything. geckodriver says so
        # out loud -- "unsupported operation: Only supported in desktop
        # applications", HTTP 500 -- while chromedriver accepts the call and
        # silently ignores it, which is why the Blink Android legs never
        # surfaced this and the first Gecko leg did.
        #
        # So the skip is on the ANDROID branch, not on the engine: the two
        # drivers disagree only about how loudly they refuse, and the rig
        # should not be asking either of them.
        #
        # THE CONSEQUENCE IS A DENOMINATOR, NOT A FAILURE. The campaign's
        # matching-not-marking rule -- correct --canvas until the drawing
        # buffer is exactly the pinned size -- cannot apply to a display this
        # rig does not own. An Android row's viewport is REPORTED, never set,
        # so such a row is `cross=no` against every desktop row by
        # construction. Two Android rows are comparable to each other and to
        # nothing else.
        if args.android:
            stage("window-not-set",
                  why="android: the device owns the display; viewport is "
                      "reported, never set")
            result["viewport_source"] = "device (reported, not set)"
        else:
            try:
                session.set_window_rect(*window)
            except WebDriverError as e:
                result["gotchas"].append("set_window_rect failed: %s" % e)

        # AFTER the session exists (geckodriver has launched the app) and
        # BEFORE navigate, because a page loaded behind a wizard is a page
        # nobody is measuring. Android only -- no desktop leg executes a line
        # of this.
        if args.android and args.browser == "firefox":
            stage("onboarding-dismiss", package=args.android_package)
            dumps = []
            try:
                result["onboarding"] = android_dismiss_onboarding(
                    args.android_package, args.adb_serial, dump_sink=dumps)
            finally:
                if dumps:
                    uia_path = os.path.join(out_dir, "%s.uia.xml" % tag)
                    with open(uia_path, "w") as fh:
                        fh.write(dumps[-1])
                    result["onboarding_last_dump"] = uia_path
            stage("onboarding-dismissed",
                  rounds=result["onboarding"]["rounds"],
                  taps=len(result["onboarding"]["taps"]))

        stage("navigate", url=args.url)
        nav_t = time.monotonic()
        session.navigate(args.url, timeout=args.page_timeout + 30)
        stage("navigated", load_s=round(time.monotonic() - nav_t, 2))
        if args.android and args.browser == "firefox":
            # See verify_android_content_view: booted is not displayed.
            #
            # Fenix loads the page into a tab and keeps its home screen in
            # front of it. The user found the way out before the rig did --
            # "I always had to go through it then navigate to the 'jump back
            # in' on the home page before the test would start" -- so that is
            # what this does, by identity, and then RE-CHECKS. The second
            # check is not soft: a page still unshown after the repair fails
            # the leg rather than producing figures for a 300x150 default
            # buffer, which is not a small viewport but no viewport at all.
            cv = verify_android_content_view(session, timeout=8.0, soft=True)
            if cv is None:
                stage("surface-loaded-tab", why="page loaded, home screen in "
                                                "front of it")
                tab_dumps = []
                try:
                    result["surfaced_tab"] = android_surface_loaded_tab(
                        args.url, args.adb_serial, dump_sink=tab_dumps)
                finally:
                    if tab_dumps:
                        p = os.path.join(out_dir, "%s.tab.uia.xml" % tag)
                        with open(p, "w") as fh:
                            fh.write(tab_dumps[-1])
                        result["surfaced_tab_last_dump"] = p
                stage("surfaced-tab", **{
                    k: v for k, v in result["surfaced_tab"].items()
                    if k != "taps"})
                cv = verify_android_content_view(session, timeout=25.0)
            result["content_view"] = cv
            stage("content-view-shown", **cv)
            # A FOURTH DIALOG WILL COME. Onboarding, the default-browser
            # chooser and now geolocation were three; treating each as a
            # bespoke step means the next one is discovered as a mystery.
            # This is the same bounded, identity-driven handler run again at
            # the point prompts actually appear -- after the page is on screen
            # and has started asking for things. It no-ops when the browser is
            # reachable and nothing is in front of it, and it fails LOUDLY on
            # a dialog it does not recognise rather than swallowing it.
            stage("dialogs-after-load")
            post_dumps = []
            try:
                result["dialogs_after_load"] = android_dismiss_onboarding(
                    args.android_package, args.adb_serial,
                    dump_sink=post_dumps)
            finally:
                if post_dumps:
                    p = os.path.join(out_dir, "%s.postload.uia.xml" % tag)
                    with open(p, "w") as fh:
                        fh.write(post_dumps[-1])
                    result["dialogs_after_load_dump"] = p
            stage("dialogs-after-load-done",
                  rounds=result["dialogs_after_load"]["rounds"],
                  taps=len(result["dialogs_after_load"]["taps"]))
            # THE ROW'S LOCATION DENOMINATOR. Granted and denied are different
            # app behaviours, so a row that does not say which it was is not
            # comparable to one that does.
            try:
                result["geolocation"] = session.execute_async(GEO_PROBE)
            except WebDriverError as e:
                result["geolocation"] = {"probe_error": str(e)[:200]}
            stage("geolocation", **{
                k: v for k, v in (result["geolocation"] or {}).items()
                if k != "position"})

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
        # ON ANDROID THE BOOT PROBE IS TOO EARLY, and only there. MEASURED on
        # Gecko/Android: at 0.45 s after load the canvas is the HTML default
        # 300x150 buffer at 0x0 css, and by the end of the leg it is 852x818
        # css / 2557x2453 buffer, non-blank, having rendered the whole run.
        # Chrome/Android is already sized at boot (750x702), which is why five
        # Blink legs never saw this. A leg that rendered correctly for three
        # minutes is not a canvas failure because a probe fired before the
        # mobile viewport settled -- and reporting it as one makes every Gecko
        # phone row read red for a reason that is not about the app.
        #
        # Scoped to android so no desktop verdict can move: on the desktop
        # arms boot and final agree, so this can only ever rescue a false red
        # that only the phone produces.
        if args.android and not canvas_ok:
            result["canvas_ok_note"] = (
                "boot probe saw a 0x0 canvas; re-read at end of leg (mobile "
                "viewport settles after the boot probe)")
        result["canvas_ok"] = canvas_ok

        if args.canvas:
            result["canvas_target"] = fit_canvas(
                session, tuple(int(v) for v in args.canvas.split("x")), window)
            stage("canvas-target", **result["canvas_target"])

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

        # The frame-line watcher accumulates every histogram-bearing reading
        # for the whole leg (the console ring evicts). The baseline interact
        # count taken HERE is what --expect-interaction-frames must see
        # strictly exceeded by the end of the leg.
        frames_watch = FrameLineWatcher(session)
        frames_watch.poll()
        # The running-total lines' history, for --expect-tile-cache-settles.
        # Polled wherever the frame watcher is polled inside the data window,
        # so the ring's eviction cannot lose a reading between polls.
        totals_watch = RunningTotalsWatcher(session)
        totals_watch.poll()
        interact_before = frames_watch.interact_n()
        if args.expect_interaction_frames or args.w3c_gesture:
            stage("frame-lines-baseline", interact_n=interact_before)

        if args.wait_console_regex:
            result["console_waits"] = []
            for pat in args.wait_console_regex:
                stage("wait-console-regex", pattern=pat)
                w = wait_console_regex(session, pat,
                                       timeout=args.wait_console_timeout)
                result["console_waits"].append(w)
                stage("wait-console-regex-done", ok=w.get("ok"),
                      waited=w.get("waited_s"))

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
        if args.expect_zero_copy_replies:
            stage("zero-copy-wait", timeout=args.expect_timeout)
            result["zero_copy"] = wait_zero_copy_replies(
                session, timeout=args.expect_timeout)
            stage("zero-copy-done", **result["zero_copy"])
        if args.expect_overlay_rasters:
            stage("overlay-rasters-wait", timeout=args.expect_timeout)
            result["overlay_rasters"] = wait_overlay_rasters(
                session, timeout=args.expect_timeout)
            stage("overlay-rasters-done", **result["overlay_rasters"])
        if args.expect_seed_applied:
            stage("seed-applied-wait", timeout=args.expect_timeout)
            result["seed_applied"] = wait_seed_applied(
                session, timeout=args.expect_timeout)
            stage("seed-applied-done", **result["seed_applied"])
        if args.expect_basemap_tiles:
            stage("basemap-tiles-wait", timeout=args.expect_timeout)
            result["basemap_tiles"] = wait_basemap_tiles(
                session, timeout=args.expect_timeout)
            stage("basemap-tiles-done", **result["basemap_tiles"])

        stage("settle", seconds=args.settle)
        time.sleep(args.settle)

        if args.w3c_gesture:
            stage("w3c-gesture", kind=args.w3c_gesture,
                  seconds=args.gesture_seconds)
            try:
                result["w3c_gesture"] = drive_w3c_gesture(
                    session, args.w3c_gesture, args.gesture_seconds,
                    (result.get("env") or {}).get("inner"))
            except WebDriverError as e:
                release_actions(session)
                result["w3c_gesture"] = {"error": str(e)}
            stage("w3c-gesture-done", **(result["w3c_gesture"] or {}))
            frames_watch.poll()

        stage("raf-warm", frames=args.frames)
        result["raf_warm"] = raf_sample(session, args.frames)
        stage("raf-warm-done", **{k: (round(v, 2) if isinstance(v, float) else v)
                                  for k, v in (result["raf_warm"] or {}).items()
                                  if k in ("ok", "n", "p50", "p95", "max")})

        stage("data-window", seconds=args.data_window,
              window_loops=args.window_loops)
        if args.window_loops:
            # STATE THE WINDOW, DO NOT INFER IT. `--data-window` is seconds,
            # and seconds are the wrong unit for a window whose whole purpose
            # is to hold a fixed amount of scripted work: two legs given the
            # same seconds finish different numbers of loops, and comparing
            # their per-frame means is the leg-selection error this rig has
            # already published once. `native_row.py`'s `bracket()` has always
            # demanded N whole loops; this is the same demand on the web path.
            #
            # The poll is what makes it work at all: markers must be collected
            # as they arrive, because before `__rig_marks` existed they were
            # scrolling out of the console ring between polls and no amount of
            # waiting could recover them.
            deadline = time.monotonic() + args.window_loops_timeout
            mid_done = False
            while True:
                sig = frames_watch.poll()
                note_load()
                seen = len(frames_watch.loops)
                if not mid_done and seen * 2 >= args.window_loops:
                    result["resources_mid"] = session.execute(RESOURCES_PROBE)
                    mid_done = True
                if seen >= args.window_loops:
                    stage("window-loops-met", loops=seen)
                    break
                if time.monotonic() > deadline:
                    stage("window-loops-TIMEOUT", loops=seen,
                          wanted=args.window_loops,
                          marks_total=(sig or {}).get("marks_total"))
                    result.setdefault("gotchas", []).append(
                        "--window-loops %d not reached in %gs (saw %d); the "
                        "row's window is NOT the length that was asked for"
                        % (args.window_loops, args.window_loops_timeout, seen))
                    break
                time.sleep(1.0)
            if not mid_done:
                result["resources_mid"] = session.execute(RESOURCES_PROBE)
        else:
            time.sleep(args.data_window / 2)
            result["resources_mid"] = session.execute(RESOURCES_PROBE)
            frames_watch.poll()
            totals_watch.poll()
            note_load()
            time.sleep(args.data_window / 2)
        note_load()
        totals_watch.poll()
        result["resources"] = session.execute(RESOURCES_PROBE)
        stage("resources", count=(result["resources"] or {}).get("count"),
              hosts=list(((result["resources"] or {}).get("hosts") or {}))[:6],
              failed=len((result["resources"] or {}).get("failed") or []))

        # Rasterization wall time, as the app itself reports it: the `N` in
        # offload::deliver_job_reply's "<kind> took <N> ms off the frame". Read
        # here, after the data window, because that is when the most job
        # replies have accumulated in the console ring.
        #
        # This is the app's OWN timing of the offloaded job, not a wall-clock
        # difference this rig computes, and it is the figure a rayon pool is
        # supposed to move. Reported per browser and never pooled across them:
        # Chromium runs SwiftShader here and Firefox runs Xvfb/llvmpipe, and a
        # median over both would describe neither.
        _sig = session.execute(WORKER_SIGNAL_PROBE) or {}
        _kinds = {}
        for _k, _vals in (_sig.get("off_frame_by_kind") or {}).items():
            _ms = sorted(n for n in _vals if isinstance(n, int))
            if not _ms:
                continue
            _kinds[_k] = {"n": len(_ms), "samples": _ms, "min": _ms[0],
                          "median": _ms[len(_ms) // 2], "max": _ms[-1]}
        result["rasterization_ms"] = {
            "by_kind": _kinds,
            "rayon_threads": (max(_sig.get("rayon_threads") or [0]) or None),
        }
        stage("rasterization",
              rayon_threads=result["rasterization_ms"]["rayon_threads"],
              **{k: "n=%d med=%d" % (v["n"], v["median"])
                 for k, v in _kinds.items()})

        # What the worker wire actually moved and what it actually copied, in
        # bytes, as the page's own running totals (worker_port::account).
        # Reported unconditionally for the same reason `rasterization_ms` is:
        # a measurement round wants the number when nothing is gating on it,
        # and `null` says the line was never seen rather than "zero bytes".
        result["transport_bytes"] = _sig.get("transport")
        if result["transport_bytes"]:
            stage("transport", **result["transport_bytes"])

        # **The WS2 baseline**, read at the END of the data window so it covers
        # the whole leg rather than the moment a gate happened to be satisfied.
        # Reported unconditionally, like `transport_bytes` and
        # `rasterization_ms`: a measurement round wants the numbers when
        # nothing is gating on them, and `null` says the line was never
        # written -- which is "the path never ran", not "zero bytes".
        #
        # Four records, never one. `overlay_raster_totals` is the
        # whole-picture overlay dispatch alone; `texture_upload_totals` is
        # every texture delta this renderer was shown, radar and font atlas
        # included; `basemap_tile_totals` is archive tile BODIES DECODED, which
        # is in neither -- a vector decode uploads no texture at all, and a
        # raster decode is one egui texture and so is a subset of the upload
        # figure rather than a term to add to it; `ground_tile_totals` is what
        # the ground phase PLACED once those bodies had decoded, which is a
        # different question again -- a leg can decode tiles and place nothing.
        # They answer four different questions and are never summed.
        result["overlay_raster_totals"] = _sig.get("rasters")
        if result["overlay_raster_totals"]:
            stage("overlay-rasters", **result["overlay_raster_totals"])
        result["texture_upload_totals"] = _sig.get("uploads")
        if result["texture_upload_totals"]:
            stage("texture-uploads", **result["texture_upload_totals"])
        result["basemap_tile_totals"] = _sig.get("basemap")
        if result["basemap_tile_totals"]:
            stage("basemap-tiles", **result["basemap_tile_totals"])
        result["ground_tile_totals"] = _sig.get("ground")
        if result["ground_tile_totals"]:
            stage("ground-tiles", **result["ground_tile_totals"])
        # A sixth record, per cache role, and the one that classifies what
        # the fourth counts: events AT the tile cache (see `tile_cache_re`).
        # None -- never 0 -- when no `tile cache (...)` line matched: an older
        # bundle, or a cache that recorded nothing, and the two are told apart
        # by the `basemap` reading beside it.
        result["tile_cache_totals"] = _sig.get("tile_cache")
        if result["tile_cache_totals"]:
            for _role, _tc in sorted(result["tile_cache_totals"].items()):
                stage("tile-cache-" + _role,
                      **{k: v for k, v in _tc.items() if k not in ("t", "role")})
        # A fifth record, and a fifth question: what the 3D floor path painted
        # and why. In none of the four above -- a floor strip's repaint is a
        # second map render, not a texture delta -- and never summed with them.
        result["floor_strip_totals"] = _sig.get("floor")
        if result["floor_strip_totals"]:
            stage("floor-strips", **result["floor_strip_totals"])
        # The skew reading beside the totals, and it is what keeps the `-` in
        # the summary honest. A null total is TWO different facts — "the app
        # never wrote the line" and "the app wrote a line this rig cannot
        # read" — and the summary printed the first as certain for both,
        # which is how a stale `squallar-web/pkg` read as "no overlay raster
        # ever moved". Present only when a line was seen and not parsed, so
        # an ordinary null stays an ordinary null.
        result["telemetry_unparsed"] = {
            k: _sig.get(k + "_unparsed")
            for k in ("rasters", "uploads", "basemap")
            if _sig.get(k + "_unparsed")
        } or None
        if result["telemetry_unparsed"]:
            stage("telemetry-unparsed", **result["telemetry_unparsed"])

        # The frame-telemetry readout: the newest cumulative reading per
        # family, the count assert, and the gesture-window bin-diff when
        # marker lines bracketed one. No ms figure from any of this gates —
        # the one gate is the interact COUNT strictly increasing.
        frames_watch.poll()
        if args.expect_interaction_frames:
            base = interact_before if interact_before is not None else 0
            stage("interaction-frames-wait", before=interact_before)
            t_int = time.monotonic()
            n_now = frames_watch.interact_n()
            # One extra telemetry period past the gestures, so the reading
            # that carries their frames has had a tick to be written.
            while ((n_now is None or n_now <= base)
                   and time.monotonic() - t_int < 30.0):
                time.sleep(1.0)
                frames_watch.poll()
                n_now = frames_watch.interact_n()
            ok = n_now is not None and n_now > base
            result["interaction_frames"] = {"ok": ok,
                                            "before": interact_before,
                                            "after": n_now}
            if not ok:
                result["interaction_frames"]["error"] = (
                    "the `frame service (interact)` count never rose above "
                    "its %s baseline (last seen: %s). Either no interaction "
                    "frame was tagged — the count path this assert exists to "
                    "prove — or the line was never written at all, which "
                    "means the leg did not seed squallar.frame_telemetry"
                    % (interact_before, n_now))
            stage("interaction-frames-done", **result["interaction_frames"])

        # Liveness by PROGRESS, not by pixels. A frame loop that dies mid-leg
        # leaves every cheap signal green -- the canvas holds its last painted
        # frame, rAF keeps firing at display rate, fetches keep resolving --
        # so the only thing that can see it is the app's own frame counter
        # still climbing at the END of the leg. Two ways to fail, both real:
        # no reading arrived recently (the emitter is gone), or readings
        # arrived and the count did not move (the loop is idle, not drawing).
        if args.expect_frame_progress:
            frames_watch.poll()
            span_ms = args.expect_frame_progress * 1000.0
            rs = frames_watch.readings("cadence")
            now_ms = session.execute("return Date.now();")
            fp = {"ok": False, "window_s": args.expect_frame_progress,
                  "readings": len(rs)}
            if not rs:
                fp["error"] = (
                    "no `frame cadence` reading was ever scraped: either the "
                    "leg did not seed squallar.frame_telemetry, or the frame "
                    "loop died before the first 2 s emission")
            else:
                newest = rs[-1]
                inside = [r for r in rs if newest["t"] - r["t"] <= span_ms]
                fp.update(newest_t=newest["t"], newest_n=newest["n"],
                          stale_ms=(None if now_ms is None
                                    else int(now_ms - newest["t"])),
                          in_window=len(inside),
                          first_n=(inside[0]["n"] if inside else None))
                stale_bad = (now_ms is not None
                             and now_ms - newest["t"] > span_ms)
                if stale_bad:
                    fp["error"] = (
                        "the newest `frame cadence` reading is %d ms old at "
                        "the end of the leg (the line is written every 2 s "
                        "while frames are being produced). The frame loop "
                        "stopped and the page kept its last painted frame"
                        % (now_ms - newest["t"]))
                elif len(inside) < 2:
                    fp["error"] = (
                        "only %d `frame cadence` reading landed in the last "
                        "%.0f s, so nothing can be diffed; a live loop writes "
                        "one every 2 s" % (len(inside),
                                           args.expect_frame_progress))
                elif inside[-1]["n"] <= inside[0]["n"]:
                    fp["error"] = (
                        "the cumulative `frame cadence` count did not move "
                        "over the last %.0f s (%d -> %d across %d readings): "
                        "the loop is alive but produced no frame"
                        % (args.expect_frame_progress, inside[0]["n"],
                           inside[-1]["n"], len(inside)))
                else:
                    fp["ok"] = True
                    fp["gained"] = inside[-1]["n"] - inside[0]["n"]
            result["frame_progress"] = fp
            stage("frame-progress", **fp)

        # The settle assertion. See `tile_cache_settles` for the rule and
        # for why silence is a zero and an absent line is an error.
        if args.expect_tile_cache_settles:
            totals_watch.poll()
            now_ms = session.execute("return Date.now();")
            tcs = tile_cache_settles(totals_watch, args.expect_tile_cache_settles,
                                     now_ms)
            result["tile_cache_settles"] = tcs
            stage("tile-cache-settles", ok=tcs["ok"], window_s=tcs["window_s"],
                  **{"delta_" + name.split(":")[-1].strip().replace(" ", "_"):
                     fam.get("delta") for name, fam in tcs["families"].items()})
        fl_last = frames_watch.last or {}
        result["frame_lines"] = {
            k: fl_last.get(k) for k in ("interact", "idle", "segments",
                                        "prep", "gpu", "gpu_unavailable",
                                        "cadence", "loop_state")}
        # Recorded beside `loop_state`, and None -- never 0 -- when no
        # `budget state:` line matched: an older bundle, not a zero reading.
        result["frame_lines"]["budget_state"] = fl_last.get("budget_state")
        gw = gesture_window_stats(frames_watch, args.quiet_window,
                                  args.window_skip_loops)
        if gw is not None:
            result["gesture_window"] = gw
            stage("gesture-window",
                  script=gw.get("script"), loops=gw.get("loops_completed"),
                  interact_n=(gw.get("interact") or {}).get("n"),
                  cadence_n=(gw.get("cadence") or {}).get("n"))

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
        # See the canvas_ok note above: on Android only, the end-of-leg read is
        # what says whether there was ever a canvas to render into.
        if args.android and not result.get("canvas_ok"):
            cf = result["canvas_final"] or {}
            if (cf.get("hasCanvas") and cf.get("clientWidth", 0) > 0
                    and cf.get("clientHeight", 0) > 0):
                result["canvas_ok"] = True
                canvas_ok = True          # the verdict below reads the local
                result["canvas_ok_note"] = (
                    "boot probe saw a 0x0 canvas at %.2fs; end of leg is "
                    "%sx%s css / %sx%s buffer. Mobile viewport settles after "
                    "the boot probe -- Chrome/Android is already sized there, "
                    "Gecko/Android is not."
                    % (result["boot"]["seconds_after_load"],
                       cf.get("clientWidth"), cf.get("clientHeight"),
                       cf.get("bufferWidth"), cf.get("bufferHeight")))
        shots = {}
        page_png = os.path.join(out_dir, "%s.page.png" % tag)
        data = save_screenshot(session.screenshot_b64(), page_png)
        shots["page"] = png_stats(data)
        shots["page"]["path"] = page_png
        canvas_el = session.find_element("#squallar-canvas")
        if canvas_el:
            canvas_png = os.path.join(out_dir, "%s.canvas.png" % tag)
            data = save_screenshot(
                session.element_screenshot_b64(canvas_el), canvas_png)
            shots["canvas"] = png_stats(data)
            shots["canvas"]["path"] = canvas_png
        else:
            shots["canvas"] = {"error": "element #squallar-canvas not found"}
        result["screenshots"] = shots
        stage("screenshots-done",
              page_blank=shots["page"].get("blank"),
              canvas_blank=shots.get("canvas", {}).get("blank"),
              page_distinct=shots["page"].get("distinct_colors"),
              canvas_distinct=shots.get("canvas", {}).get("distinct_colors"))

        if args.android:
            result["device_after"] = android_device_state(args.adb_serial)
            stage("device-after", **result["device_after"])

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
        # A wasm TRAP is not a panic and carries no `panicked at` line: this
        # target is `panic-strategy: abort`, so an allocation failure or an
        # `abort()` reaches the console as a bare `RuntimeError: unreachable
        # executed` (or, from a Rust future, an unhandled rejection carrying
        # only a wasm stack). Counted separately and gated the same way,
        # because a trapped module is a dead module: nothing unwinds, so every
        # RefCell borrow held at the trap stays held for the life of the page.
        traps = [e for e in rig_errors
                 if any(m in str(e.get("msg", ""))
                        for m in ("unreachable executed", "RuntimeError",
                                  "memory access out of bounds",
                                  "Out of bounds memory access",
                                  "wasm-function["))]
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
        zc = result.get("zero_copy")
        ovr = result.get("overlay_rasters")
        bmt = result.get("basemap_tiles")
        # A leg whose seed never landed measured a scene nobody chose. That is
        # not a weaker fact than a dead worker wire -- it makes every figure on
        # the row uninterpretable -- so it fails the run on the same terms.
        sap = result.get("seed_applied")
        worker_ok = ((wrt is None or bool(wrt.get("ok")))
                     and (dr is None or bool(dr.get("ok")))
                     and (rp is None or bool(rp.get("ok")))
                     and (zc is None or bool(zc.get("ok")))
                     and (ovr is None or bool(ovr.get("ok")))
                     and (sap is None or bool(sap.get("ok")))
                     and (bmt is None or bool(bmt.get("ok"))))
        # The count assert (--expect-interaction-frames) and any requested
        # console waits. Counts and presence only; no ms figure is in here.
        ifr = result.get("interaction_frames")
        ifr_ok = ifr is None or bool(ifr.get("ok"))
        cwaits = result.get("console_waits")
        cwaits_ok = (cwaits is None
                     or all(bool(w.get("ok")) for w in cwaits))

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
        # Two adapters are asked, because they are two questions: a page the
        # app runs on the WebGPU backend has no WebGL2 context by design (the
        # canvas has answered `getContext("webgpu")` and answers no other), so
        # its WebGL classification reads `none` while a real GPU draws. Either
        # a hardware WebGL adapter or a hardware WebGPU adapter clears the
        # floor; a software or absent WebGL adapter beside no WebGPU adapter
        # fails it, exactly as before. See `hardware_floor`.
        webgpu_adapter = classify_webgpu_adapter(wg)
        result["webgpu_adapter"] = webgpu_adapter
        hw_ok = None
        hw_via = None
        if args.require_hardware:
            hw_via = hardware_floor(adapter, webgpu_adapter)
            hw_ok = hw_via is not None
            if not hw_ok:
                result["gotchas"].append(
                    "--require-hardware: WebGL adapter is %s; WebGPU adapter "
                    "is %s; neither is a GPU -- fix the GPU flags rather than "
                    "reporting this figure"
                    % (adapter_label(adapter),
                       webgpu_adapter_label(webgpu_adapter)))

        fp_ok = (result.get("frame_progress") is None
                 or bool((result.get("frame_progress") or {}).get("ok")))
        tcs_ok = (result.get("tile_cache_settles") is None
                  or bool((result.get("tile_cache_settles") or {}).get("ok")))
        # THE CANVAS THIS LEG ACTUALLY GOT, asserted rather than requested.
        # `fit_canvas` has always returned `met`, and until 2026-08-31 nothing
        # read it: a leg could ask for 2878x1651, be handed 1280x815 because
        # the window manager or the Xvfb screen would not go that big, and
        # report PASS while its row implied a size it never rendered at. A
        # size-specific defect -- and there is one: the app dies on a winit
        # `RefCell already borrowed` panic at 2878x1651 and survives at
        # 1280x815 -- is invisible to a leg that quietly ran small.
        ct = result.get("canvas_target")
        cv_ok = None
        if args.expect_canvas:
            if ct is None:
                cv_ok = False
                result["canvas_expect"] = {
                    "ok": False,
                    "error": "--expect-canvas without --canvas: there is no "
                             "target to have met"}
            else:
                cv_ok = bool(ct.get("met"))
                result["canvas_expect"] = {
                    "ok": cv_ok, "asked": ct.get("asked"), "got": ct.get("got"),
                    "error": (None if cv_ok else
                              "canvas drawing buffer is %s, not the %s this "
                              "leg asked for -- every figure and every verdict "
                              "on this row describes the WRONG SIZE"
                              % (ct.get("got"), ct.get("asked")))}
        result["pass"] = (booted and canvas_ok and raf_ok
                          and canvas_blank is not True and not panics
                          and not traps and fp_ok and tcs_ok
                          and worker_ok and ifr_ok and cwaits_ok
                          and sw_ok is not False and coi_ok is not False
                          and cv_ok is not False
                          and hw_ok is not False)
        result["verdict"] = {
            "arm": args.arm,
            "adapter_class": adapter.get("class"),
            "adapter_renderer": adapter.get("renderer"),
            "webgpu_adapter_class": webgpu_adapter.get("class"),
            "hardware_ok": hw_ok,
            "hardware_via": hw_via,
            "booted": booted, "canvas_ok": canvas_ok, "raf_ok": raf_ok,
            "service_worker_ok": sw_ok,
            "cross_origin_isolated": coi,
            "cross_origin_isolated_ok": coi_ok,
            "resource_failures": len((result.get("resources") or {})
                                     .get("failed") or []),
            "rig_error_count": len(rig_errors),
            "panic_count": len(panics),
            "first_panic": (str(panics[0].get("msg"))[:300] if panics else None),
            "wasm_trap_count": len(traps),
            "first_wasm_trap": (str(traps[0].get("msg"))[:300]
                                if traps else None),
            "frame_progress_ok": (None if result.get("frame_progress") is None
                                  else bool(result["frame_progress"]["ok"])),
            "tile_cache_settles_ok": (None if result.get("tile_cache_settles") is None
                                      else bool(result["tile_cache_settles"]["ok"])),
            # The SIZE this row's every other number is a figure for. A leg
            # that passed at 1280x815 and one that passed at 2878x1651 are not
            # the same evidence, and a summary that does not say which is which
            # cannot be read.
            "canvas_buffer": _buffer_size(result),
            "canvas_target_met": (None if ct is None else bool(ct.get("met"))),
            "canvas_ok_expected": cv_ok,
            "page_blank": shots["page"].get("blank"),
            "canvas_blank": canvas_blank,
            "worker_round_trip_ok": (None if wrt is None
                                     else bool(wrt.get("ok"))),
            "doctored_respawn_ok": (None if dr is None
                                    else bool(dr.get("ok"))),
            "interaction_frames_ok": (None if ifr is None
                                      else bool(ifr.get("ok"))),
            "console_waits_ok": (None if cwaits is None else cwaits_ok),
            "rayon_pool_ok": (None if rp is None else bool(rp.get("ok"))),
            "zero_copy_replies_ok": (None if zc is None else bool(zc.get("ok"))),
            "overlay_rasters_ok": (None if ovr is None else bool(ovr.get("ok"))),
            # Same rule as `rayon_threads` below: reported whether or not
            # anything gated on it, because this IS the WS3c measurement.
            "transport_bytes": result.get("transport_bytes"),
            # And the WS2 baseline, on the same terms. Two records with two
            # denominators; see where they are collected.
            "overlay_raster_totals": result.get("overlay_raster_totals"),
            "texture_upload_totals": result.get("texture_upload_totals"),
            # Reported unconditionally, not only under --expect-rayon-threads:
            # a measurement run wants the number even when nothing is gating
            # on it, and `null` distinguishes "never observed" from "one".
            "rayon_threads": (None if rp is None else rp.get("threads")),
        }
        exit_code = EXIT_PASS if result["pass"] else EXIT_LEG_FAILED

    except Exception as e:
        # A full disk is the BOX failing, not the app, and it is re-raised
        # rather than filed here: what this leg was asserting is UNKNOWN, it
        # neither passed nor failed, and `_run_cli` turns it into EXIT_INFRA so
        # the launcher can say so. Filing it as a leg failure (which is what
        # happened on 2026-08-31) sends a reader hunting a rendering bug that
        # does not exist.
        if isinstance(e, OSError) and e.errno in INFRA_ERRNOS:
            raise
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
        if args.android:
            # Every leg serves on a fresh kernel-chosen port, so a rig that
            # only ADDS mappings leaves one per leg on somebody's phone --
            # thirteen after one evening, measured. Clean up what we create.
            result["adb_reverse_removed"] = adb_reverse_cleanup()

    result["total_s"] = round(time.monotonic() - t0, 2)
    # The closing sample, after the browser is gone: `end` should describe the
    # box the leg ran on, not the teardown.
    note_load()
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
    # The arm, the adapter AND crossOriginIsolated lead every summary, and
    # are repeated on each line that carries a cap or a frame time. A figure
    # whose adapter is unknown is worse than no figure: it gets quoted. COI
    # is a DENOMINATOR, not a detail: without it there is no SAB, the worker
    # pool is the one-thread fallback, and every frame figure describes a
    # threading configuration the app never ships in -- run_measure.sh
    # treats coi!=true as an INVALID row.
    binfo = result.get("binary") or {}
    print("[%s] SUMMARY arm=%s adapter=%s (%s) via %s coi=%s"
          % (tag, result.get("arm"), alabel, adapter.get("vendor"),
             binfo.get("gpu_mode") or binfo.get("ff_mode") or "?",
             env.get("cross_origin_isolated")))
    hd = binfo.get("host_display") or {}
    if hd:
        print("[%s] SUMMARY display=%s (%s) xauthority=%s"
              % (tag, hd.get("display") or "none", hd.get("source")
                 or hd.get("why"), hd.get("xauthority")))
    print("[%s] SUMMARY pass=%s booted=%s canvas=%sx%s (buffer %sx%s) dpr=%s"
          % (tag, result.get("pass"), v.get("booted"),
             b.get("clientWidth"), b.get("clientHeight"),
             b.get("bufferWidth"), b.get("bufferHeight"), env.get("dpr")))
    if result.get("viewport_source"):
        # The Android row's denominators, printed where the caps are, because
        # nothing on this row can be read beside a desktop row: the viewport
        # was reported by a device this rig does not own, not corrected to a
        # pinned size, so `cross=no` is a property of the leg and not a result.
        bw, bh = b.get("bufferWidth"), b.get("bufferHeight")
        px = (bw * bh) if isinstance(bw, int) and isinstance(bh, int) else None
        print("[%s] SUMMARY viewport: %s -- %sx%s css at dpr %s = %s device "
              "pixels; cross=no against any desktop or native row BY "
              "CONSTRUCTION (two Android rows compare to each other only)"
              % (tag, result["viewport_source"], b.get("clientWidth"),
                 b.get("clientHeight"), env.get("dpr"), px))
        before = result.get("device_before") or {}
        after = result.get("device_after") or {}
        t0_c, t1_c = before.get("battery_temp_c"), after.get("battery_temp_c")
        drift = (round(t1_c - t0_c, 1)
                 if isinstance(t0_c, (int, float))
                 and isinstance(t1_c, (int, float)) else None)
        geo = result.get("geolocation") or {}
        grant = result.get("location_grant") or {}
        pos = geo.get("position")
        print("[%s] SUMMARY location: permission=%s position=%s os_granted=%s"
              "%s -- A DENOMINATOR, not a detail: the UserLocation layer draws "
              "on the Ground surface and feeds a content key, so a row taken "
              "with location denied is not comparable to one taken with it "
              "allowed"
              % (tag, geo.get("permission"),
                 ("%.2f,%.2f" % (pos["lat"], pos["lon"])) if pos else None,
                 len(grant.get("granted") or []),
                 (" err=%s" % geo.get("error")) if geo.get("error") else ""))
        print("[%s] SUMMARY device: battery %s%% -> %s%%, %s C -> %s C "
              "(drift %s C). BOTH ENDS, because a phone throttles: a figure "
              "quoted without them cannot be told apart from a thermal one"
              % (tag, before.get("battery_percent"),
                 after.get("battery_percent"), t0_c, t1_c, drift))
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
    wa = result.get("webgpu_adapter") or classify_webgpu_adapter(wg)
    if v.get("hardware_ok") is False:
        print("[%s] SUMMARY HARDWARE ARM FAILED: WebGL adapter is %s, WebGPU "
              "adapter is %s -- neither is a GPU"
              % (tag, alabel, webgpu_adapter_label(wa)))
    elif v.get("hardware_ok") is True:
        print("[%s] SUMMARY hardware floor cleared by the %s adapter (WebGL "
              "%s; WebGPU %s)"
              % (tag, v.get("hardware_via"), alabel, webgpu_adapter_label(wa)))
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
    zc = result.get("zero_copy")
    if zc is not None:
        print("[%s] SUMMARY zero-copy replies: %s%s"
              % (tag, "OK" if zc.get("ok") else "FAILED",
                 "" if zc.get("ok") else "; " + str(zc.get("error"))))
    # The WS3c measurement, printed whether or not it was gated -- and never
    # pooled across browsers, for the same reason the per-kind medians are not.
    tb = result.get("transport_bytes")
    if tb:
        print("[%s] SUMMARY transport: %s replies, %s B out with %s B copied "
              "out of the worker, %s B in with %s B copied out of the page"
              % (tag, tb.get("replies"), tb.get("out_moved"),
                 tb.get("out_copied"), tb.get("in_moved"), tb.get("in_copied")))
    ovr = result.get("overlay_rasters")
    if ovr is not None:
        print("[%s] SUMMARY overlay rasters: %s%s"
              % (tag, "OK" if ovr.get("ok") else "FAILED",
                 "" if ovr.get("ok") else "; " + str(ovr.get("error"))))
    sap = result.get("seed_applied")
    if sap is not None:
        print("[%s] SUMMARY seed applied: %s%s"
              % (tag, "OK" if sap.get("ok") else "FAILED",
                 (" (prelude wrote %s; %s loop-state lines)"
                  % (sap.get("seeded"), sap.get("loud")))
                 if sap.get("ok") else "; " + str(sap.get("error"))))
    bmt = result.get("basemap_tiles")
    if bmt is not None:
        print("[%s] SUMMARY basemap tiles: %s%s"
              % (tag, "OK" if bmt.get("ok") else "FAILED",
                 (" (%s vector bodies decoded)" % bmt.get("vector_tiles"))
                 if bmt.get("ok") else "; " + str(bmt.get("error"))))
    # The WS2 baseline, printed whether or not it was gated, and per browser --
    # never pooled, for the same reason the per-kind medians are not.
    ort = result.get("overlay_raster_totals")
    if ort:
        # `inked` is printed against its own denominator, `pictures`, because
        # the bare count says nothing on its own: 6 inked is healthy at 6
        # pictures and is half the map missing at 12.
        print("[%s] SUMMARY overlay raster totals [whole-picture overlay "
              "dispatch only]: %s dispatched, %s arrived, %s pictures of %s B, "
              "%s of %s inked, %s shown, %s promoted, %s dropped, "
              "%s superseded, %s cancelled"
              % (tag, ort.get("dispatched"), ort.get("arrived"),
                 ort.get("pictures"), ort.get("picture_bytes"),
                 ort.get("inked"), ort.get("pictures"),
                 ort.get("shown"), ort.get("promoted"), ort.get("dropped"),
                 ort.get("superseded"), ort.get("cancelled")))
    tut = result.get("texture_upload_totals")
    if tut:
        # `whole` is a routing subset of `blocking`, never added to it; the
        # GPU total is the disjoint pair staged + blocking.
        print("[%s] SUMMARY texture upload totals [EVERY egui texture on this "
              "renderer, radar and font atlas included]: %s deltas, %s B to "
              "the GPU, %s B whole, %s bands, %s B staged, %s B blocking"
              % (tag, tut.get("deltas"), tut.get("bytes"),
                 tut.get("whole_bytes"), tut.get("bands"),
                 tut.get("staged_bytes"), tut.get("blocking_bytes")))
    bmtt = result.get("basemap_tile_totals")
    if bmtt:
        # A THIRD denominator, added to neither figure above: bodies DECODED,
        # not rasters dispatched and not texture deltas. A vector body uploads
        # no texture at all; a raster body is one egui texture and so is a
        # subset of the upload figure, never a term to add to it.
        print("[%s] SUMMARY basemap tile totals [archive tile BODIES DECODED, "
              "in neither figure above]: %s vector, %s raster, %s sniffed"
              % (tag, bmtt.get("vector_tiles"), bmtt.get("raster_tiles"),
                 bmtt.get("sniffed_tiles")))
    tct = result.get("tile_cache_totals")
    if tct:
        # A SIXTH denominator, per cache role: events at the tile cache.
        # `refetch after eviction` is a SUBSET of `asks`; the four put kinds
        # are disjoint and sum to the puts; the last three are LEVELS. Never
        # subtracted from `uploads` above.
        for role in sorted(tct):
            c = tct[role]
            print("[%s] SUMMARY tile cache totals (%s) [events AT the tile "
                  "cache; refetch is a subset of asks; entries/resident/parsed "
                  "are levels]: %s asks, %s restyle asks, %s refetch after "
                  "eviction, %s puts first, %s restyle, %s duplicate, %s orphan, "
                  "%s evicted pending, %s evicted resident of %s B, %s entries, "
                  "%s B resident, %s parsed"
                  % (tag, role, c.get("asks"), c.get("restyle_asks"),
                     c.get("refetch_after_eviction"), c.get("puts_first"),
                     c.get("puts_restyle"), c.get("puts_duplicate"),
                     c.get("puts_orphan"), c.get("evicted_pending"),
                     c.get("evicted_resident"), c.get("evicted_bytes"),
                     c.get("resident_entries"), c.get("resident_bytes"),
                     c.get("parsed_entries")))
    tcs = result.get("tile_cache_settles")
    if tcs is not None:
        print("[%s] SUMMARY tile cache settles [last %.0f s of a static "
              "viewport; three deltas, never added]: %s"
              % (tag, tcs.get("window_s") or 0,
                 "OK" if tcs.get("ok") else "FAILED"))
        for name, fam in (tcs.get("families") or {}).items():
            print("[%s] SUMMARY   %s: delta %s over %s readings in the window "
                  "(%s)%s"
                  % (tag, name, fam.get("delta"), fam.get("in_window", 0),
                     fam.get("basis", "no reading"),
                     "" if fam.get("ok") else "; " + str(fam.get("error"))))
    # The frame-telemetry readout. Cumulative rows are labelled cumulative
    # and the gesture-window rows are labelled with their bracket, because
    # the spike showed cumulative-from-boot p99s are boot-contaminated and
    # the two must never be quoted as the same figure. GPU rows are a
    # different clock from the service rows and are never added to them.
    fl = result.get("frame_lines") or {}
    if fl.get("interact"):
        i = fl["interact"]
        print("[%s] SUMMARY [%s] frame service (interact) [cumulative from "
              "boot]: n=%s p50=%s us p90=%s us p99=%s us"
              % (tag, alabel, i.get("n"), i.get("p50"), i.get("p90"),
                 i.get("p99")))
    if fl.get("idle"):
        i = fl["idle"]
        print("[%s] SUMMARY [%s] frame service (idle) [cumulative from "
              "boot]: n=%s p50=%s us p90=%s us p99=%s us"
              % (tag, alabel, i.get("n"), i.get("p50"), i.get("p90"),
                 i.get("p99")))
    if fl.get("segments"):
        s = fl["segments"]
        print("[%s] SUMMARY [%s] frame segments (interact, p99 us, "
              "cumulative): pre=%s pump=%s ui=%s prepare=%s finish=%s "
              "post=%s; acquire n=%s p50=%s us p99=%s us"
              % (tag, alabel, s.get("pre"), s.get("pump"), s.get("ui"),
                 s.get("prepare"), s.get("finish"), s.get("post"),
                 s.get("acquire_n"), s.get("acquire_p50"),
                 s.get("acquire_p99")))
    if fl.get("prep"):
        p = fl["prep"]
        print("[%s] SUMMARY [%s] frame prep costs [cumulative us]: %s "
              "passes, tessellate=%s upload_apply=%s mirror=%s "
              "buffers_and_callbacks=%s"
              % (tag, alabel, p.get("passes"), p.get("tessellate_us"),
                 p.get("upload_apply_us"), p.get("mirror_us"),
                 p.get("buffers_and_callbacks_us")))
    if fl.get("gpu"):
        g = fl["gpu"]
        fam = lambda f: "n=%s p50=%s p99=%s" % (f.get("n"), f.get("p50"),
                                                f.get("p99"))
        print("[%s] SUMMARY [%s] gpu passes [GPU clock, us, cumulative; "
              "never added to service]: raymarch %s; ground %s; mirror %s; "
              "main %s; %s frames"
              % (tag, alabel, fam(g.get("raymarch") or {}),
                 fam(g.get("ground") or {}), fam(g.get("mirror") or {}),
                 fam(g.get("main") or {}), g.get("frames")))
    elif fl.get("gpu_unavailable"):
        print("[%s] SUMMARY [%s] gpu passes: unavailable (adapter lacks "
              "TIMESTAMP_QUERY) -- an absence, never an extrapolation"
              % (tag, alabel))
    if fl.get("cadence"):
        c = fl["cadence"]
        print("[%s] SUMMARY [%s] frame cadence [cumulative from boot]: "
              "n=%s p50=%s us p99=%s us"
              % (tag, alabel, c.get("n"), c.get("p50"), c.get("p99")))
    if fl.get("loop_state"):
        s = fl["loop_state"]
        # A LEVEL at the end of the leg, not a total over it. `resident`,
        # `in flight` and `failed` are disjoint subsets of `listed` and are
        # never added to it; `allowed`/`cap`/`held` bound frames TEXTURED,
        # which is a different denominator from the slots `listed` counts.
        print("[%s] SUMMARY [%s] loop state [level, end of leg]: %s panes, "
              "%s layers animating, %s frames listed, %s resident "
              "(%s in flight, %s failed); allowed plan=%s section=%s "
              "volume=%s overlay=%s, cap %s, held %s; share %s B, pool %s B "
              "in [%s, %s]; advance %s us"
              % (tag, alabel, s.get("panes"), s.get("layers"),
                 s.get("listed"), s.get("resident"), s.get("in_flight"),
                 s.get("failed"), s.get("allowed_plan"),
                 s.get("allowed_section"), s.get("allowed_volume"),
                 s.get("allowed_overlay"), s.get("cap"), s.get("held"),
                 s.get("share_bytes"), s.get("pool_bytes"),
                 s.get("floor_bytes"), s.get("ceiling_bytes"),
                 s.get("advance_us")))
    if fl.get("budget_state"):
        b = fl["budget_state"]
        # A LEVEL at the end of the leg. `pool` is the live loop pool in MiB
        # and `ceiling` the bracket's constant; vram/ram/declared are three sources
        # and never one figure; the two `linear` figures are two instances;
        # `cap` is the capacity in force and its source (0 presumed, 1
        # measured, 2 probed) -- Firefox on WebGL2 is `288 0`, never merged
        # with a Chromium reading; `probe` is where the WebGPU probe stands
        # (0 absent, 1 skipped, 2 pending, 3 empty, 4 found, 5 found capped),
        # read off this level line because the probe's own lines are evicted
        # from the console ring within seconds; `balloon` is what the loops
        # hold above their base, a subset of `pool` and never added to it.
        print("[%s] SUMMARY [%s] budget state [level, end of leg]: bracket %s, "
              "rung %s, steps %s; pool %s MiB, ceiling %s MiB; vram %s MiB, "
              "ram %s MiB, declared %s MiB, threads %s, form %s; "
              "linear %s/%s MiB; cap %s MiB source %s; probe %s; balloon %s MiB"
              % (tag, alabel, b.get("bracket"), b.get("rung"), b.get("steps"),
                 b.get("pool_mib"), b.get("ceiling_mib"), b.get("vram_mib"),
                 b.get("ram_mib"), b.get("declared_mib"), b.get("threads"),
                 b.get("form"), b.get("linear_page_mib"),
                 b.get("linear_worker_mib"), b.get("cap_mib"),
                 b.get("cap_source"), b.get("probe"), b.get("balloon_mib")))
    gw = result.get("gesture_window")
    if gw:
        print("[%s] SUMMARY [%s] gesture window (%s, %s loops, %s):"
              % (tag, alabel, gw.get("script"), gw.get("loops_completed"),
                 gw.get("basis")))
        for family in ("interact", "cadence"):
            d = gw.get(family) or {}
            if d.get("error"):
                print("[%s] SUMMARY [%s]   window %s: %s"
                      % (tag, alabel, family, d["error"]))
            else:
                print("[%s] SUMMARY [%s]   window %s: n=%s p50=%s us "
                      "p90=%s us p99=%s us"
                      % (tag, alabel, family, d.get("n"), d.get("p50_us"),
                         d.get("p90_us"), d.get("p99_us")))
        # The windowed per-segment and per-take families. Printed with their
        # exact mean because that is the figure the bins cannot give: four
        # bins per octave means any true ratio from 1.68x to 2.38x prints as
        # 2.00x. `mean` is read AGAINST p90/p99, never instead of them.
        #
        # A `take` figure's denominator is ONE TAKE -- one completion moved
        # off a tile source's channel and handled to completion. It is not per
        # frame, not per pass, and is never added to any `segment` figure: a
        # `segment pump` sample is a whole frame's pump phase and can contain
        # several takes or none.
        for family in watcher_named_in(gw):
            d = gw.get(family) or {}
            if d.get("error"):
                print("[%s] SUMMARY [%s]   window %s: %s"
                      % (tag, alabel, family, d["error"]))
            elif d.get("n"):
                print("[%s] SUMMARY [%s]   window %s: n=%s mean=%s us "
                      "p50=%s us p90=%s us p99=%s us max=%s us"
                      % (tag, alabel, family, d.get("n"), d.get("mean_us"),
                         d.get("p50_us"), d.get("p90_us"), d.get("p99_us"),
                         d.get("max_us")))
            else:
                # A family with a reading but no samples IN the window. Said
                # out loud: "nothing happened here" is a figure, and it is a
                # different fact from a family that was never reported at all.
                print("[%s] SUMMARY [%s]   window %s: n=0 (reported, no "
                      "samples in the window)" % (tag, alabel, family))
    ifr = result.get("interaction_frames")
    if ifr is not None:
        print("[%s] SUMMARY interaction frames: %s (interact n %s -> %s%s)"
              % (tag, "OK" if ifr.get("ok") else "FAILED",
                 ifr.get("before"), ifr.get("after"),
                 "" if ifr.get("ok") else "; " + str(ifr.get("error"))))
    for w in (result.get("console_waits") or []):
        print("[%s] SUMMARY console wait /%s/: %s%s"
              % (tag, w.get("pattern"), "OK" if w.get("ok") else "FAILED",
                 "" if w.get("ok") else "; " + str(w.get("error"))))
    wg_ = result.get("w3c_gesture")
    if wg_ is not None:
        print("[%s] SUMMARY w3c gesture: %s" % (tag, json.dumps(wg_)))
    rm = result.get("rasterization_ms")
    if rm and rm.get("by_kind"):
        # One line per KIND, never a pooled figure: see the probe.
        for kind in sorted(rm["by_kind"]):
            k = rm["by_kind"][kind]
            print("[%s] SUMMARY off-the-frame %s: n=%d min=%d median=%d "
                  "max=%d ms (rayon %s threads)"
                  % (tag, kind, k["n"], k["min"], k["median"], k["max"],
                     rm.get("rayon_threads")))
    return exit_code


class LoudArgumentParser(argparse.ArgumentParser):
    """An argument parser whose refusal is impossible to scroll past.

    argparse's own `error()` prints a twenty-five line usage block and then one
    line naming the problem, and exits 2 -- the same code this rig uses for "the
    leg ran and failed". Both halves of that were load-bearing in the 2026-08-31
    incident: the wrapper could not tell the two apart by exit code, and a human
    reading the log saw eight usage dumps and one real sentence buried in each.

    So: the sentence comes FIRST, in a banner, with the exit code that says
    which kind of failure this is; the usage block follows for whoever needs it.
    """

    def error(self, message):
        sys.stderr.write(
            "\n"
            "!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!\n"
            "drive.py REFUSED ITS COMMAND LINE -- nothing ran, no result was\n"
            "written, and no artefact on disk describes this invocation.\n"
            "\n"
            "  %s\n"
            "\n"
            "The usual cause is a rig commit that went missing: the launcher\n"
            "passes a flag this copy of drive.py does not declare. Check that\n"
            "run_tier2.sh and drive.py are from the SAME commit before\n"
            "believing anything else in this log.\n"
            "!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!\n"
            "\n" % message)
        self.print_usage(sys.stderr)
        sys.exit(EXIT_USAGE)


def main(argv=None):
    ap = LoudArgumentParser(
        description="headless WebDriver smoke/measurement rig for squallar-web")
    ap.add_argument("--browser", choices=("chromium", "firefox", "safari"))
    ap.add_argument("--url")
    ap.add_argument("--out-dir", default="out")
    ap.add_argument("--tag", default=None, help="output file prefix "
                    "(default: browser name)")
    ap.add_argument("--run-id", default=None,
                    help="opaque token identifying THIS invocation. It is "
                         "copied verbatim into the result JSON as `run_id`, "
                         "and a launcher that passed one is expected to refuse "
                         "any artefact that does not carry it back. Without "
                         "it, nothing on disk ties a verdict to the run that "
                         "produced it: a leg that never started leaves the "
                         "previous run's JSON in place and every reader -- the "
                         "summary block, a comparison script, a human -- reads "
                         "the stale verdict as this run's")
    ap.add_argument("--driver", default=None,
                    help="driver binary (default: %s / geckodriver on PATH "
                         "then %s)" % (DEFAULT_CHROMEDRIVER, DEFAULT_GECKODRIVER))
    ap.add_argument("--binary", default=None, help="browser binary override")
    ap.add_argument("--window", default="1280x900")
    ap.add_argument("--canvas", default=None,
                    help="target canvas DRAWING BUFFER size WxH; the window "
                         "is corrected until the buffer matches, and the row "
                         "records whether it did. Pixels, not the window, are "
                         "what the overlay picture is sized from, so this is "
                         "what makes two legs comparable")
    ap.add_argument("--expect-canvas", action="store_true",
                    help="FAIL the leg if --canvas was not met. Without this "
                         "the target is a request: `met` is recorded and "
                         "nothing reads it, so a leg that could not be made "
                         "that big -- a window manager that refused, an Xvfb "
                         "screen smaller than the target -- passes while every "
                         "figure on its row silently describes a different "
                         "size. Pass it on any leg whose whole point is the "
                         "size it runs at")
    ap.add_argument("--frames", type=int, default=120,
                    help="rAF deltas per sample (default 120)")
    ap.add_argument("--settle", type=float, default=6.0,
                    help="seconds after boot before the warm rAF sample")
    ap.add_argument("--data-window", type=float, default=10.0,
                    help="seconds to let live data arrive before second sample")
    ap.add_argument("--window-loops", type=int, default=0,
                    help="hold the data window open until this many gesture "
                         "loop markers have been SEEN, instead of for "
                         "--data-window seconds. The unit a comparison needs: "
                         "two legs given equal seconds finish unequal loops, "
                         "and their per-frame means are then not comparable. "
                         "0 (default) keeps the seconds behaviour.")
    ap.add_argument("--window-loops-timeout", type=float, default=600.0,
                    help="give up waiting for --window-loops after this many "
                         "seconds; the leg still reports, with a gotcha and a "
                         "`window-loops-TIMEOUT` stage, because a short "
                         "window that says so is recoverable and one that "
                         "does not is not")
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
    ap.add_argument("--expect-zero-copy-replies", action="store_true",
                    help="fail unless the page observes reply bytes arriving "
                         "without having been copied out of the worker's "
                         "memory (WS3c): out_moved>0 and out_copied==0 within "
                         "--expect-timeout. The page classifies each arriving "
                         "buffer itself, so a transport that reverted to "
                         "copying cannot pass. NEGATIVE CONTROL: run serve.py "
                         "WITHOUT --coep -- with no cross-origin isolation "
                         "every message takes the copying wire and this must "
                         "go red")
    ap.add_argument("--expect-overlay-rasters", action="store_true",
                    help="fail unless the whole-picture overlay path actually "
                         "ran in this browser (WS2 baseline): dispatched>0, "
                         "pictures>0, picture_bytes>0, shown+promoted>0, and "
                         "arrived==pictures+dropped, within --expect-timeout. "
                         "Needs a texture overlay in the scene -- run_tier2.sh "
                         "seeds RadarCoverage, the one that needs no network, so "
                         "the gate does not depend on the weather. NEGATIVE "
                         "CONTROL: the every-layer-off seed written out beside "
                         "SEED_LS; dropping RadarCoverage alone is NOT a control, "
                         "the two default-on texture overlays cover for it")
    ap.add_argument("--expect-seed-applied", action="store_true",
                    help="fail unless THIS BROWSER really applied the scene "
                         "seed: window.__rig.seeded names squallar.ui, a "
                         "`loop state:` line proves the app read that same "
                         "seed object, and neither the timezone-fallback line "
                         "nor a config-refused warning was ever logged. THE "
                         "HOLE THIS CLOSES: the host-side seed tests prove a "
                         "literal PARSES into the claimed scene and say "
                         "nothing about a browser ever reading it -- a leg "
                         "pointed at /index.html gets no prelude, no "
                         "localStorage write, and opens on a site derived "
                         "from the machine's timezone while every other "
                         "assertion stays green. NEGATIVE CONTROL: the defect "
                         "itself, --url .../index.html")
    ap.add_argument("--expect-basemap-tiles", action="store_true",
                    help="fail unless the self-hosted VECTOR BASEMAP really "
                         "decoded tiles in this browser: vector_tiles>0 within "
                         "--expect-timeout. One conjunct, because the basemap "
                         "ships enabled and draws every frame regardless of "
                         "the weather, so there is no configuration in which "
                         "zero is correct. THE HOLE THIS CLOSES: a usize->u64 "
                         "offset widening in vendor/pmtiles made the basemap "
                         "serve zero tiles for as long as that build shipped "
                         "and this rig passed every leg -- an empty map still "
                         "boots, still paints a non-blank canvas, still "
                         "attaches its worker and still satisfies every "
                         "conjunct of --expect-overlay-rasters, which does not "
                         "include the basemap. NEGATIVE CONTROL: revert that "
                         "widening; measured zero in both browsers with every "
                         "other assertion green. Needs "
                         "squallar.raster_telemetry seeded, as "
                         "--expect-overlay-rasters does")
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
    ap.add_argument("--window-skip-loops", type=int, default=0,
                    metavar="N",
                    help="report a SECOND, narrower window beside the default "
                         "one, bracketed from the Nth completed scripted loop "
                         "instead of from the `begin` marker. The begin marker "
                         "is logged at the player's construction -- boot -- so "
                         "the default window contains the boot burst; this one "
                         "is whole scripted loops with boot excluded. Additive: "
                         "the default window is still reported unchanged, so "
                         "rows taken before this existed stay comparable.")
    ap.add_argument("--quiet-window", type=float, default=None,
                    metavar="SECS",
                    help="for a leg that arms NO gesture (scene E1: a loop "
                         "playing with nobody touching it), bin-diff the "
                         "frame families over `first reading + SECS` to the "
                         "newest reading instead of returning no window. A "
                         "WALL-CLOCK bracket, not the app's own markers: the "
                         "basis string says so and such a row is never "
                         "compared to a gestured one. Ignored entirely on a "
                         "leg whose markers appeared.")
    ap.add_argument("--expect-interaction-frames", action="store_true",
                    help="fail unless the scraped `frame service (interact)` "
                         "count STRICTLY INCREASED over the leg -- the count "
                         "assert that proves driven frames really tag as "
                         "interaction (WO-1's deferred mechanical "
                         "non-vacuity). Needs the squallar.frame_telemetry "
                         "seed; a leg that never wrote the line fails with "
                         "that stated. Count only -- no ms figure gates")
    ap.add_argument("--expect-frame-progress", type=float, default=None,
                    metavar="SECONDS",
                    help="LIVENESS BY PROGRESS. Fail unless the app's own "
                         "cumulative `frame cadence` count was still STRICTLY "
                         "INCREASING over the last SECONDS of the leg, and a "
                         "reading landed inside it. This is the only "
                         "assertion in this file that can see a frame loop "
                         "that DIED partway: the canvas keeps its last "
                         "painted frame, requestAnimationFrame keeps firing "
                         "at display rate, async tasks keep logging, and "
                         "every screenshot and rAF check still passes. Needs "
                         "the squallar.frame_telemetry seed; a leg that never "
                         "wrote the line fails with that stated")
    ap.add_argument("--expect-tile-cache-settles", type=float, default=None,
                    metavar="SECONDS",
                    help="fail unless, over the last SECONDS of the leg, the "
                         "deltas of `tile cache (base): refetch after "
                         "eviction`, `ground tiles: uploads` and `basemap "
                         "tiles: vector` are ALL zero -- three denominators, "
                         "each asserted alone. A static viewport whose tiles "
                         "have arrived owes none of them; a tile cache below "
                         "its working set owes all three every frame, "
                         "because it evicts a tile still on the glass, asks "
                         "for it again, decodes it again and uploads it "
                         "again. Silence inside the window is a zero (the "
                         "lines are written only when their ledger moves); a "
                         "line never written at all is an ERROR, never a "
                         "zero. Needs the squallar.raster_telemetry seed. "
                         "Pair with --expect-frame-progress: a dead frame "
                         "loop is silent too, and only the app's own frame "
                         "counter tells the two apart")
    ap.add_argument("--w3c-gesture", choices=("pan", "wheel", "pan+wheel"),
                    default=None,
                    help="drive real input through the driver's W3C /actions "
                         "endpoint after settle: mirrored pointer drags "
                         "and/or wheel notches around the viewport centre, "
                         "net-zero so the scene ends where it began. Verified "
                         "on geckodriver first (Firefox governs); a driver "
                         "that refuses the wheel input source falls back to "
                         "a synthesized WheelEvent, recorded per browser on "
                         "the result as wheel_source")
    ap.add_argument("--gesture-seconds", type=float, default=4.0,
                    help="how long --w3c-gesture drives (default 4)")
    ap.add_argument("--wait-console-regex", action="append", default=[],
                    metavar="REGEX",
                    help="wait until a console-ring line matches this JS "
                         "regex before proceeding (after the app-backend "
                         "wait); repeatable, and each one failing to match "
                         "within --wait-console-timeout fails the run. The "
                         "generic wait-until-console-line primitive")
    ap.add_argument("--wait-console-timeout", type=float, default=60.0)
    ap.add_argument("--android", action="store_true",
                    help="drive the phone's own browser over adb, plus `adb "
                         "reverse tcp:P tcp:P` before navigating so the "
                         "served URL works unchanged on the device. BOTH "
                         "engines: --browser chromium goes through "
                         "chromedriver + goog:chromeOptions.androidPackage, "
                         "--browser firefox through geckodriver + "
                         "moz:firefoxOptions.androidPackage. Needs adb on "
                         "PATH. IMPLEMENTED BUT NOT DEVICE-TESTED: no Android "
                         "device was attached when either engine landed")
    ap.add_argument("--android-package", default=None,
                    help="Android package for --android. Default is resolved "
                         "from --browser: com.android.chrome for chromium, "
                         "org.mozilla.firefox for firefox (Beta is "
                         "org.mozilla.firefox_beta, Nightly org.mozilla.fenix)")
    ap.add_argument("--android-allow-daily-driver", action="store_true",
                    help="drive a release browser package anyway. The drivers "
                         "run `pm clear <package>` before every session "
                         "(MEASURED on geckodriver 0.37.1: it returned "
                         "Success), which DELETES that browser's tabs, "
                         "logins, bookmarks and history. Only for a device "
                         "nobody uses")
    ap.add_argument("--android-use-running-app", action="store_true",
                    help="chromium only: attach to the ALREADY RUNNING "
                         "browser (goog:chromeOptions.androidUseRunningApp) "
                         "instead of restart-and-clear. The rig launches "
                         "the package first, because the capability "
                         "attaches and never starts")
    ap.add_argument("--android-clear-app-data", action="store_true",
                    help="chromium only: DROP androidKeepAppDataDir and let "
                         "chromedriver `pm clear` the package before every "
                         "session. Authorised for com.chrome.beta only. It "
                         "buys a uniform denominator -- every pass starts "
                         "from the same cleared state, so variance is not "
                         "contaminated by a cache warming mid-run -- at the "
                         "cost of being PESSIMISTIC: a cold HTTP cache and "
                         "cold service worker on every pass is not what a "
                         "returning user pays. The row says which via "
                         "profile_state")
    ap.add_argument("--android-activity", default=None,
                    help="moz:firefoxOptions.androidActivity for a Firefox "
                         "build geckodriver has no default for; must not "
                         "contain '/'. Firefox only -- the chromium Android "
                         "path takes no activity")
    ap.add_argument("--adb-serial", default=None,
                    help="adb device serial for --android (default: the one "
                         "attached device). Used for `adb reverse` on both "
                         "engines, and additionally passed to geckodriver as "
                         "moz:firefoxOptions.androidDeviceSerial")
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
                    help="fail the run unless the WebGL renderer OR the "
                         "WebGPU adapter is a real GPU (pair with --arm "
                         "hardware). Two adapters, because a page on the "
                         "WebGPU backend has no WebGL2 context by design; "
                         "the verdict names which one cleared the floor "
                         "(`hardware_via`). Without it the hardware arm "
                         "cannot fail: a silent SwiftShader fallback reads "
                         "exactly like a driver")
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
                         "JSON when possible); repeatable. A pref that "
                         "changes what the app RUNS ON makes the leg a "
                         "non-shipping configuration, and the row has to say "
                         "so: `dom.webgpu.enabled=true` moves linux firefox "
                         "off `Gl` -- which is what ships there -- onto "
                         "BrowserWebGpu. That is worth doing to match another "
                         "platform's backend so a cross-OS pair varies one "
                         "thing, but the resulting figures are the "
                         "experiment's cost, not any user's.")
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
    if args.android:
        # Arg-validated here, loudly, because the mode cannot be smoke-tested
        # without a device: a wrong invocation must fail at the CLI, not
        # three minutes into a phone session. Every rule lives in
        # validate_android_args() so the selftest drives the same code the CLI
        # does, over the whole matrix, with no device and no parser.
        err = validate_android_args(
            args.browser, args.android_package, args.android_activity,
            adb_present=bool(shutil.which("adb")),
            allow_daily_driver=args.android_allow_daily_driver,
            clear_app_data=args.android_clear_app_data)
        if err:
            ap.error(err)
        args.android_package = (args.android_package
                                or ANDROID_DEFAULT_PACKAGE[args.browser])
    if args.adb_serial and not args.android:
        ap.error("--adb-serial is only meaningful with --android")
    if args.android_activity and not args.android:
        ap.error("--android-activity is only meaningful with --android")
    return run_smoke(args)


def _run_cli():
    """`main()` with the box's own failures separated from the app's.

    A full disk raises `OSError: [Errno 122] Disk quota exceeded` from wherever
    the process next touches the filesystem -- which, observed on 2026-08-31,
    was inside a `print()` during teardown, long after every assertion had
    already passed. That escaped as an unhandled traceback and the launcher
    filed it as a failed leg, sending a reader to look for a rendering bug that
    was never there.

    The message is best-effort and the EXIT CODE is not: if stderr is on the
    same full filesystem the write fails too, and the code is then the only
    thing that survives to say what happened.
    """
    try:
        return main()
    except OSError as e:
        if e.errno not in INFRA_ERRNOS:
            raise
        try:
            os.write(2, (
                "\n"
                "!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!\n"
                "INFRASTRUCTURE FAILURE, not a leg failure: %s\n"
                "The BOX ran out of room. Whatever this leg was asserting is\n"
                "UNKNOWN -- it neither passed nor failed. Free space and run\n"
                "it again; do not read this as a defect in the app.\n"
                "!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!\n"
                % e).encode("utf-8", "replace"))
        except OSError:
            pass
        return EXIT_INFRA


if __name__ == "__main__":
    sys.exit(_run_cli())
