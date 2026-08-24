#!/usr/bin/env bash
# Marketing screenshots, from the Tier-2 rig rather than beside it.
#
# This is a scene list and a loop. Every hard part -- launching a driver,
# waiting for a booted canvas rather than a painted one, seeding app state
# before the first script runs -- is already solved in serve.py and drive.py,
# and reimplementing any of it would produce shots that disagree with the gate.
#
# WHAT MAKES THE SCENES WORK. serve.py exposes a synthetic /index-rig.html that
# injects `--seed-local-storage` as the first script in <head>, before any app
# script runs (serve.py:105-112, :208-220). UiConfig is `#[serde(default)]`
# throughout, so a partial subset parses -- which means a scene is just the
# handful of fields it cares about. Omitting `config_version` is deliberate: the
# seed then reads as v1 and walks the whole migration ladder, exactly as the
# Tier-2 seed does.
#
# THE SHOTS ARE OF LIVE WEATHER. There is no way to pin a historical scan
# through UiConfig today, so what lands on the map is whatever is happening when
# you run this. Shoot during real convection or you will get a very calm
# marketing site. `RIG_SITES` overrides the site list if the interesting storm
# is somewhere other than the defaults.
#
# Usage:
#   ./shoot_marketing.sh                    # every scene, into www/screenshots
#   ./shoot_marketing.sh hero cross-section # just these
#   RIG_SCALE=2 ./shoot_marketing.sh        # 2x -- SEE THE CAVEAT BELOW
#   RIG_BROWSER=firefox ./shoot_marketing.sh
set -euo pipefail

RIG_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$RIG_DIR/../.." && pwd)"
WEB_DIR="${SQUALLAR_WEB_DIR:-$REPO_ROOT/squallar-web}"
OUT_DIR="${RIG_OUT_DIR:-$RIG_DIR/out/marketing}"
DEST="${MARKETING_DEST:-$REPO_ROOT/www/screenshots}"
PY=python3

BROWSER="${RIG_BROWSER:-chromium}"
WINDOW="${RIG_WINDOW:-1280x800}"
SETTLE="${RIG_SETTLE:-10}"
DATA_WINDOW="${RIG_DATA_WINDOW:-25}"
SCALE="${RIG_SCALE:-1}"

# Longer than the gate's defaults on purpose. Tier-2 wants the earliest instant
# it can prove the canvas is not blank; a screenshot wants the tilt fully
# painted and the overlays resolved, which is a different question and a slower
# one.

# ---------------------------------------------------------------- scenes ----
# name|description|seed JSON
#
# Keep the description honest -- it becomes the alt text, and a screenshot
# whose caption overstates what is on screen is the easiest kind of marketing
# lie to tell by accident.
scenes() {
  cat <<'SCENES'
hero|Base reflectivity with the layer panel, dBZ ramp and timeline transport|{"squallar.ui":"{\"pane_count\":1,\"panes\":[{\"site\":\"KTLX\",\"enabled_overlays\":{\"RadarSites\":true,\"Alerts\":true}}]}"}
velocity|Storm-relative velocity on a single pane|{"squallar.ui":"{\"pane_count\":1,\"panes\":[{\"site\":\"KTLX\",\"selected_product\":\"SRV\",\"enabled_overlays\":{\"RadarSites\":true}}]}"}
four-pane|Four linked panes comparing products across one volume|{"squallar.ui":"{\"pane_count\":4,\"panes\":[{\"site\":\"KTLX\"},{\"site\":\"KTLX\",\"selected_product\":\"SRV\"},{\"site\":\"KTLX\",\"selected_product\":\"CC\"},{\"site\":\"KTLX\",\"selected_product\":\"ZDR\"}]}"}
alerts|NWS warning polygons over the national picture|{"squallar.ui":"{\"pane_count\":1,\"panes\":[{\"site\":\"KTLX\",\"zoom\":5.0,\"enabled_overlays\":{\"Alerts\":true,\"Mrms\":true}}]}"}
satellite|The GMGSI global geostationary mosaic|{"squallar.ui":"{\"pane_count\":1,\"panes\":[{\"site\":\"KTLX\",\"zoom\":4.0,\"enabled_overlays\":{\"Gmgsi\":true}}]}"}
SCENES
}

# ------------------------------------------------------------------ deps ----
command -v "$PY" >/dev/null || { echo "FATAL: python3 not found" >&2; exit 1; }
if [ ! -f "$WEB_DIR/pkg/squallar_web_bg.wasm" ]; then
  echo "FATAL: $WEB_DIR/pkg/squallar_web_bg.wasm missing." >&2
  echo "Build first:  .github/scripts/wasm-threads.sh wasm-pack build squallar-web \\" >&2
  echo "                  --target web --release --no-typescript --no-pack" >&2
  exit 1
fi

mkdir -p "$OUT_DIR" "$DEST"

# 2x. `--force-device-scale-factor=1` is hardcoded at drive.py:425 and base args
# are emitted before --chromium-arg (drive.py:629), so last-flag-wins SHOULD
# carry it. That is inference about Chrome's flag handling, not a measurement --
# check the pixel dimensions of what lands before trusting a 2x run.
SCALE_ARGS=()
if [ "$SCALE" != "1" ]; then
  case "$BROWSER" in
    chromium) SCALE_ARGS=(--chromium-arg="--force-device-scale-factor=$SCALE") ;;
    firefox)  SCALE_ARGS=(--ff-pref "layout.css.devPixelsPerPx=$SCALE") ;;
  esac
  echo "NOTE: 2x is unverified here. Confirm the output is $((${WINDOW%x*} * SCALE))px wide."
fi

SERVER_PID=""
stop_server() {
  [ -n "$SERVER_PID" ] || return 0
  kill "$SERVER_PID" 2>/dev/null || true
  wait "$SERVER_PID" 2>/dev/null || true
  SERVER_PID=""
}
trap stop_server EXIT

# start_server <seed-json>: sets PORT and URL. Mirrors run_tier2.sh's handshake
# -- a fresh kernel-chosen port per scene, so no scene inherits another's
# service-worker scope or cache identity.
start_server() {
  local seed="$1"
  local ready="$OUT_DIR/serve.ready"
  : > "$ready"
  "$PY" "$RIG_DIR/serve.py" --dir "$WEB_DIR" --port 0 \
      --log "$OUT_DIR/serve.log" \
      --seed-local-storage "$seed" \
      > "$ready" 2>> "$OUT_DIR/serve.stderr" &
  SERVER_PID=$!
  PORT=""
  local _tag _base
  for _ in $(seq 1 100); do
    kill -0 "$SERVER_PID" 2>/dev/null || {
      echo "FATAL: serve.py exited early:" >&2; cat "$OUT_DIR/serve.stderr" >&2; return 1; }
    if [ -s "$ready" ]; then read -r _tag PORT _base < "$ready"; break; fi
    sleep 0.1
  done
  [ -n "$PORT" ] || { echo "FATAL: serve.py never printed its ready line" >&2; return 1; }
  URL="http://127.0.0.1:$PORT/index-rig.html"
  return 0
}

want=("$@")
wanted() {
  [ ${#want[@]} -eq 0 ] && return 0
  local n
  for n in "${want[@]}"; do [ "$n" = "$1" ] && return 0; done
  return 1
}

shot_count=0
fail=0

while IFS='|' read -r name desc seed; do
  [ -n "$name" ] || continue
  wanted "$name" || continue

  echo "=== $name -- $desc"
  start_server "$seed" || { fail=1; continue; }

  if "$PY" "$RIG_DIR/drive.py" \
        --browser "$BROWSER" --url "$URL" \
        --out-dir "$OUT_DIR" --tag "$name" \
        --window "$WINDOW" --settle "$SETTLE" --data-window "$DATA_WINDOW" \
        ${SCALE_ARGS[@]+"${SCALE_ARGS[@]}"}
  then
    # The page shot, not the canvas shot: the chrome IS the product here. A
    # bare canvas hides the layer panel, the colour ramp and the transport,
    # which is most of what distinguishes this from a radar image.
    if [ -f "$OUT_DIR/$name.page.png" ]; then
      cp "$OUT_DIR/$name.page.png" "$DEST/$name.png"
      echo "    -> $DEST/$name.png  ($(identify -format '%wx%h' "$DEST/$name.png" 2>/dev/null || echo 'size unknown'))"
      shot_count=$((shot_count + 1))
    else
      echo "    FAILED: drive.py returned 0 but wrote no page shot" >&2
      fail=1
    fi
  else
    echo "    FAILED: drive.py exited non-zero" >&2
    fail=1
  fi
  stop_server
done < <(scenes)

echo
echo "$shot_count screenshot(s) -> $DEST"
[ "$fail" -eq 0 ] || { echo "one or more scenes failed" >&2; exit 1; }
