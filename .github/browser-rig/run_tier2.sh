#!/usr/bin/env bash
#
# run_tier2.sh -- the Tier-2 browser gate: serve the built rustdar-web bundle
# on a fresh port, drive Chromium then Firefox against the full PWA, write
# out/<browser>.json + screenshots, clean up every process via traps (no
# orphan servers/drivers even on ^C).
#
# Adapted from the 2026-08-18 measurement rig's run_smoke.sh; this copy is the
# permanent CI gate. Paths are derived from the script location (this file
# lives at .github/browser-rig/ inside the repo), never from any scratchpad.
#
# Usage: run_tier2.sh [--skip-build]
#   --skip-build      serve rustdar-web as-is (CI builds in its own step; the
#                     default is a fresh `wasm-pack build` first)
#
# Environment knobs (all optional):
#   RUSTDAR_WEB_DIR   dir to serve   (default <repo>/rustdar-web)
#   RIG_OUT_DIR       output dir     (default <rig>/out)
#   RIG_CHROMEDRIVER  chromedriver   (default /usr/bin/chromedriver)
#   RIG_GECKODRIVER   geckodriver    (default: $(ensure-geckodriver.sh))
#   RIG_FRAMES        rAF deltas per sample (default 120)
#   RIG_BROWSERS      "chromium firefox" (default), or a subset
#   RIG_DRIVE_EXTRA   extra args appended to every drive.py call

set -u -o pipefail   # not -e: attempt BOTH browsers and still summarise

RIG_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$RIG_DIR/../.." && pwd)"
WEB_DIR="${RUSTDAR_WEB_DIR:-$REPO_ROOT/rustdar-web}"
OUT_DIR="${RIG_OUT_DIR:-$RIG_DIR/out}"
CHROMEDRIVER="${RIG_CHROMEDRIVER:-/usr/bin/chromedriver}"
FRAMES="${RIG_FRAMES:-120}"
BROWSERS="${RIG_BROWSERS:-chromium firefox}"
PY=python3

SKIP_BUILD=0
for arg in "$@"; do
  case "$arg" in
    --skip-build) SKIP_BUILD=1 ;;
    *) echo "unknown argument: $arg" >&2; exit 1 ;;
  esac
done

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
  echo "building rustdar-web (wasm-pack; --skip-build to serve as-is)"
  (cd "$REPO_ROOT" &&
    wasm-pack build rustdar-web --target web --release --no-typescript --no-pack) || {
    echo "FATAL: wasm-pack build failed" >&2
    exit 1
  }
fi
if [ ! -f "$WEB_DIR/pkg/rustdar_web_bg.wasm" ]; then
  echo "FATAL: $WEB_DIR/pkg/rustdar_web_bg.wasm missing -- build first" >&2
  exit 1
fi

SERVER_PID=""
cleanup() {
  local rc=$?
  trap - EXIT INT TERM
  # 1. the static server
  if [ -n "$SERVER_PID" ]; then
    kill "$SERVER_PID" 2>/dev/null
  fi
  # 2. any driver process group drive.py did not get to tear down
  #    (drive.py removes its pgid file on a clean stop)
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
  [ -n "$SERVER_PID" ] && kill -9 "$SERVER_PID" 2>/dev/null
  for f in "$OUT_DIR"/*.driver.pgid; do
    [ -f "$f" ] || continue
    pgid="$(cat "$f" 2>/dev/null)"
    [ -n "$pgid" ] && kill -KILL -- "-$pgid" 2>/dev/null
    rm -f "$f"
  done
  exit "$rc"
}
trap cleanup EXIT INT TERM

# ---------------------------------------------------------------- serve ----
# Fresh (kernel-chosen) port every run: no stale service-worker scope, no
# cross-run cache identity. serve.py prints one ready line to stdout.
READY_FILE="$OUT_DIR/serve.ready"
: > "$READY_FILE"
"$PY" "$RIG_DIR/serve.py" --dir "$WEB_DIR" --port 0 \
    --log "$OUT_DIR/serve.log" > "$READY_FILE" 2>> "$OUT_DIR/serve.stderr" &
SERVER_PID=$!

PORT=""
for _ in $(seq 1 100); do
  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    echo "FATAL: serve.py exited early:" >&2
    cat "$OUT_DIR/serve.stderr" >&2
    exit 1
  fi
  if [ -s "$READY_FILE" ]; then
    read -r _tag PORT _base < "$READY_FILE"
    break
  fi
  sleep 0.1
done
if [ -z "$PORT" ]; then
  echo "FATAL: serve.py never printed its ready line" >&2
  exit 1
fi
URL="http://127.0.0.1:$PORT/index-rig.html"
echo "serving $WEB_DIR on port $PORT (pid $SERVER_PID) -> $URL"

# ---------------------------------------------------------------- drive ----
EXTRA=()
if [ -n "${RIG_DRIVE_EXTRA:-}" ]; then
  # shellcheck disable=SC2206
  EXTRA+=($RIG_DRIVE_EXTRA)
fi

overall=0
for browser in $BROWSERS; do
  echo
  echo "================ $browser ================"
  case "$browser" in
    chromium) DRIVER="$CHROMEDRIVER" ;;
    firefox)  DRIVER="$GECKODRIVER" ;;
    *) echo "unknown browser: $browser" >&2; overall=1; continue ;;
  esac
  "$PY" "$RIG_DIR/drive.py" \
      --browser "$browser" --url "$URL" \
      --out-dir "$OUT_DIR" --tag "$browser" \
      --driver "$DRIVER" --frames "$FRAMES" \
      ${EXTRA[@]+"${EXTRA[@]}"}
  rc=$?
  if [ "$rc" -ne 0 ]; then
    echo "$browser leg exited rc=$rc" >&2
    overall=1
  fi
done

# -------------------------------------------------------------- summary ----
echo
echo "================ tier2 summary ================"
"$PY" - "$OUT_DIR" $BROWSERS <<'EOF'
import json, os, sys
out = sys.argv[1]
for tag in sys.argv[2:]:
    p = os.path.join(out, tag + ".json")
    if not os.path.isfile(p):
        print("%-9s NO RESULT (%s missing)" % (tag, p)); continue
    r = json.load(open(p))
    v = r.get("verdict") or {}
    env = r.get("env") or {}
    b = r.get("canvas_final") or (r.get("boot") or {}).get("probe") or {}
    rw = r.get("raf_warm") or {}
    rl = r.get("raf_later") or {}
    def raf(d):
        return ("p50=%.2f p95=%.2f max=%.2f" % (d["p50"], d["p95"], d["max"])
                if d.get("ok") else "FAILED")
    sh = r.get("screenshots") or {}
    print("%-9s %s  boot=%s canvas=%sx%s raf_warm[%s] raf_later[%s] "
          "page_blank=%s canvas_blank=%s errors=%s panics=%s gl=%s"
          % (tag, "PASS" if r.get("pass") else "FAIL",
             v.get("booted"), b.get("clientWidth"), b.get("clientHeight"),
             raf(rw), raf(rl) if rl else "-",
             (sh.get("page") or {}).get("blank"),
             (sh.get("canvas") or {}).get("blank"),
             v.get("rig_error_count"), v.get("panic_count"),
             (env.get("gl_renderer") or env.get("webgl") or "?")))
    if v.get("first_panic"):
        print("%-9s first panic: %s" % ("", v["first_panic"][:180]))
    if r.get("exception"):
        print("%-9s failed at stage %r: %s"
              % ("", r.get("failed_stage"), r.get("exception")))
EOF

echo
echo "artifacts in $OUT_DIR:"
ls -l "$OUT_DIR" | sed 's/^/  /'
exit "$overall"
