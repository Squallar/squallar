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
#   C  6 panes (3 volume + 3 plan, THREE sites -- KTLX, KINX, KVNX), each
#      pane layer-unlinked, pan-zoom-2d -- the many-pane worst case.
#      `layer_link` is written explicitly and is load-bearing: it defaults to
#      TRUE, and a layer-linked group converges every pane onto the ACTIVE
#      pane's site on the first shell frame (`Gui::propagate_layer_state`,
#      which calls `p.set_site(active_site)`). Every scene-C row taken before
#      2026-08-31 therefore measured six panes on ONE site, KTLX, with KINX
#      and KVNX named in the seed and never displayed -- and this comment said
#      "two sites" while the seed named three, so all three statements
#      disagreed. Pinned now by `every_measure_scene_seeds_the_layout_it_claims`.
#   D  scene A's layer stack driven by ui-sweep -- the UI-responsiveness
#      scenario (toggles, panels, slider) through the click registry.
#   E1 scene A's layer stack with the pane's loop seeded PLAYING and no
#      gesture -- the loop's own per-frame cost, idle. Scenes A..D all run
#      with loops OFF, and looping is the heaviest texture consumer the app
#      has, so E is the coverage gap the campaign had.
#   E2 the same, with pan-zoom-2d armed -- a user panning while the data
#      animates, which is the realistic worst case.
#   E3 a volume pane looping, orbit-3d -- MAX_LOOP_VOLUME_BUILDS_PER_FRAME
#      is 1 and a resident grid set is the largest thing the app holds.
#
# Every E row needs denominators A..D do not: how many layers were really
# looping, frames RESIDENT against frames LISTED (a loop that lists fourteen
# and holds three animates three while every phase reads healthy), the pool
# against its floor/ceiling, and the playback interval. Those come off the
# app's own `loop state:` line, which `drive.py`'s FRAME_LINE_PROBE scrapes
# and the per-leg summary prints -- they are NOT derivable from the A..D
# denominator set, and an E row without them is not comparable to anything.
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
#   RIG_GECKODRIVER   geckodriver        (default: $(ensure-geckodriver.sh);
#                     only bootstrapped when firefox is actually requested)
#   RIG_SAFARIDRIVER  safaridriver       (default: safaridriver on PATH, then
#                     /usr/bin/safaridriver -- Apple ships it in the OS)
#   RIG_BROWSERS      "firefox chromium" (default; firefox governs, runs and
#                     reports first; never merged). `safari` is accepted and
#                     is macOS-only: it needs no X display and no downloaded
#                     driver, so both of those steps are scoped to the
#                     browsers actually asked for. A Safari row is a THIRD
#                     engine (WebKit) and is never merged with either of the
#                     other two. Its prerequisites are one-time and manual:
#                     `sudo safaridriver --enable` and Safari's Develop menu
#                     -> "Allow Remote Automation".
#   RIG_SCENES        "A B C D" (default), or a subset, or any of the loop
#                     scenes E1/E2/E3 (not in the default set: they are a
#                     separate lane and they cost a settle each)
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
#   RIG_ANDROID       1 = drive the phone's own browser over adb instead of a
#                     browser on this box. BOTH engines: RIG_BROWSERS=firefox
#                     goes through geckodriver + moz:firefoxOptions, chromium
#                     through chromedriver + goog:chromeOptions. Default 0.
#
#                     RIG_DRIVE_EXTRA="--android" ALONE IS NOT ENOUGH and this
#                     knob exists because of it: the display check below is a
#                     hard FATAL for any X11 browser, and an Android leg needs
#                     no X display at all -- so a phone leg on a headless box
#                     would die before drive.py was ever exec'd, and on a box
#                     with a display it would pass the check for the wrong
#                     reason. This is also where the row's Android
#                     denominators get printed.
#   RIG_ANDROID_PACKAGE  override the package (default: resolved per engine by
#                     drive.py -- com.android.chrome / org.mozilla.firefox).
#                     org.mozilla.firefox_beta and org.mozilla.fenix are
#                     SEPARATE INSTALLS with separate data, which is the way
#                     to keep automation off somebody's daily browser.
#   RIG_ADB_SERIAL    adb serial when more than one device is attached
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
SAFARIDRIVER="${RIG_SAFARIDRIVER:-$(command -v safaridriver || echo /usr/bin/safaridriver)}"
BROWSERS="${RIG_BROWSERS:-firefox chromium}"
SCENES="${RIG_SCENES:-A B C D}"
SETTLE="${RIG_SETTLE:-6}"
MEASURE_WINDOW="${RIG_MEASURE_WINDOW:-46}"
FRAMES="${RIG_FRAMES:-240}"
PANEL="${RIG_PANEL:-off}"
CANVAS="${RIG_CANVAS:-}"
TLS="${RIG_TLS:-0}"
ANDROID="${RIG_ANDROID:-0}"
ANDROID_PACKAGE="${RIG_ANDROID_PACKAGE:-}"
ADB_SERIAL="${RIG_ADB_SERIAL:-}"
# Scripted loops treated as settle for the SECOND, narrower window every
# gestured leg now also reports. `GesturePlayer::LOOP_SECONDS` is 20 s.
SKIP_LOOPS="${RIG_SKIP_LOOPS:-2}"
PY=python3

# RIG_COMMIT lets a leg run from an exported bundle that has no .git -- which
# is how the macOS rows are taken: the tree is copied to the Mac, not cloned.
# The commit is a DENOMINATOR on every row, so "unknown" is not an acceptable
# answer when the caller knows it.
COMMIT="${RIG_COMMIT:-$(git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)}"

# ------------------------------------------------------------- scenes ----
# All 17 layer ids: LAYER_ID_LEDGER minus the retired FakeSource. Spelled
# out rather than derived so a ledger edit shows up as a diff here too; the
# count is what the campaign names everywhere a scene-A row is quoted.
ALL_LAYERS='\"ModelData\":true,\"SpcOutlook\":true,\"Radar\":true,\"SpcDiscussions\":true,\"NwsAlerts\":true,\"StormReports\":true,\"Lightning\":true,\"Metar\":true,\"CityLabels\":true,\"RadarSites\":true,\"RadarCoverage\":true,\"UserLocation\":true,\"ColorScale\":true,\"SpcFireOutlook\":true,\"Mrms\":true,\"Gmgsi\":true,\"Terrain\":true,\"BasemapTiles\":true'

PANEL_SEED=""
if [ "$PANEL" = on ]; then
  PANEL_SEED='\"diagnostics_panel\":true,'
fi

# Scene E's loop posture, spelled once. Both figures are the app's own
# defaults today (`UiConfig::default`) and are written into the seed anyway,
# because they are DENOMINATORS: an E row's cost is a function of how much
# weather the loop keeps ready and how fast it steps, and a row measured on
# whatever the defaults happened to be that week is not comparable to one
# measured after they move. `loop_speed_fps` also comes back on the row as
# `advance us` off the app's own `loop state:` line, so the seed and what the
# app really did are checkable against each other rather than assumed equal.
LOOP_SEED='\"loop_lookback_secs\":3600,\"loop_speed_fps\":10.0,'

# scene_seed <scene>: the localStorage seed JSON; scene_script <scene>: the
# gesture script the seed arms (also the row's script= denominator).
scene_seed() {
  case "$1" in
    A) echo '{"squallar.ui": "{'"$PANEL_SEED"'\"pane_count\":1,\"panes\":[{\"site\":\"KTLX\",\"enabled_overlays\":{'"$ALL_LAYERS"'}}]}", "squallar.frame_telemetry": "1", "squallar.raster_telemetry": "1", "squallar.gesture_script": "pan-zoom-2d"}' ;;
    B) echo '{"squallar.ui": "{'"$PANEL_SEED"'\"pane_count\":1,\"panes\":[{\"site\":\"KTLX\",\"render\":\"Volume\"}]}", "squallar.frame_telemetry": "1", "squallar.raster_telemetry": "1", "squallar.gesture_script": "orbit-3d"}' ;;
    C) echo '{"squallar.ui": "{'"$PANEL_SEED"'\"pane_count\":6,\"panes\":[{\"site\":\"KTLX\",\"layer_link\":false,\"render\":\"Volume\"},{\"site\":\"KTLX\",\"layer_link\":false},{\"site\":\"KINX\",\"layer_link\":false,\"render\":\"Volume\"},{\"site\":\"KINX\",\"layer_link\":false},{\"site\":\"KVNX\",\"layer_link\":false,\"render\":\"Volume\"},{\"site\":\"KVNX\",\"layer_link\":false}]}", "squallar.frame_telemetry": "1", "squallar.raster_telemetry": "1", "squallar.gesture_script": "pan-zoom-2d"}' ;;
    D) echo '{"squallar.ui": "{'"$PANEL_SEED"'\"pane_count\":1,\"panes\":[{\"site\":\"KTLX\",\"enabled_overlays\":{'"$ALL_LAYERS"'}}]}", "squallar.frame_telemetry": "1", "squallar.raster_telemetry": "1", "squallar.gesture_script": "ui-sweep"}' ;;
    E1) echo '{"squallar.ui": "{'"$PANEL_SEED$LOOP_SEED"'\"pane_count\":1,\"panes\":[{\"site\":\"KTLX\",\"loop_playback\":\"playing\",\"enabled_overlays\":{'"$ALL_LAYERS"'}}]}", "squallar.frame_telemetry": "1", "squallar.raster_telemetry": "1"}' ;;
    E2) echo '{"squallar.ui": "{'"$PANEL_SEED$LOOP_SEED"'\"pane_count\":1,\"panes\":[{\"site\":\"KTLX\",\"loop_playback\":\"playing\",\"enabled_overlays\":{'"$ALL_LAYERS"'}}]}", "squallar.frame_telemetry": "1", "squallar.raster_telemetry": "1", "squallar.gesture_script": "pan-zoom-2d"}' ;;
    E3) echo '{"squallar.ui": "{'"$PANEL_SEED$LOOP_SEED"'\"pane_count\":1,\"panes\":[{\"site\":\"KTLX\",\"render\":\"Volume\",\"loop_playback\":\"playing\"}]}", "squallar.frame_telemetry": "1", "squallar.raster_telemetry": "1", "squallar.gesture_script": "orbit-3d"}' ;;
    *) return 1 ;;
  esac
}
scene_script() {
  case "$1" in
    A|C|E2) echo pan-zoom-2d ;;
    B|E3)   echo orbit-3d ;;
    D)      echo ui-sweep ;;
    E1)     echo none ;;
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

# Which of the requested browsers need an X display and a downloaded driver.
# Safari needs neither: it is macOS-only, Quartz is the compositor, and Apple
# ships safaridriver in the OS. Both checks below are therefore scoped to the
# browsers that were actually asked for -- a safari-only run on a Mac must not
# die on a display it will never open, and must not reach for geckodriver.
# On macOS the same exemption extends to firefox and chromium: Quartz is the
# only compositor there too, so an X display is not merely unnecessary, it does
# not exist and the lookup below would abort a leg that can run perfectly well.
# geckodriver is still needed -- but it must be a macOS build, which
# ensure-geckodriver.sh does not fetch, so a Mac leg passes RIG_GECKODRIVER.
NEEDS_X11=0
NEEDS_GECKO=0
for _br in $BROWSERS; do
  case "$_br" in
    firefox)  [ "$(uname -s)" = Darwin ] || NEEDS_X11=1; NEEDS_GECKO=1 ;;
    chromium) [ "$(uname -s)" = Darwin ] || NEEDS_X11=1 ;;
  esac
done
# An Android leg renders on the phone's own compositor. There is no X display
# in the story at all, so the FATAL below would be refusing a leg it cannot
# describe -- and on a box that happens to have a display it would pass for a
# reason that has nothing to do with the leg. geckodriver is still needed: it
# runs on THIS box and drives the phone over adb.
if [ "$ANDROID" = 1 ]; then
  NEEDS_X11=0
fi

# ------------------------------------------------------- display check ----
if [ "$NEEDS_X11" = 1 ]; then
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
else
  DISPLAY_INFO='{"display": null, "why": "not needed: no X11 browser requested"}'
  echo "measurement arm display: skipped (browsers=[$BROWSERS] need no X display)"
fi

SKIP_BUILD=0
for arg in "$@"; do
  case "$arg" in
    --skip-build) SKIP_BUILD=1 ;;
    *) echo "unknown argument: $arg" >&2; exit 1 ;;
  esac
done

if [ -z "${RIG_GECKODRIVER:-}" ] && [ "$NEEDS_GECKO" = 1 ]; then
  RIG_GECKODRIVER="$(bash "$RIG_DIR/ensure-geckodriver.sh")" || {
    echo "FATAL: ensure-geckodriver.sh failed" >&2
    exit 1
  }
fi
GECKODRIVER="${RIG_GECKODRIVER:-}"

mkdir -p "$OUT_DIR"

# --------------------------------------------------- binding the figures ----
#
# The same seam, and the same repair, as `run_tier2.sh` -- see the long block
# beside its own `LEDGER=` for the incident. This summary had the identical
# shape: `os.path.isfile(out/<tag>.json)` and then print that file's numbers,
# with NOTHING tying the file to the run that just happened. A leg that died
# before writing left the previous run's JSON under the same name and its
# figures were reported as this run's.
#
# It matters MORE here than in the gate, not less. A stale verdict at least
# looks like a verdict and a reader may sanity-check it; a stale FIGURE is a
# plausible number in a comparison table, and the A/B lane that found this was
# doing exactly that -- twelve legs, twelve reads of one file, every one
# reporting `total_s=179.18`. Nothing about a row like that looks wrong. It was
# caught because twelve identical three-minute timings are impossible, which is
# not a mechanism.
#
# Same three mechanisms: a per-attempt run id the driver copies into the JSON
# and this summary re-checks, artefacts wiped before the leg writes them, and
# the driver's exit code recorded so "never ran" is not read as "ran".
MEASURE_LEDGER="$OUT_DIR/measure-legs.tsv"
: > "$MEASURE_LEDGER"

new_run_id() {
  local u=""
  if [ -r /proc/sys/kernel/random/uuid ]; then
    u="$(cat /proc/sys/kernel/random/uuid 2>/dev/null)"
  fi
  if [ -z "$u" ]; then
    u="$(date -u +%Y%m%dT%H%M%S%N)-$$-${RANDOM}${RANDOM}"
  fi
  printf 'measure-%s' "$u"
}

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
if [ "$ANDROID" = 1 ]; then
  EXTRA+=(--android)
  [ -n "$ANDROID_PACKAGE" ] && EXTRA+=(--android-package "$ANDROID_PACKAGE")
  [ -n "$ADB_SERIAL" ]      && EXTRA+=(--adb-serial "$ADB_SERIAL")
  # THE ROW'S DENOMINATOR, printed once beside the commit line rather than
  # inferred later. On a device this rig does not own the viewport is
  # REPORTED, never set: --canvas asks for a correction the phone will not
  # perform, so matching-not-marking cannot apply and every Android row is
  # cross=no against every desktop and native row. Two Android rows compare to
  # each other. Nothing else.
  if [ -n "$CANVAS" ]; then
    echo "NOTE: RIG_CANVAS=$CANVAS is IGNORED on an Android leg -- the device"
    echo "      owns the display. The achieved viewport is on each row."
  fi
  ANDROID_DEVICE="$( (adb ${ADB_SERIAL:+-s "$ADB_SERIAL"} shell getprop ro.product.model 2>/dev/null; \
                      adb ${ADB_SERIAL:+-s "$ADB_SERIAL"} shell getprop ro.build.version.release 2>/dev/null) \
                    | tr '\n' ' ' )"
  echo "android=1 device=[${ANDROID_DEVICE:-UNREADABLE}] package=${ANDROID_PACKAGE:-per-engine default} serial=${ADB_SERIAL:-the one attached}"
  echo "android rows are cross=no BY CONSTRUCTION: viewport reported, not set"
  # FOLD STATE, on a foldable, is a row separator and not a footnote: folded
  # and unfolded are different resolutions, aspects and DPRs, so they are
  # different denominators and their figures must never be merged. Printed
  # here because the viewport alone makes them merely LOOK different, and a
  # reader who does not know the device is a foldable will read two
  # populations as one noisy one. READ ONLY -- `cmd device_state` can also
  # FORCE a state, and forcing one on somebody's daily phone is a system
  # change this rig does not make.
  ANDROID_FOLD="$( adb ${ADB_SERIAL:+-s "$ADB_SERIAL"} shell cmd device_state state 2>/dev/null \
                   | grep -oE "name='[A-Z_]+'" | head -1 | cut -d"'" -f2 )"
  echo "android fold_state=${ANDROID_FOLD:-UNREADABLE(not a foldable, or cmd device_state unavailable)}"
  if [ -z "$ANDROID_FOLD" ]; then
    echo "      -- if this IS a foldable, the rows cannot say which half they"
    echo "         measured and must not be pooled with the other half."
  fi
fi
if [ -n "${RIG_DRIVE_EXTRA:-}" ]; then
  # shellcheck disable=SC2206
  EXTRA+=($RIG_DRIVE_EXTRA)
fi

# run_leg <browser> <scene>
#
# Sets LAST_RUN_ID / LAST_RC for the caller, which records one ledger row per
# leg; the summary refuses any artefact that does not carry the id back.
LAST_TAG=""
LAST_RUN_ID=""
LAST_RC=0
run_leg() {
  local browser="$1" scene="$2"
  # The tag carries the TARGET, not just the engine. An Android row and a
  # desktop row for the same scene and browser are different measurements of
  # different machines, and sharing a filename is how one silently overwrites
  # the other and gets quoted as it.
  local tag="$scene.$browser" driver seed
  [ "$ANDROID" = 1 ] && tag="$scene.$browser.android"
  # The caller records the ledger row and the summary reads the artefact by
  # that name, so the tag has to travel out of here. Spelling it twice is how
  # every Android leg reported NO RESULT while three of them had just passed.
  LAST_TAG="$tag"
  LAST_RUN_ID="$(new_run_id)"
  LAST_RC=0
  # Everything this tag can write goes first, so a missing file means this leg
  # wrote nothing rather than "an older run's file is still here".
  rm -f "$OUT_DIR/$tag.json" \
        "$OUT_DIR/$tag.page.png" "$OUT_DIR/$tag.canvas.png" \
        "$OUT_DIR/$tag.fail.png" "$OUT_DIR/$tag.driver.log" \
        "$OUT_DIR/$tag.xvfb.log"
  case "$browser" in
    chromium) driver="$CHROMEDRIVER" ;;
    firefox)  driver="$GECKODRIVER" ;;
    safari)   driver="$SAFARIDRIVER" ;;
    *) echo "unknown browser: $browser" >&2; return 1 ;;
  esac
  seed="$(scene_seed "$scene")" || { echo "unknown scene: $scene" >&2; return 1; }
  # Scene E1 arms no gesture on purpose -- its whole question is what a
  # playing loop costs a frame NOBODY is touching. It therefore has no
  # interact family to assert on and no marker pair to bracket, so it takes
  # the wall-clock quiet window instead and drops the count assert. Every
  # other leg keeps both: --expect-interaction-frames is the row-validity
  # check that a hollow row cannot pass.
  local arm_args=(--expect-interaction-frames)
  if [ "$(scene_script "$scene")" = none ]; then
    arm_args=(--quiet-window "$SETTLE")
  fi
  # The `begin` marker is logged at the player's construction -- boot -- so
  # the default window holds the boot burst. Ask for the settled window
  # beside it on every gestured leg; it is additive and the default is still
  # printed, so nothing measured before this moves.
  if [ "$(scene_script "$scene")" != none ]; then
    arm_args+=(--window-skip-loops "$SKIP_LOOPS")
  fi
  start_server "$seed" || return 1
  # Hardware arm, headed, --require-hardware: a leg that fell back to a
  # software rasteriser must refuse to report rather than mislabel. The
  # in-app player interacts from the first frame, so the whole
  # settle+window stretch is scripted time and the marker lines bracket
  # whole loops inside it.
  "$PY" "$RIG_DIR/drive.py" \
      --browser "$browser" --url "$URL" \
      --out-dir "$OUT_DIR" --tag "$tag" --run-id "$LAST_RUN_ID" \
      --driver "$driver" --frames "$FRAMES" \
      --settle "$SETTLE" --data-window "$MEASURE_WINDOW" \
      --arm hardware --require-hardware \
      --expect-seed-applied \
      "${arm_args[@]}" \
      ${EXTRA[@]+"${EXTRA[@]}"}
  local rc=$?
  LAST_RC="$rc"
  stop_server
  return "$rc"
}

overall=0
for browser in $BROWSERS; do
  for scene in $SCENES; do
    echo
    echo "================ scene $scene / $browser ================"
    run_leg "$browser" "$scene" || overall=1
    printf '%s\t%s\t%s\n' "$LAST_TAG" "$LAST_RUN_ID" "$LAST_RC" \
        >> "$MEASURE_LEDGER"
  done
done

# -------------------------------------------------------------- summary ----
echo
echo "================ measure summary (NOT A GATE; no figure here gates) ================"
"$PY" - "$OUT_DIR" "$COMMIT" "$PANEL" "$SCENE_B_COLS" "$MEASURE_LEDGER" <<'EOF'
import json, os, sys
out, commit, panel, scene_b_cols, ledger_path = sys.argv[1:6]

legs = []
with open(ledger_path) as f:
    for line in f:
        line = line.rstrip("\n")
        if not line:
            continue
        parts = line.split("\t")
        while len(parts) < 3:
            parts.append("")
        legs.append(dict(zip(("tag", "run_id", "rc"), parts)))

for leg in legs:
    tag, want = leg["tag"], leg["run_id"]
    p = os.path.join(out, tag + ".json")
    scene = tag.split(".", 1)[0]
    if not os.path.isfile(p):
        print("ROW %-12s NO RESULT (%s missing; driver rc=%s). This leg "
              "produced no figures -- it is not a row with bad numbers, it is "
              "the absence of a row." % (tag, p, leg["rc"]))
        continue
    try:
        r = json.load(open(p))
    except Exception as e:
        print("ROW %-12s NO RESULT (%s is unreadable: %s)" % (tag, p, e))
        continue
    got = r.get("run_id")
    if got != want:
        # THE STALE READ. Every number below would have been printed as this
        # run's; in a comparison table nothing about them looks wrong.
        print("ROW %-12s NO RESULT -- STALE ARTEFACT: %s carries run_id %r, "
              "this leg was launched as %r. It was left by an EARLIER run "
              "(started_utc=%s, total_s=%s) and its figures describe nothing "
              "that happened just now."
              % (tag, p, got, want, r.get("started_utc"), r.get("total_s")))
        continue
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
    # E1 is unarmed BY DESIGN (a loop playing, nobody touching it), so it
    # carries no interact assert and its window is the wall-clock quiet
    # bracket. Its OWN validity check is that the loop actually ran: a row
    # reporting zero animating layers measured a still picture, whatever its
    # frame figures say.
    if scene == "E1":
        ls = (r.get("frame_lines") or {}).get("loop_state") or {}
        if not ls:
            invalid.append("no `loop state` line scraped; the loop "
                           "denominators are missing and the row is not an "
                           "E row")
        elif not ls.get("layers"):
            invalid.append("0 layers animating: the seeded loop never armed, "
                           "so this measured a still picture")
    else:
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
    # ---- the Android denominators, on the row rather than in the JSON ----
    #
    # These were written into the artefact and never PRINTED, which is the
    # same failure the campaign keeps hitting: a denominator nobody reads is a
    # denominator nobody applies. `profile_state` is the one that matters
    # most -- every Blink Android figure this campaign holds was taken on a
    # browser chromedriver had just `pm clear`ed, and not one of those rows
    # says so. A cleared row and a preserved row are DIFFERENT MEASUREMENTS:
    # cleared is the more reproducible of the two (no state accumulates across
    # passes) and also the PESSIMISTIC one (a cold HTTP cache and a cold
    # service worker on every pass is not what a returning user pays), so it
    # reads worse than real second-visit use and must never be quoted beside a
    # preserved row as though they were one population.
    binf = r.get("binary") or {}
    if binf.get("android_package"):
        db = r.get("device_before") or {}
        da = r.get("device_after") or {}
        def _t(d):
            if d.get("error"):
                return "?"
            return "%s%%/%sC" % (d.get("battery_percent", "?"),
                                 d.get("battery_temp_c", "?"))
        print("ROW   android package=%s profile_state=%s driver=%s "
              "attached_to_running=%s battery/temp before=%s after=%s"
              % (binf.get("android_package"),
                 binf.get("profile_state", "UNRECORDED"),
                 binf.get("driver_version", "?"),
                 binf.get("android_use_running_app"),
                 _t(db), _t(da)))
        # The DIRECTION the denominator biases, printed rather than left for
        # the reader to work out. A caveat nobody can act on from the row is
        # the same as no caveat: these rows are pessimistic against real
        # second-visit use, which is the safe direction for a bar we are
        # trying to clear -- and saying so is what stops a later reader
        # quoting them as "what a user sees".
        ps = str(binf.get("profile_state") or "")
        if ps.startswith("cleared"):
            print("ROW   android profile_state=cleared BIASES PESSIMISTIC: "
                  "cold HTTP cache and cold service worker on EVERY pass, "
                  "which is not what a returning user pays. Uniform across "
                  "passes (no state accumulates, so variance is not "
                  "cache-warming), but never comparable to a preserved-"
                  "profile row")
        elif ps.startswith("preserved"):
            print("ROW   android profile_state=preserved: state CARRIES "
                  "ACROSS passes, so an early pass and a late one are not "
                  "the same measurement and pass-to-pass variance includes "
                  "cache warming. Never comparable to a cleared-profile row")
    # E3 is a volume pane too, so it carries the terrain-campaign columns for
    # the same reason scene B does: without them a post-wiring row silently
    # becomes incomparable to every row before it.
    if scene in ("B", "E3"):
        print("ROW   %s" % scene_b_cols)
        # The 3D floor path's own row, on the two scenes that draw a floor.
        # `paints` is per pane per frame the strip drew and `mirror renders`
        # per mirror pass encoded -- two denominators, never added, and
        # neither is a term of `pictures` or of any upload figure. The causes
        # overlap `paints` and each other: `key moves` is what the floor's
        # content asked for, so paints far above it is a floor repainting for
        # a reason that is not its content.
        fl = r.get("floor_strip_totals")
        if fl:
            print("ROW   floor %s paints, %s mirror renders; asked %s key "
                  "moves, %s on a stable key, %s incomplete"
                  % (fl.get("paints"), fl.get("mirror_renders"),
                     fl.get("key_moves"), fl.get("paints_on_stable_key"),
                     fl.get("incomplete_paints")))
        else:
            print("ROW   floor unknown(no floor strips line)")
    if scene.startswith("E"):
        # The E-only denominators. Without these an E row is not an E row:
        # `layers` is how many loops were really running, and `resident` vs
        # `listed` is the silent-under-fill check -- a loop that lists its
        # whole cap and holds three frames animates three while every phase
        # reads healthy. `resident`/`in flight`/`failed` are disjoint SUBSETS
        # of `listed` and are never added to it; `allowed`/`cap`/`held` bound
        # frames TEXTURED, a different denominator from the slots `listed`
        # counts; a `pool` below what the tier boots at is a back-off.
        ls = (r.get("frame_lines") or {}).get("loop_state") or {}
        if ls:
            mib = lambda b: ("%.1f" % (b / 1048576.0)) if b is not None else "?"
            print("ROW   loop %s panes, %s layers animating, %s frames "
                  "listed, %s resident (%s in flight, %s failed); allowed "
                  "plan=%s section=%s volume=%s overlay=%s, cap=%s held=%s; "
                  "share=%s MiB pool=%s MiB floor=%s MiB ceiling=%s MiB; "
                  "advance=%s us"
                  % (ls.get("panes"), ls.get("layers"), ls.get("listed"),
                     ls.get("resident"), ls.get("in_flight"), ls.get("failed"),
                     ls.get("allowed_plan"), ls.get("allowed_section"),
                     ls.get("allowed_volume"), ls.get("allowed_overlay"),
                     ls.get("cap"), ls.get("held"),
                     mib(ls.get("share_bytes")), mib(ls.get("pool_bytes")),
                     mib(ls.get("floor_bytes")), mib(ls.get("ceiling_bytes")),
                     ls.get("advance_us")))
        else:
            print("ROW   loop UNKNOWN: no `loop state` line scraped -- the E "
                  "denominators are missing and this row is not comparable")
        # MB/picture is NOT printed again here. It is on the ROW line above,
        # beside `viewport=`/`px=`/`cross=`, where `685a806e` put it -- and
        # that is the right place, because the picture size is a pure function
        # of the surface and the two belong on one line. A second copy down
        # here would be a figure with the same name and no surface beside it,
        # which is exactly the incomparable pair that commit exists to stop.
    # `idle` here is the window's input-free frames -- the scripted quiet
    # phases, where the post-gesture settle re-raster lands (WO-8). Its max
    # is the settle burst's worst frame, printed so the cost moved out of the
    # interact window stays a figure instead of vanishing between families.
    # Two windows over the same readings, NEVER merged and never averaged:
    # the default one starts at the `begin` marker, which the player logs at
    # construction -- boot -- so it contains the boot burst; the settled one
    # is whole scripted loops with boot excluded. A row quoting one must say
    # which.
    for label, w in (("", gw), ("settled ", gw.get("settled") or {})):
        if not w:
            continue
        for family in ("interact", "idle", "cadence"):
            d = w.get(family) or {}
            if d and not d.get("error"):
                note = " [settle burst]" if family == "idle" else ""
                print("ROW   %swindow %-8s n=%-6s p50=%s us p90=%s us "
                      "p99=%s us max=%s us [%s loops, %s]%s"
                      % (label, family, d.get("n"), d.get("p50_us"),
                         d.get("p90_us"), d.get("p99_us"), d.get("max_us"),
                         w.get("loops_completed"), w.get("basis"), note))
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
