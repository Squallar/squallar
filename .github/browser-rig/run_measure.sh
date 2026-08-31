#!/usr/bin/env bash
#
# run_measure.sh -- the 250 Hz measurement arm of the browser rig: the four
# campaign scenes, the in-app gesture player, and gesture-windowed frame
# percentiles with every denominator printed on the row.
#
# THIS IS NOT A GATE. Nothing here fails CI, no ms figure it prints may ever
# gate anything, and CI has no GPU to run it on. run_tier2.sh remains the
# behavioural gate; run_gpu_arm.sh remains the adapter-caps arm; this script
# answers one question those cannot: what does an interaction frame cost, per
# scene, per browser, on a real driver, counted only inside the gesture
# window. Like run_gpu_arm.sh it puts real browser windows on the desktop --
# firefox cannot reach a driver any other way.
#
# The scenes (seeded via localStorage before any app script runs; the seeds
# arm the in-app gesture player, which injects through the app's own input
# path and writes the marker lines drive.py brackets its bin-diffs with):
#
#   A  1 pane, KTLX, ALL 17 layers (LAYER_ID_LEDGER minus the retired
#      FakeSource), pan-zoom-2d -- the 2D worst case.
#   B  1 pane, KTLX, "render":"Volume", orbit-3d -- the 3D scene. Rows carry
#      the terrain-campaign denominator columns (GroundPass / heights /
#      sun_lighting / GRID_ASSET / height-archive generation / buildings),
#      scraped from the sources at run time: the day the terrain wiring lands
#      those flip, and a scene-B row without them silently becomes
#      incomparable to every row before it.
#   C  6 panes (3 volume + 3 plan, two sites), pan-zoom-2d -- the many-pane
#      worst case.
#   D  scene A's layer stack driven by ui-sweep -- the UI-responsiveness
#      scenario (toggles, panels, slider) through the click registry.
#
# Every row prints its full denominator set: scene, browser, arm, adapter,
# app-selected backend, canvas buffer resolution, dpr, observed refresh
# (from the idle rAF sample), crossOriginIsolated, diagnostics panel state,
# script, commit, and the whole-picture overlay raster SIZE. A row with
# coi!=true is stamped INVALID -- without cross-origin isolation there is no
# SharedArrayBuffer, the worker pool is the one-thread fallback, and the row
# describes a threading configuration the app never ships in. Firefox and
# Chromium rows are never merged; Firefox governs and runs first.
#
# ---- Pixels and MB/picture are denominators, not details ------------------
#
# Scene A's interact p99 tracks the size of the whole-picture overlay raster.
# That size is a pure function of the surface the app was given -- the canvas
# on the web, the window on native -- and not of the build:
#
#     picture bytes = (W * 1.5) * ((H - 40) * 1.5) * 4
#
# (the 1.5 is `OVERDRAW_FRACTION` 0.25 spent on both sides, the 40 is the top
# bar in points, the 4 is RGBA). Verified exact at three surfaces on
# 2026-08-31: native 1920x1080 -> 17,971,200 B; firefox canvas 1280x779 ->
# 8,509,440 B; chromium canvas 1248x714 -> 7,570,368 B.
#
# Three ways that has already produced an incomparable pair:
#
#   * **Native leg vs native leg.** A window-manager fullscreen request that
#     raced on some runs and landed on others: 17,971,200 B a picture at the
#     app's own 1920x1080 default, 43,344,000 B at a 3440x1440 fullscreen
#     window -- 2.4x -- between two runs of "the same protocol".
#   * **Firefox vs chromium.** One `--window 1280x900` gives the two browsers
#     canvases of 1280x779 and 1248x714: 1.00M vs 0.89M pixels, a 12%
#     difference between the two rows the campaign reads side by side.
#   * **Native vs web.** 1920x1080 is 2.07M pixels against the rig's ~1.0M --
#     2.1x. A native p99 quoted beside a web p99 without this stated is a
#     comparison of two viewport sizes.
#
# So: **a pair whose viewport pixels differ is not a comparison.** Every row
# prints `viewport=`, `px=`, `dpr=`, `pictures=` and `MB/picture=`.
#
# ---- The cross-target rule: match the pixels, or do not compare ------------
#
# MATCHING IS CHOSEN over a "not cross-comparable" marker, because matching is
# cheap and a marker only tells you afterwards that the run was wasted:
#
#   * Web: `RIG_CANVAS=WxH` passes `--canvas` to drive.py, which corrects the
#     window until the canvas DRAWING BUFFER is exactly that, and records
#     whether it got there (`canvas met=`). Same pixels in both browsers.
#   * Native: run the window at the same WxH (the runner's `--geom`), which
#     produces the identical picture -- the formula above has no target in it.
#
# The marker is kept as the fallback, because an unmet target must not read as
# a met one: a row whose `--canvas` was not requested, or was requested and
# missed, prints `cross=no` and may not be quoted beside a row from another
# target. `cross=yes` means the buffer is exactly the asked-for size.
#
# ---- Basemap state is a denominator too -----------------------------------
#
# Every web leg measured before `a7465238` (2026-08-31) drew NO basemap: the
# pmtiles offsets were truncated to 32 bits, so no vector tile ever resolved
# on wasm32 and the web legs skipped tile placement, tessellation and tile
# uploads entirely while the native legs did all of it. Rows now print
# `basemap=<decoded>/<placed>` off two independent counters, so a pre-fix row
# can never again be quoted beside a post-fix one:
#
#   * DECODED is `basemap tiles:` -- archive tile bodies that came back. A leg
#     on the wrong side of the truncation reads `none-decoded`.
#   * PLACED is `ground tiles:` -- what the ground phase then put on the frame
#     thread. A leg can decode tiles and place nothing, which is why this is
#     not folded into the first. `placed` alone is NOT the test here: the
#     fills went to the GPU, so a drawing pane is legitimately zero in that
#     one field and positive in stroke points, labels, draws and uploads.
#
# ---- The native protocol ---------------------------------------------------
#
# There is no native runner in the tree (campaign instruments live on harness
# branches). What a native leg must do, so a later lane can reproduce a row:
#
#   1. Release binary, fresh XDG_CONFIG_HOME, the scene's ui.json seeded into
#      $XDG_CONFIG_HOME/squallar/ui.json with the same shape as scene_seed
#      below, plus frame_telemetry=1 and raster_telemetry=1.
#   2. SQUALLAR_GESTURE_SCRIPT=<the scene's script> in the environment.
#   3. **Pin the window size and VERIFY it.** The app requests
#      RENDER_WIDTH x RENDER_HEIGHT (1920x1080) at create and persists no
#      geometry, so a leg that touches no window manager is already pinned;
#      a leg that wants another size must resize BY WINDOW ID (`xdotool
#      search --pid`, never by title -- `wmctrl -r Squallar` matches any
#      window whose title contains the string) and then read the geometry
#      back. A leg that cannot show the size it asked for is INVALID.
#   4. Settle 30 s, then bracket exactly two WHOLE script loops using the
#      player's own `gesture script <name> loop complete` markers, and diff
#      the embedded histograms across the bracket. Percentiles do not
#      difference; histograms do.
#   5. Diff `overlay rasters:` across the SAME bracket and report
#      `picture_bytes / pictures` as the row's MB/picture. Cumulative-from-
#      boot folds in whatever was drawn before the window settled.
#   6. Two runs; a third when they diverge by more than 15%.
#
# Usage: run_measure.sh [--skip-build]
#   --skip-build      serve squallar-web as-is (default: wasm-pack build
#                     through wasm-threads.sh first)
#
# Environment knobs (all optional):
#   SQUALLAR_WEB_DIR  dir to serve       (default <repo>/squallar-web)
#   RIG_OUT_DIR       output dir         (default <rig>/out-measure)
#   RIG_CHROMEDRIVER  chromedriver       (default: chromedriver on PATH)
#   RIG_GECKODRIVER   geckodriver        (default: $(ensure-geckodriver.sh))
#   RIG_BROWSERS      "firefox chromium" (default; firefox governs, runs and
#                     reports first; never merged)
#   RIG_SCENES        "A B C D" (default), or a subset
#   RIG_SETTLE        seconds before the warm rAF sample (default 6)
#   RIG_MEASURE_WINDOW  seconds of scripted time after settle (default 46 --
#                     at least two full 20 s script loops, so the window is
#                     bracketed by whole loops)
#   RIG_FRAMES        rAF deltas per sample (default 240)
#   RIG_DISPLAY       X display (default: $DISPLAY, then :0)
#   RIG_PANEL         "on" seeds the diagnostics panel visible (default off);
#                     printed as the panel= denominator either way
#   RIG_CANVAS        WxH the canvas drawing buffer is corrected to before
#                     the window opens (default: unset, each browser keeps
#                     whatever its chrome leaves). Set it on any leg whose row
#                     will be read beside another browser's or beside a native
#                     row, and give the native leg the same WxH window. Only a
#                     row whose target was MET prints cross=yes.
#   RIG_TLS           1 = serve https with a self-signed pair and drive at
#                     RIG_HOST (the TLS/COI smoke; a phone-shaped posture on
#                     this box). Default 0 = plain 127.0.0.1
#   RIG_HOST          host/IP to serve and drive when RIG_TLS=1 (default:
#                     this box's LAN IP as serve.py detects it)
#   RIG_DRIVE_EXTRA   extra args appended to every drive.py call
#
# --expect-interaction-frames rides on every leg: it is a COUNT assert (the
# scraped interact family strictly grew) and is this script's row-validity
# check, not a CI gate -- a leg whose player never injected produces no
# window and must say so rather than print a hollow row.

set -u -o pipefail   # not -e: attempt every leg and still summarise

RIG_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$RIG_DIR/../.." && pwd)"
WEB_DIR="${SQUALLAR_WEB_DIR:-$REPO_ROOT/squallar-web}"
OUT_DIR="${RIG_OUT_DIR:-$RIG_DIR/out-measure}"
CHROMEDRIVER="${RIG_CHROMEDRIVER:-$(command -v chromedriver || echo /usr/bin/chromedriver)}"
BROWSERS="${RIG_BROWSERS:-firefox chromium}"
SCENES="${RIG_SCENES:-A B C D}"
SETTLE="${RIG_SETTLE:-6}"
MEASURE_WINDOW="${RIG_MEASURE_WINDOW:-46}"
FRAMES="${RIG_FRAMES:-240}"
PANEL="${RIG_PANEL:-off}"
CANVAS="${RIG_CANVAS:-}"
TLS="${RIG_TLS:-0}"
PY=python3

COMMIT="$(git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)"

# ------------------------------------------------------------- scenes ----
# All 17 layer ids: LAYER_ID_LEDGER minus the retired FakeSource. Spelled
# out rather than derived so a ledger edit shows up as a diff here too; the
# count is what the campaign names everywhere a scene-A row is quoted.
ALL_LAYERS='\"ModelData\":true,\"SpcOutlook\":true,\"Radar\":true,\"SpcDiscussions\":true,\"NwsAlerts\":true,\"StormReports\":true,\"Lightning\":true,\"Metar\":true,\"CityLabels\":true,\"RadarSites\":true,\"UserLocation\":true,\"ColorScale\":true,\"SpcFireOutlook\":true,\"Mrms\":true,\"Gmgsi\":true,\"Terrain\":true,\"BasemapTiles\":true'

PANEL_SEED=""
if [ "$PANEL" = on ]; then
  PANEL_SEED='\"diagnostics_panel\":true,'
fi

# scene_seed <scene>: the localStorage seed JSON; scene_script <scene>: the
# gesture script the seed arms (also the row's script= denominator).
scene_seed() {
  case "$1" in
    A) echo '{"squallar.ui": "{'"$PANEL_SEED"'\"pane_count\":1,\"panes\":[{\"site\":\"KTLX\",\"enabled_overlays\":{'"$ALL_LAYERS"'}}]}", "squallar.frame_telemetry": "1", "squallar.raster_telemetry": "1", "squallar.gesture_script": "pan-zoom-2d"}' ;;
    B) echo '{"squallar.ui": "{'"$PANEL_SEED"'\"pane_count\":1,\"panes\":[{\"site\":\"KTLX\",\"render\":\"Volume\"}]}", "squallar.frame_telemetry": "1", "squallar.raster_telemetry": "1", "squallar.gesture_script": "orbit-3d"}' ;;
    C) echo '{"squallar.ui": "{'"$PANEL_SEED"'\"pane_count\":6,\"panes\":[{\"site\":\"KTLX\",\"render\":\"Volume\"},{\"site\":\"KTLX\"},{\"site\":\"KINX\",\"render\":\"Volume\"},{\"site\":\"KINX\"},{\"site\":\"KVNX\",\"render\":\"Volume\"},{\"site\":\"KVNX\"}]}", "squallar.frame_telemetry": "1", "squallar.raster_telemetry": "1", "squallar.gesture_script": "pan-zoom-2d"}' ;;
    D) echo '{"squallar.ui": "{'"$PANEL_SEED"'\"pane_count\":1,\"panes\":[{\"site\":\"KTLX\",\"enabled_overlays\":{'"$ALL_LAYERS"'}}]}", "squallar.frame_telemetry": "1", "squallar.raster_telemetry": "1", "squallar.gesture_script": "ui-sweep"}' ;;
    *) return 1 ;;
  esac
}
scene_script() {
  case "$1" in
    A|C) echo pan-zoom-2d ;;
    B)   echo orbit-3d ;;
    D)   echo ui-sweep ;;
  esac
}

# ---------------------------------------- scene-B denominator columns ----
# Scraped from the sources at run time, LOUDLY unknown when a pattern no
# longer matches: a silent default here is how a post-terrain-wiring row
# would masquerade as comparable to a pre-wiring one. The tripwire day
# (pane_ground_heights returns Some) these flip and every scene-B row says
# so by itself.
scrape() {  # scrape <file> <grep-pattern> <sed-extract>
  local hit
  hit="$(grep -oE "$2" "$REPO_ROOT/$3" 2>/dev/null | head -1)"
  if [ -z "$hit" ]; then
    echo "UNKNOWN(re-derive: pattern '$2' gone from $3)"
  else
    echo "$hit"
  fi
}

# `heights`/`buildings`: whether the provider fns are still the unconditional
# None stubs (the shipped state). Checked as "the fn body's first
# non-comment statement is `None`" over the few lines after the signature.
stub_state() {  # stub_state <fn-name>
  local body
  body="$(grep -A6 "^fn $1(" "$REPO_ROOT/squallar-egui/src/ui_map.rs" 2>/dev/null \
          | sed -n '2,7p' | grep -vE '^\s*//' | grep -m1 -E '^\s*(None|Some)')"
  case "$body" in
    *None*) echo "stub-none" ;;
    *Some*) echo "WIRED(re-spike scene B)" ;;
    *)      echo "UNKNOWN(re-derive: $1 moved in ui_map.rs)" ;;
  esac
}

HEIGHTS_STATE="$(stub_state pane_ground_heights)"
BUILDINGS_STATE="$(stub_state pane_building_prisms)"
GRID_ASSET_STATE="$(scrape - 'GRID_ASSET: Option<&\[u8\]> = (None|Some[^;]*)' squallar-geo/src/min_elevation.rs | sed 's/.*= //')"
SUN_DEFAULT="$(scrape - 'DEFAULT_SUN_LIGHTING: bool = (true|false)' squallar-egui/src/pane_content.rs | sed 's/.*= //')"
HEIGHT_ARCHIVE_GEN="$(scrape - 'terrain-rgb/[A-Za-z0-9_-]+' squallar-egui/src/tiles.rs | sed 's|terrain-rgb/||')"
if [ "$HEIGHTS_STATE" = stub-none ]; then
  GROUND_PASS="none(heights stub)"
else
  GROUND_PASS="UNKNOWN(heights no longer stubbed; count ground passes)"
fi
SCENE_B_COLS="ground_pass=$GROUND_PASS heights=$HEIGHTS_STATE sun_lighting_default=$SUN_DEFAULT grid_asset=$GRID_ASSET_STATE height_archive=$HEIGHT_ARCHIVE_GEN buildings=$BUILDINGS_STATE"

echo "commit=$COMMIT scenes=[$SCENES] browsers=[$BROWSERS] panel=$PANEL tls=$TLS canvas=${CANVAS:-unpinned}"
echo "scene B denominator columns: $SCENE_B_COLS"

# ------------------------------------------------------- display check ----
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
  echo "FATAL: no usable X display for the measurement arm: $DISPLAY_INFO" >&2
  echo "       (this arm needs a logged-in graphical session, like run_gpu_arm.sh)" >&2
  exit 1
fi
echo "measurement arm display: $DISPLAY_INFO"

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
  echo "building squallar-web (wasm-pack, CARGO_BUILD_JOBS=4)"
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

# start_server <seed-json>: serve with --coep (production posture) and the
# scene's seed; TLS + LAN host under RIG_TLS=1. Sets PORT and URL.
start_server() {
  local seed="$1"
  local ready_file="$OUT_DIR/serve.ready"
  local host_args=() scheme=http host=127.0.0.1
  if [ "$TLS" = 1 ]; then
    host="${RIG_HOST:-$("$PY" -c "
import importlib.util
spec = importlib.util.spec_from_file_location('serve', '$RIG_DIR/serve.py')
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
print(m.lan_ip() or '127.0.0.1')")}"
    host_args=(--host 0.0.0.0 --tls)
    scheme=https
  fi
  : > "$ready_file"
  "$PY" "$RIG_DIR/serve.py" --dir "$WEB_DIR" --port 0 \
      --log "$OUT_DIR/serve.log" \
      --seed-local-storage "$seed" --coep \
      ${host_args[@]+"${host_args[@]}"} \
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
  URL="$scheme://$host:$PORT/index-rig.html"
  echo "serving $WEB_DIR on port $PORT (pid $SERVER_PID) -> $URL"
  return 0
}

EXTRA=()
if [ -n "$CANVAS" ]; then
  EXTRA+=(--canvas "$CANVAS")
fi
if [ -n "${RIG_DRIVE_EXTRA:-}" ]; then
  # shellcheck disable=SC2206
  EXTRA+=($RIG_DRIVE_EXTRA)
fi

# run_leg <browser> <scene>
run_leg() {
  local browser="$1" scene="$2"
  local tag="$scene.$browser" driver seed
  case "$browser" in
    chromium) driver="$CHROMEDRIVER" ;;
    firefox)  driver="$GECKODRIVER" ;;
    *) echo "unknown browser: $browser" >&2; return 1 ;;
  esac
  seed="$(scene_seed "$scene")" || { echo "unknown scene: $scene" >&2; return 1; }
  start_server "$seed" || return 1
  # Hardware arm, headed, --require-hardware: a leg that fell back to a
  # software rasteriser must refuse to report rather than mislabel. The
  # in-app player interacts from the first frame, so the whole
  # settle+window stretch is scripted time and the marker lines bracket
  # whole loops inside it.
  "$PY" "$RIG_DIR/drive.py" \
      --browser "$browser" --url "$URL" \
      --out-dir "$OUT_DIR" --tag "$tag" \
      --driver "$driver" --frames "$FRAMES" \
      --settle "$SETTLE" --data-window "$MEASURE_WINDOW" \
      --arm hardware --require-hardware \
      --expect-interaction-frames \
      ${EXTRA[@]+"${EXTRA[@]}"}
  local rc=$?
  stop_server
  return "$rc"
}

overall=0
TAGS=""
for browser in $BROWSERS; do
  for scene in $SCENES; do
    echo
    echo "================ scene $scene / $browser ================"
    TAGS="$TAGS $scene.$browser"
    run_leg "$browser" "$scene" || overall=1
  done
done

# -------------------------------------------------------------- summary ----
echo
echo "================ measure summary (NOT A GATE; no figure here gates) ================"
"$PY" - "$OUT_DIR" "$COMMIT" "$PANEL" "$SCENE_B_COLS" $TAGS <<'EOF'
import json, os, sys
out, commit, panel, scene_b_cols = sys.argv[1:5]

for tag in sys.argv[5:]:
    p = os.path.join(out, tag + ".json")
    scene = tag.split(".", 1)[0]
    if not os.path.isfile(p):
        print("ROW %-12s NO RESULT (%s missing)" % (tag, p))
        continue
    r = json.load(open(p))
    env = r.get("env") or {}
    ad = r.get("adapter") or {}
    v = r.get("verdict") or {}
    b = r.get("canvas_final") or {}
    app = r.get("app_backend") or {}
    line = app.get("backend") or ""
    head = "wgpu selected the "
    i = line.find(head)
    backend = "UNKNOWN"
    if i >= 0:
        rest = line[i + len(head):]
        j = rest.find(" backend")
        backend = rest[:j] if j > 0 else rest
    coi = env.get("cross_origin_isolated")
    raf = r.get("raf_warm") or {}
    hz = ("%.0f" % (1000.0 / raf["p50"])) if raf.get("ok") and raf.get("p50") else "?"
    gw = r.get("gesture_window") or {}
    ifr = r.get("interaction_frames") or {}

    # Row validity: every reason is printed, none is silent. COI is the
    # loud one -- a non-isolated leg measured the one-thread fallback.
    invalid = []
    if coi is not True:
        invalid.append("coi=%s: NOT cross-origin isolated; the worker pool "
                       "is the one-thread fallback and this row describes a "
                       "configuration the app never ships in" % coi)
    if v.get("hardware_ok") is False:
        invalid.append("adapter is %s, not a GPU" % ad.get("renderer"))
    if not ifr.get("ok"):
        invalid.append("interact count never grew (player not armed, or "
                       "telemetry not loud)")
    if not gw:
        invalid.append("no gesture window (no marker lines scraped)")

    # The whole-picture overlay raster size. Cumulative from boot, and said so
    # -- a windowed figure would need the raster line kept per reading the way
    # the frame lines are. It is a DENOMINATOR: a before/after pair that does
    # not match here is comparing two surfaces, not two builds. See the
    # MB/picture note in this script's header.
    ort = r.get("overlay_raster_totals") or {}
    pics = ort.get("pictures") or 0
    mbpp = ("%.2f" % (ort.get("picture_bytes", 0) / pics / 1e6)) if pics else "-"

    # Viewport PIXELS, not a resolution string: pixels are what the picture is
    # sized from, and two rows at 1280x779 and 1248x714 look alike written out
    # and are 12% apart counted.
    bw, bh = b.get("bufferWidth") or 0, b.get("bufferHeight") or 0
    ct = r.get("canvas_target") or {}
    cross = "yes" if ct.get("met") else "no"

    # Did the basemap draw at all? Two independent readings, and the decode
    # one leads because it is the direct answer: a leg on the wrong side of
    # the pmtiles 32-bit truncation (`a7465238`) decodes zero vector tiles.
    # The ground counters are the second half -- a leg can decode tiles and
    # still place nothing -- and `placed` alone is not the test there, because
    # the fills went to the GPU and a drawing pane reads 0 in that one.
    bt = r.get("basemap_tile_totals")
    g = r.get("ground_tile_totals")
    decoded = None if not bt else (bt.get("vector_tiles", 0)
                                   + bt.get("raster_tiles", 0))
    placed = None if not g else (g.get("stroke_points", 0) or g.get("labels", 0)
                                 or g.get("draws", 0) or g.get("uploads", 0))
    if decoded is None and placed is None:
        basemap = "unknown(no basemap or ground line)"
    else:
        basemap = "%s-decoded/%s-placed" % (
            "some" if decoded else ("none" if decoded == 0 else "?"),
            "some" if placed else ("none" if placed == 0 else "?"))

    # The denominator row. One leg, one line, never merged with any other.
    print("ROW scene=%s browser=%s arm=%s adapter=%s:%s backend=%s "
          "viewport=%sx%s px=%s dpr=%s cross=%s hz~%s coi=%s panel=%s "
          "script=%s basemap=%s pictures=%s MB/picture=%s commit=%s%s"
          % (scene, r.get("browser"), r.get("arm"), ad.get("class"),
             ad.get("renderer"), backend,
             bw, bh, bw * bh, env.get("dpr"), cross,
             hz, coi, panel, gw.get("script") or "-", basemap, pics, mbpp,
             commit, "" if not invalid else "  ** INVALID **"))
    if ct and not ct.get("met"):
        print("ROW   canvas target %s asked, %s got: this row is NOT "
              "cross-comparable" % (ct.get("asked"), ct.get("got")))
    if scene == "B":
        print("ROW   %s" % scene_b_cols)
    # `idle` here is the window's input-free frames -- the scripted quiet
    # phases, where the post-gesture settle re-raster lands (WO-8). Its max
    # is the settle burst's worst frame, printed so the cost moved out of the
    # interact window stays a figure instead of vanishing between families.
    for family in ("interact", "idle", "cadence"):
        d = gw.get(family) or {}
        if d and not d.get("error"):
            note = " [settle burst]" if family == "idle" else ""
            print("ROW   window %-8s n=%-6s p50=%s us p90=%s us p99=%s us "
                  "max=%s us [%s loops, %s]%s"
                  % (family, d.get("n"), d.get("p50_us"), d.get("p90_us"),
                     d.get("p99_us"), d.get("max_us"),
                     gw.get("loops_completed"), gw.get("basis"), note))
    fl = r.get("frame_lines") or {}
    idle = fl.get("idle") or {}
    if idle:
        print("ROW   cumulative idle n=%-6s p50=%s us p99=%s us "
              "[from boot; the instrument's own floor, never a window figure]"
              % (idle.get("n"), idle.get("p50"), idle.get("p99")))
    if fl.get("gpu_unavailable"):
        print("ROW   gpu passes: unavailable (adapter lacks TIMESTAMP_QUERY)")
    for why in invalid:
        print("ROW   INVALID: %s" % why)
EOF

echo
echo "artifacts in $OUT_DIR:"
ls -l "$OUT_DIR" | sed 's/^/  /'
exit "$overall"
