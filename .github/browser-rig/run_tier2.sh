#!/usr/bin/env bash
#
# run_tier2.sh -- the Tier-2 browser gate: serve the built rustdar-web bundle
# on a fresh port and drive the full PWA in Chromium and Firefox. Two passes
# per browser:
#
#   live      the app against LIVE network; asserts boot, canvas non-blank,
#             rAF sane, zero panics/errors, AND the worker wire: >=1
#             "rasterization worker attached" plus >=1 "took N ms off the
#             frame" job reply within 180 s (m4 -- a booted page with a dead
#             wire passes every weaker check).
#   doctored  serve.py --doctor-first-worker hands the FIRST /worker.js
#             request a stub posting a doctored build token; asserts the page
#             logs "rasterization worker is a different build", terminates,
#             and >=1000 ms later (first backoff rung) attaches the REAL
#             refetched worker (m5), and then the m4 round-trip on top.
#
# Network posture (campaign-resolved): LIVE network, one auto-retry per pass
# as the flake-quarantine policy -- the app's own backoff machinery is part of
# what is exercised. A pass that fails twice fails the leg.
#
# The scene is pinned by seeding localStorage with a PANE on KTLX before any
# app script runs (browsers would otherwise pick different default sites).
# There is no app-wide site to seed since WO-SITE: a pane carries its own.
#
# Adapted from the 2026-08-18 measurement rig's run_smoke.sh; this copy is the
# permanent CI gate. Paths derive from the script location (this file lives at
# .github/browser-rig/ inside the repo), never from any scratchpad.
#
# Usage: run_tier2.sh [--skip-build]
#   --skip-build      serve rustdar-web as-is (CI builds in its own step; the
#                     default is a fresh `wasm-pack build` first)
#
# Environment knobs (all optional):
#   RUSTDAR_WEB_DIR   dir to serve   (default <repo>/rustdar-web)
#   RIG_OUT_DIR       output dir     (default <rig>/out)
#   RIG_CHROMEDRIVER  chromedriver   (default: chromedriver on PATH, else
#                                     /usr/bin/chromedriver)
#   RIG_GECKODRIVER   geckodriver    (default: $(ensure-geckodriver.sh))
#   RIG_FRAMES        rAF deltas per sample (default 120)
#   RIG_BROWSERS      "chromium firefox" (default), or a subset
#   RIG_EXPECT_TIMEOUT  seconds for the worker-wire assertions (default 180)
#   RIG_DRIVE_EXTRA   extra args appended to every drive.py call
#   RIG_SERVE_EXTRA   extra args appended to every serve.py launch
#
# The cross-origin-isolation proof (WS3a) is these two together -- serve the
# isolation headers AND let the real service worker through, then assert both
# sides rather than trusting the header was honoured:
#
#   RIG_SERVE_EXTRA="--coep --no-block-sw" \
#   RIG_DRIVE_EXTRA="--expect-cross-origin-isolated --expect-service-worker" \
#     run_tier2.sh --skip-build
#
# Without --expect-cross-origin-isolated the run proves nothing: a browser
# that ignored the headers looks exactly like one that honoured them.
#
# ADAPTER: this gate is the rig's SOFTWARE arm, deliberately and permanently.
# Chromium runs on SwiftShader and Firefox on Xvfb -> Mesa llvmpipe, so it is
# deterministic and runs on a CI box with no GPU. Every cap it reports
# (MAX_TEXTURE_SIZE and friends) therefore describes a software rasteriser and
# must never be promoted into a budget. The real-driver figures come from
# run_gpu_arm.sh, which is a measurement arm and not a gate. The two are never
# merged; each prints its arm and renderer beside every cap it quotes.

set -u -o pipefail   # not -e: attempt every leg and still summarise

RIG_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$RIG_DIR/../.." && pwd)"
WEB_DIR="${RUSTDAR_WEB_DIR:-$REPO_ROOT/rustdar-web}"
OUT_DIR="${RIG_OUT_DIR:-$RIG_DIR/out}"
CHROMEDRIVER="${RIG_CHROMEDRIVER:-$(command -v chromedriver || echo /usr/bin/chromedriver)}"
FRAMES="${RIG_FRAMES:-120}"
BROWSERS="${RIG_BROWSERS:-chromium firefox}"
EXPECT_TIMEOUT="${RIG_EXPECT_TIMEOUT:-180}"
PY=python3

# UiConfig is #[serde(default)], so a config this partial parses; the key is
# the app's own real localStorage key. The site rides in a PANE, because that
# is the only place a site lives -- the app-wide `site` key was retired at
# WO-SITE, and a config carrying one seeds nothing now.
#
# Written in the OLDEST config shape on purpose: no `config_version` reads as
# version 1, so the seed walks the whole migration chain up to whatever this
# build speaks. The rig then needs no edit when CONFIG_VERSION moves, and the
# chain is exercised on every Tier-2 run.
SEED_LS='{"rustdar.ui": "{\"pane_count\":1,\"panes\":[{\"site\":\"KTLX\"}]}"}'

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

SERVE_EXTRA=()
if [ -n "${RIG_SERVE_EXTRA:-}" ]; then
  # shellcheck disable=SC2206
  SERVE_EXTRA+=($RIG_SERVE_EXTRA)
fi

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
stop_server() {
  if [ -n "$SERVER_PID" ]; then
    kill "$SERVER_PID" 2>/dev/null
    wait "$SERVER_PID" 2>/dev/null
    SERVER_PID=""
  fi
}

cleanup() {
  local rc=$?
  trap - EXIT INT TERM
  stop_server
  # any driver process group drive.py did not get to tear down
  # (drive.py removes its pgid file on a clean stop)
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

# ---------------------------------------------------------------- serve ----
# Fresh (kernel-chosen) port every pass: no stale service-worker scope, no
# cross-run cache identity, and a fresh --doctor-first-worker arm each time.
# Sets PORT and URL; returns non-zero on failure.
start_server() {
  local ready_file="$OUT_DIR/serve.ready"
  : > "$ready_file"
  "$PY" "$RIG_DIR/serve.py" --dir "$WEB_DIR" --port 0 \
      --log "$OUT_DIR/serve.log" \
      --seed-local-storage "$SEED_LS" \
      "$@" ${SERVE_EXTRA[@]+"${SERVE_EXTRA[@]}"} \
      > "$ready_file" 2>> "$OUT_DIR/serve.stderr" &
  SERVER_PID=$!
  PORT=""
  local _tag _base
  for _ in $(seq 1 100); do
    if ! kill -0 "$SERVER_PID" 2>/dev/null; then
      echo "FATAL: serve.py exited early:" >&2
      cat "$OUT_DIR/serve.stderr" >&2
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

# ---------------------------------------------------------------- drive ----
EXTRA=()
if [ -n "${RIG_DRIVE_EXTRA:-}" ]; then
  # shellcheck disable=SC2206
  EXTRA+=($RIG_DRIVE_EXTRA)
fi
if [ ${#SERVE_EXTRA[@]} -gt 0 ]; then
  echo "serve.py extra args: ${SERVE_EXTRA[*]}"
fi
if [ ${#EXTRA[@]} -gt 0 ]; then
  echo "drive.py extra args: ${EXTRA[*]}"
fi

# run_pass <browser> <tag> <doctored 0|1>: one server + one drive.py run.
run_pass() {
  local browser="$1" tag="$2" doctored="$3"
  local driver server_args=() drive_args=()
  case "$browser" in
    chromium) driver="$CHROMEDRIVER" ;;
    firefox)  driver="$GECKODRIVER" ;;
    *) echo "unknown browser: $browser" >&2; return 1 ;;
  esac
  drive_args+=(--expect-worker-round-trip --expect-timeout "$EXPECT_TIMEOUT")
  if [ "$doctored" -eq 1 ]; then
    server_args+=(--doctor-first-worker)
    drive_args+=(--expect-doctored-respawn)
  fi
  start_server ${server_args[@]+"${server_args[@]}"} || return 1
  "$PY" "$RIG_DIR/drive.py" \
      --browser "$browser" --url "$URL" \
      --out-dir "$OUT_DIR" --tag "$tag" \
      --driver "$driver" --frames "$FRAMES" \
      "${drive_args[@]}" \
      ${EXTRA[@]+"${EXTRA[@]}"}
  local rc=$?
  stop_server
  return "$rc"
}

overall=0
TAGS=""
for browser in $BROWSERS; do
  for leg in live doctored; do
    if [ "$leg" = doctored ]; then
      tag="$browser.doctored"; doctored=1
    else
      tag="$browser"; doctored=0
    fi
    TAGS="$TAGS $tag"
    echo
    echo "================ $tag ================"
    if run_pass "$browser" "$tag" "$doctored"; then
      continue
    fi
    # Retry-once quarantine (live-network flake policy): a fresh server, a
    # fresh port, a fresh browser profile. A second failure fails the leg.
    echo "$tag FAILED; one quarantine retry (live-network flake policy)" >&2
    echo "================ $tag (retry) ================"
    if ! run_pass "$browser" "$tag" "$doctored"; then
      echo "$tag failed twice" >&2
      overall=1
    fi
  done
done

# -------------------------------------------------------------- summary ----
echo
echo "================ tier2 summary ================"
"$PY" - "$OUT_DIR" $TAGS <<'EOF'
import json, os, sys
out = sys.argv[1]
for tag in sys.argv[2:]:
    p = os.path.join(out, tag + ".json")
    if not os.path.isfile(p):
        print("%-18s NO RESULT (%s missing)" % (tag, p)); continue
    r = json.load(open(p))
    v = r.get("verdict") or {}
    env = r.get("env") or {}
    b = r.get("canvas_final") or (r.get("boot") or {}).get("probe") or {}
    rw = r.get("raf_warm") or {}
    def raf(d):
        return ("p50=%.2f p95=%.2f" % (d["p50"], d["p95"])
                if d.get("ok") else "FAILED")
    sh = r.get("screenshots") or {}
    wrt = r.get("worker_round_trip")
    dr = r.get("doctored_respawn")
    def tri(x):
        return "-" if x is None else ("ok" if x.get("ok") else "FAIL")
    print("%-18s %s  boot=%s canvas=%sx%s raf[%s] canvas_blank=%s "
          "errors=%s panics=%s round_trip=%s respawn=%s"
          % (tag, "PASS" if r.get("pass") else "FAIL",
             v.get("booted"), b.get("clientWidth"), b.get("clientHeight"),
             raf(rw),
             (sh.get("canvas") or {}).get("blank"),
             v.get("rig_error_count"), v.get("panic_count"),
             tri(wrt), tri(dr)))
    wg = env.get("webgpu") or {}
    if wg.get("gpu_object") is None and not wg.get("probe_error"):
        webgpu = "-"
    elif wg.get("probe_error"):
        webgpu = "probe-error"
    elif not wg.get("gpu_object"):
        webgpu = "absent"
    elif wg.get("adapter"):
        webgpu = "adapter(maxTex2D=%s)" % (
            (wg.get("adapter_limits") or {}).get("maxTextureDimension2D"))
    else:
        webgpu = "object-but-no-adapter"
    sw = r.get("service_worker") or {}
    # The caps are properties of the ADAPTER, and this gate runs the SOFTWARE
    # arm, so they describe SwiftShader / llvmpipe and not the machine. Name
    # the arm and the renderer on the same line as the numbers -- a cap quoted
    # without its adapter is how a budget gets sized for a rasteriser.
    ad = r.get("adapter") or {}
    print("%-18s   arm=%s adapter=%s (%s)"
          % ("", r.get("arm", "software"), ad.get("class", "?"),
             ad.get("renderer") or env.get("gl_renderer") or "?"))
    print("%-18s   caps [%s] max_texture=%s max_3d=%s cores=%s webgpu=%s"
          % ("", ad.get("renderer") or env.get("gl_renderer") or "?",
             env.get("max_texture_size"), env.get("max_3d_texture_size"),
             env.get("hardware_concurrency"), webgpu))
    print("%-18s   isolation crossOriginIsolated=%s SAB=%s sw_blocked=%s "
          "sw_regs=%s resource_failures=%s"
          % ("", env.get("cross_origin_isolated"),
             env.get("shared_array_buffer"), sw.get("blocked_by_rig"),
             len(sw.get("registrations") or []),
             len((r.get("resources") or {}).get("failed") or [])))
    if sw.get("expect_error"):
        print("%-18s   sw EXPECT FAILED: %s" % ("", sw["expect_error"]))
    for f in ((r.get("resources") or {}).get("failed") or [])[:8]:
        print("%-18s   resource status=%s %s" % ("", f.get("status"), f.get("u")))
    if dr and dr.get("ok"):
        print("%-18s   respawn attach %.0f ms after the doctored refusal"
              % ("", dr.get("delta_ms", -1)))
    for d in (wrt, dr):
        if d and not d.get("ok"):
            print("%-18s   %s" % ("", d.get("error")))
    if v.get("first_panic"):
        print("%-18s first panic: %s" % ("", v["first_panic"][:180]))
    if r.get("exception"):
        print("%-18s failed at stage %r: %s"
              % ("", r.get("failed_stage"), r.get("exception")))
EOF

echo
echo "artifacts in $OUT_DIR:"
ls -l "$OUT_DIR" | sed 's/^/  /'
exit "$overall"
