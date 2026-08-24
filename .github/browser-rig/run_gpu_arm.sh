#!/usr/bin/env bash
#
# run_gpu_arm.sh -- the HARDWARE arm of the browser rig. Same page, same
# server discipline and same probes as run_tier2.sh; the one difference is
# which adapter answers.
#
# Why this lives in .github/browser-rig/ and not on a harness branch
# ------------------------------------------------------------------
# Campaign instruments never ship on main, and this is not one. It is a
# permanent capability of the rig: the rig's env probe reports MAX_TEXTURE_SIZE,
# MAX_3D_TEXTURE_SIZE, MAX_RENDERBUFFER_SIZE and a WebGPU adapter, and every
# one of those numbers is a property of the ADAPTER rather than of the browser
# or of the build. Without this script the rig can only ever answer for
# SwiftShader and llvmpipe, so any budget keyed on those caps is sized for a
# software rasteriser -- which is exactly how a machine with an RTX 3090 came
# to be described as unable to reach a GPU. The software arm answers "does the
# app work"; this arm answers "what may the app assume". Both are permanent
# questions, so both belong beside the rig they share.
#
# THIS IS NOT A GATE. CI has no GPU; run_tier2.sh remains the gate and its
# software arm is untouched. This script puts a real browser window on the
# user's desktop, because firefox needs a display to reach a driver at all.
# Chromium does not: RIG_DRIVE_EXTRA="--display none" takes its hardware
# figures headless over ANGLE/EGL, measured identical (32768 / 16384 / 32768,
# frame timing slightly tighter). So a GPU-equipped runner could gate on the
# chromium half without a session -- the display requirement is firefox's.
#
# Usage: run_gpu_arm.sh [--skip-build] [--also-software]
#   --skip-build      serve squallar-web as-is (default: wasm-pack build first)
#   --also-software   run the software arm too, into *.sw.json, so the
#                     software-vs-hardware delta comes out of ONE invocation
#                     on ONE build rather than out of two reports
#
# Environment knobs (all optional):
#   SQUALLAR_WEB_DIR   dir to serve   (default <repo>/squallar-web)
#   RIG_OUT_DIR       output dir     (default <rig>/out-gpu)
#   RIG_CHROMEDRIVER  chromedriver   (default: chromedriver on PATH)
#   RIG_GECKODRIVER   geckodriver    (default: $(ensure-geckodriver.sh))
#   RIG_FRAMES        rAF deltas per sample (default 240 -- frame timing is
#                     the figure this arm exists to compare, so it is sampled
#                     harder than the gate samples it)
#   RIG_BROWSERS      "firefox chromium" (default; firefox governs, so it
#                     runs first and is reported first)
#   RIG_DISPLAY       X display to use (default: $DISPLAY, then :0)
#   RIG_SETTLE        seconds before the warm rAF sample (default 6)
#   RIG_DATA_WINDOW   seconds of live data before the second sample (default 8)
#   RIG_DRIVE_EXTRA   extra args appended to every drive.py call
#
# The run is deliberately short: no worker-wire assertions, no doctored-token
# leg, no service-worker legs. Those are behaviour, run_tier2.sh owns them,
# and they are what makes a Tier-2 pass take minutes. This arm wants an
# adapter, four caps, a WebGPU answer and two rAF samples.
#
# Asking the WebGPU question
# --------------------------
# The default hardware arm answers it for the SHIPPING configuration, and on
# 2026-08-22 that answer was "no adapter" in both browsers. WebGPU is reachable
# here, but only off the default path, and each browser charges differently for
# it -- so the question has two more forms, both asked without editing the rig:
#
#   # firefox: pref-gated, and WebGL2 keeps working beside it
#   RIG_BROWSERS=firefox RIG_DRIVE_EXTRA="--ff-pref dom.webgpu.enabled=true" \
#     run_gpu_arm.sh --skip-build
#
#   # chromium: needs the Vulkan ANGLE backend, and LOSES WebGL2 entirely --
#   # the app boots and paints nothing, so --require-hardware fails the run
#   RIG_BROWSERS=chromium RIG_DRIVE_EXTRA="\
#       --chromium-arg=--enable-unsafe-webgpu \
#       --chromium-arg=--enable-features=Vulkan \
#       --chromium-arg=--use-angle=vulkan" run_gpu_arm.sh --skip-build
#
# The `=` form is required: argparse would read a bare `--enable-...` as one of
# its own options.

set -u -o pipefail   # not -e: attempt every leg and still summarise

RIG_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$RIG_DIR/../.." && pwd)"
WEB_DIR="${SQUALLAR_WEB_DIR:-$REPO_ROOT/squallar-web}"
OUT_DIR="${RIG_OUT_DIR:-$RIG_DIR/out-gpu}"
CHROMEDRIVER="${RIG_CHROMEDRIVER:-$(command -v chromedriver || echo /usr/bin/chromedriver)}"
FRAMES="${RIG_FRAMES:-240}"
BROWSERS="${RIG_BROWSERS:-firefox chromium}"
SETTLE="${RIG_SETTLE:-6}"
DATA_WINDOW="${RIG_DATA_WINDOW:-8}"
PY=python3

# Identical to run_tier2.sh's seed: a figure from here and a verdict from
# there must describe the same scene, or the delta is a scene delta.
SEED_LS='{"squallar.ui": "{\"pane_count\":1,\"panes\":[{\"site\":\"KTLX\"}]}"}'

SKIP_BUILD=0
ALSO_SOFTWARE=0
for arg in "$@"; do
  case "$arg" in
    --skip-build)    SKIP_BUILD=1 ;;
    --also-software) ALSO_SOFTWARE=1 ;;
    *) echo "unknown argument: $arg" >&2; exit 1 ;;
  esac
done

# ------------------------------------------------------- display check ----
# Fail here rather than three minutes in with a llvmpipe reading. The cookie
# matters as much as the socket: a browser that opens the socket without it
# dies with "Invalid MIT-MAGIC-COOKIE-1 key", which reads exactly like "there
# is no display on this box" and is how this arm came to be thought impossible.
DISPLAY_INFO="$("$PY" - "$RIG_DIR" "${RIG_DISPLAY:-}" <<'EOF'
import importlib.util, json, os, sys
rig = sys.argv[1]
explicit = sys.argv[2] or None
spec = importlib.util.spec_from_file_location("drive", os.path.join(rig, "drive.py"))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
print(json.dumps(m.resolve_host_display(explicit)))
EOF
)"
if [ -z "$DISPLAY_INFO" ] || ! echo "$DISPLAY_INFO" | grep -q '"display": ":'; then
  echo "FATAL: no usable X display for the hardware arm: $DISPLAY_INFO" >&2
  echo "       (this arm needs a logged-in graphical session; CI has none," >&2
  echo "        which is why run_tier2.sh's software arm exists)" >&2
  exit 1
fi
echo "hardware arm display: $DISPLAY_INFO"

if [ -z "${RIG_GECKODRIVER:-}" ]; then
  RIG_GECKODRIVER="$(bash "$RIG_DIR/ensure-geckodriver.sh")" || {
    echo "FATAL: ensure-geckodriver.sh failed" >&2
    exit 1
  }
fi
GECKODRIVER="$RIG_GECKODRIVER"

mkdir -p "$OUT_DIR"

# ---------------------------------------------------------------- build ----
if [ "$SKIP_BUILD" -eq 0 ]; then
  echo "building squallar-web (wasm-pack, CARGO_BUILD_JOBS=4)"
  # Through wasm-threads.sh since WS3b -- the nightly/atomics/build-std/
  # shared-memory configuration is the only one this bundle compiles in.
  (cd "$REPO_ROOT" && CARGO_BUILD_JOBS=4 \
    .github/scripts/wasm-threads.sh \
    wasm-pack build squallar-web --target web --release --no-typescript --no-pack) || {
    echo "FATAL: wasm-pack build failed" >&2
    exit 1
  }
fi
if [ ! -f "$WEB_DIR/pkg/squallar_web_bg.wasm" ]; then
  echo "FATAL: $WEB_DIR/pkg/squallar_web_bg.wasm missing -- build first" >&2
  exit 1
fi

SERVER_PID=""
stop_server() {
  if [ -n "$SERVER_PID" ]; then
    kill "$SERVER_PID" 2>/dev/null
    wait "$SERVER_PID" 2>/dev/null
    SERVER_PID=""
  fi
}

# A headed run puts windows on a real desktop, so the sweep matters more here
# than it does for the gate: a leaked process group is a leaked window.
cleanup() {
  local rc=$?
  trap - EXIT INT TERM
  stop_server
  local f pgid
  for f in "$OUT_DIR"/*.driver.pgid; do
    [ -f "$f" ] || continue
    pgid="$(cat "$f" 2>/dev/null)"
    if [ -n "$pgid" ]; then
      echo "cleanup: killing leftover driver process group $pgid ($f)" >&2
      kill -TERM -- "-$pgid" 2>/dev/null
    fi
    rm -f "$f"
  done
  sleep 0.5
  for f in "$OUT_DIR"/*.driver.pgid; do
    [ -f "$f" ] || continue
    pgid="$(cat "$f" 2>/dev/null)"
    [ -n "$pgid" ] && kill -KILL -- "-$pgid" 2>/dev/null
    rm -f "$f"
  done
  exit "$rc"
}
trap cleanup EXIT INT TERM

start_server() {
  local ready_file="$OUT_DIR/serve.ready"
  : > "$ready_file"
  # --coep since WS3b: without cross-origin isolation there is no
  # SharedArrayBuffer, the rasterization worker falls back to a one-thread
  # rayon pool, and this arm would report a rasterization time for a
  # configuration the app is never deployed in. Production serves the headers.
  "$PY" "$RIG_DIR/serve.py" --dir "$WEB_DIR" --port 0 \
      --log "$OUT_DIR/serve.log" \
      --seed-local-storage "$SEED_LS" --coep \
      > "$ready_file" 2>> "$OUT_DIR/serve.stderr" &
  SERVER_PID=$!
  PORT=""
  local _tag _base
  for _ in $(seq 1 100); do
    if ! kill -0 "$SERVER_PID" 2>/dev/null; then
      echo "FATAL: serve.py exited early:" >&2
      tail -20 "$OUT_DIR/serve.stderr" >&2
      return 1
    fi
    if [ -s "$ready_file" ]; then
      read -r _tag PORT _base < "$ready_file"
      break
    fi
    sleep 0.1
  done
  if [ -z "$PORT" ]; then
    echo "FATAL: serve.py never printed its ready line" >&2
    return 1
  fi
  URL="http://127.0.0.1:$PORT/index-rig.html"
  echo "serving $WEB_DIR on port $PORT (pid $SERVER_PID) -> $URL"
  return 0
}

EXTRA=()
if [ -n "${RIG_DRIVE_EXTRA:-}" ]; then
  # shellcheck disable=SC2206
  EXTRA+=($RIG_DRIVE_EXTRA)
fi

# run_pass <browser> <tag> <arm>
run_pass() {
  local browser="$1" tag="$2" arm="$3"
  local driver arm_args=()
  case "$browser" in
    chromium) driver="$CHROMEDRIVER" ;;
    firefox)  driver="$GECKODRIVER" ;;
    *) echo "unknown browser: $browser" >&2; return 1 ;;
  esac
  if [ "$arm" = hardware ]; then
    # --require-hardware is the non-triviality floor: without it this arm
    # cannot come back false, because a silent software fallback produces the
    # same clean, plausible JSON as a driver does.
    arm_args+=(--arm hardware --require-hardware)
    [ -n "${RIG_DISPLAY:-}" ] && arm_args+=(--display "$RIG_DISPLAY")
  else
    arm_args+=(--arm software)
  fi
  start_server || return 1
  "$PY" "$RIG_DIR/drive.py" \
      --browser "$browser" --url "$URL" \
      --out-dir "$OUT_DIR" --tag "$tag" \
      --driver "$driver" --frames "$FRAMES" \
      --settle "$SETTLE" --data-window "$DATA_WINDOW" \
      "${arm_args[@]}" \
      ${EXTRA[@]+"${EXTRA[@]}"}
  local rc=$?
  stop_server
  return "$rc"
}

overall=0
TAGS=""
for browser in $BROWSERS; do
  echo
  echo "================ $browser (hardware arm) ================"
  TAGS="$TAGS $browser.hw"
  run_pass "$browser" "$browser.hw" hardware || overall=1
  if [ "$ALSO_SOFTWARE" -eq 1 ]; then
    echo
    echo "================ $browser (software arm, control) ================"
    TAGS="$TAGS $browser.sw"
    run_pass "$browser" "$browser.sw" software || overall=1
  fi
done

# -------------------------------------------------------------- summary ----
echo
echo "================ gpu-arm summary ================"
"$PY" - "$OUT_DIR" $TAGS <<'EOF'
import json, os, sys
out = sys.argv[1]

rows = []
for tag in sys.argv[2:]:
    p = os.path.join(out, tag + ".json")
    if not os.path.isfile(p):
        print("%-16s NO RESULT (%s missing)" % (tag, p))
        continue
    rows.append((tag, json.load(open(p))))

def adapter(r):
    a = r.get("adapter") or {}
    cls = a.get("class", "?")
    return cls, (a.get("renderer") or "?")

def webgpu(r):
    wg = ((r.get("env") or {}).get("webgpu")) or {}
    if wg.get("probe_error"):
        return "probe-error"
    if not wg.get("gpu_object"):
        return "absent"
    if not wg.get("adapter"):
        return "object-but-no-adapter"
    lim = wg.get("adapter_limits") or {}
    info = wg.get("adapter_info") or {}
    return "adapter %s (maxTex2D=%s)" % (
        info.get("description") or info.get("vendor") or "?",
        lim.get("maxTextureDimension2D"))

def raf(r, key):
    d = r.get(key) or {}
    return ("p50=%.2f p95=%.2f p99=%.2f" % (d["p50"], d["p95"], d["p99"])
            if d.get("ok") else "FAILED")

# Every row names its adapter. Nothing here is ever averaged across browsers
# or across arms: two browsers are two targets and two arms are two machines.
for tag, r in rows:
    cls, rend = adapter(r)
    env = r.get("env") or {}
    v = r.get("verdict") or {}
    print("%-16s arm=%-8s adapter=%s (%s)"
          % (tag, r.get("arm"), cls, rend))
    print("%-16s   pass=%s booted=%s canvas_blank=%s hardware_ok=%s"
          % ("", r.get("pass"), v.get("booted"),
             (r.get("screenshots") or {}).get("canvas", {}).get("blank"),
             v.get("hardware_ok")))
    print("%-16s   [%s] max_texture=%s max_3d=%s max_renderbuffer=%s"
          % ("", rend, env.get("max_texture_size"),
             env.get("max_3d_texture_size"), env.get("max_renderbuffer_size")))
    print("%-16s   [%s] webgpu %s" % ("", rend, webgpu(r)))
    print("%-16s   [%s] raf warm  %s" % ("", rend, raf(r, "raf_warm")))
    print("%-16s   [%s] raf later %s" % ("", rend, raf(r, "raf_later")))
    if r.get("exception"):
        print("%-16s   failed at stage %r: %s"
              % ("", r.get("failed_stage"), r.get("exception")))
    for g in (r.get("gotchas") or []):
        print("%-16s   gotcha: %s" % ("", g))

# The delta, per browser, only where BOTH arms ran in this invocation. A
# hardware figure next to a software figure taken on another build or another
# day is not a delta.
by_browser = {}
for tag, r in rows:
    by_browser.setdefault(r.get("browser"), {})[r.get("arm")] = r
pairs = [(b, a) for b, a in by_browser.items()
         if "hardware" in a and "software" in a]
if pairs:
    print()
    print("---- software -> hardware delta (same build, same invocation) ----")
    for b, arms in pairs:
        hw, sw = arms["hardware"], arms["software"]
        henv, senv = hw.get("env") or {}, sw.get("env") or {}
        print("%s: adapter %r -> %r"
              % (b, (sw.get("adapter") or {}).get("renderer"),
                 (hw.get("adapter") or {}).get("renderer")))
        for k, label in (("max_texture_size", "MAX_TEXTURE_SIZE"),
                         ("max_3d_texture_size", "MAX_3D_TEXTURE_SIZE"),
                         ("max_renderbuffer_size", "MAX_RENDERBUFFER_SIZE")):
            print("  %-22s %s -> %s" % (label, senv.get(k), henv.get(k)))
        for key in ("raf_warm", "raf_later"):
            d0, d1 = sw.get(key) or {}, hw.get(key) or {}
            if d0.get("ok") and d1.get("ok"):
                print("  %-22s p50 %.2f -> %.2f ms, p95 %.2f -> %.2f ms"
                      % (key, d0["p50"], d1["p50"], d0["p95"], d1["p95"]))
        print("  %-22s %s -> %s" % ("webgpu", webgpu(sw), webgpu(hw)))
EOF

echo
echo "artifacts in $OUT_DIR:"
ls -l "$OUT_DIR" | sed 's/^/  /'
exit "$overall"
