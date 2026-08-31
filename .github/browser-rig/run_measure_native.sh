#!/usr/bin/env bash
#
# run_measure_native.sh -- the NATIVE half of the measurement protocol.
#
# `run_measure.sh` measures the web target through a browser driver. This is
# its counterpart: the same scenes, the same in-app gesture player, the same
# marker-bracketed bin-diffed windowing, and a ROW line carrying the same
# columns, so a native row and a web row sit in one table.
#
# THIS IS NOT A GATE. Nothing here fails CI and no ms figure it prints may
# gate anything. It answers one question: what does an interaction frame cost
# on this machine, per scene, counted only inside the gesture window.
#
# ---- Why this exists -------------------------------------------------------
#
# The protocol was specified in prose in `run_measure.sh`'s header and
# implemented four separate times in four private scratchpads, each by a
# different lane, each dying with its session. No two lanes' native rows were
# guaranteed comparable, every lane paid to rebuild it, and a device matrix
# run on hardware there is exactly one pass at would have produced
# incomparable rows. So it is in the tree.
#
# ---- The six steps ---------------------------------------------------------
#
#   1. Release binary, FRESH REDIRECTED `XDG_CONFIG_HOME` and `XDG_CACHE_HOME`
#      per leg. Never the user's real config: a measurement must not be able
#      to overwrite the config of the person running it, and a leg must start
#      from a known scene rather than from whatever the last session
#      persisted. The scene is `run_measure.sh`'s own `scene_seed`, evaluated
#      by bash and remapped from localStorage keys to `<key>.json` files --
#      read, never restated, so scene A cannot mean two different things on
#      the two targets.
#   2. `SQUALLAR_GESTURE_SCRIPT` in the environment. It outranks the stored
#      key, so the scene's script is armed even if the seed were stale.
#   3. **Window geometry pinned by PID-RESOLVED WINDOW ID and READ BACK**, then
#      checked against the picture-bytes formula. See below -- this is the one
#      that has already silently corrupted a campaign.
#   4. Bracket whole script loops with the player's own `loop complete`
#      markers and DIFF the embedded histograms across the bracket.
#      Cumulative-from-boot figures contaminated an entire early scoreboard.
#   5. Diff `overlay rasters:` across the SAME bracket for MB/picture.
#   6. Two runs; a third when they diverge by more than 15% -- adjudicated on
#      the UNBINNED interact frame count. `Hist` is four bins per octave, so
#      any ratio in 1.68-2.38x prints as exactly 2.00x and a percentile-based
#      rule is wrong in both directions. See `native_row.py`'s header.
#
# ---- The window-size trap --------------------------------------------------
#
# A prior lane's `wmctrl -r Squallar -e ...` matched by TITLE SUBSTRING and had
# silently been running some legs at 3440x1440 instead of 1920x1080 -- proven
# only afterwards, by exact factorization of the picture totals. So geometry
# here is resolved through `xdotool search --pid`, never by title; it is READ
# BACK rather than requested and trusted; and the achieved surface is then
# checked against
#
#     picture bytes = (W * 1.5) * ((H - 40) * 1.5) * 4
#
# A leg whose bytes disagree with its window is REFUSED, and the row names the
# size the bytes say it really ran at.
#
# ---- Load is measured DURING the leg, not gated at the start ---------------
#
# A start-of-leg load gate is insufficient and its error is ONE-SIDED: load
# depresses only the cheaper-frame arm, so it biases ratios rather than adding
# noise, and a compile that begins after the gate passes is invisible to it.
# Load is sampled every 5 s for the whole leg; the row carries
# `loadavg_start/end/max` and a `quiet=` stamp.
#
# ---- Counterbalancing ------------------------------------------------------
#
# With two arms, `--counterbalance` runs them ABBA rather than base-first-
# twice. Un-counterbalanced order plus box load is what explained a
# 22.6-vs-45.3 ms disagreement that cost days. Every row carries its
# `position=`.
#
# Usage:
#   run_measure_native.sh [options]
#
#   --arm LABEL=/path/to/binary   an arm to measure (repeatable; default: one
#                                 arm `main` at the built release binary)
#   --scenes "A B"                default A
#   --geom WxH                    window size to pin (default 1920x1080, the
#                                 app's own RENDER_WIDTH x RENDER_HEIGHT).
#                                 Give the WEB leg the same WxH via RIG_CANVAS
#                                 or the two rows are not comparable.
#   --counterbalance              two arms, ABBA (default: in-order)
#   --runs N                      repeats per arm per scene (default 2)
#   --skip-loops N                whole loops discarded first (default 2; the
#                                 player logs `begin` at construction, so the
#                                 first loops are the boot burst)
#   --window-loops N              whole loops measured (default 2)
#   --hold N                      seconds held open past the window so
#                                 liveness has a reading to compare (default 20)
#   --quiet-max F                 loadavg the leg must stay under (default:
#                                 a quarter of nproc)
#   --out-dir DIR                 default <rig>/out-native
#   --gates                       print the default gate set and exit
#   --selftest                    run native_row.py's own tests and exit
#
# Environment: RIG_DISPLAY, RIG_PANEL (on|off), SQUALLAR_NATIVE_BIN.

set -u -o pipefail   # not -e: attempt every leg and still summarise

RIG_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$RIG_DIR/../.." && pwd)"
ROW_PY="$RIG_DIR/native_row.py"
PY=python3

OUT_DIR="$RIG_DIR/out-native"
SCENES="A"
GEOM="1920x1080"
RUNS=2
SKIP_LOOPS=2
WINDOW_LOOPS=2
HOLD=20
COUNTERBALANCE=0
PANEL="${RIG_PANEL:-off}"
DISPLAY_ARG="${RIG_DISPLAY:-${DISPLAY:-:0}}"
QUIET_MAX=""
declare -a ARMS=()

# `GesturePlayer::LOOP_SECONDS`. Only used to size the timeout; the bracket
# itself is marker-driven, never clock-driven.
LOOP_SECONDS=20

while [ $# -gt 0 ]; do
  case "$1" in
    --arm)            ARMS+=("$2"); shift 2 ;;
    --scenes)         SCENES="$2"; shift 2 ;;
    --geom)           GEOM="$2"; shift 2 ;;
    --runs)           RUNS="$2"; shift 2 ;;
    --skip-loops)     SKIP_LOOPS="$2"; shift 2 ;;
    --window-loops)   WINDOW_LOOPS="$2"; shift 2 ;;
    --hold)           HOLD="$2"; shift 2 ;;
    --quiet-max)      QUIET_MAX="$2"; shift 2 ;;
    --out-dir)        OUT_DIR="$2"; shift 2 ;;
    --counterbalance) COUNTERBALANCE=1; shift ;;
    --gates)          exec "$PY" "$ROW_PY" gates --repo-root "$REPO_ROOT" ;;
    --selftest)       exec "$PY" "$ROW_PY" selftest ;;
    -h|--help)        sed -n '2,90p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [ "${#ARMS[@]}" -eq 0 ]; then
  ARMS=("main=${SQUALLAR_NATIVE_BIN:-$REPO_ROOT/target/release/squallar}")
fi
if [ -z "$QUIET_MAX" ]; then
  # A quarter of the cores: enough headroom that a sibling lane's -j 4 compile
  # trips it, which is the case that actually biases a ratio here.
  QUIET_MAX="$(awk -v n="$(nproc)" 'BEGIN { printf "%.1f", (n / 4.0) }')"
fi

W="${GEOM%x*}"
H="${GEOM#*x}"
COMMIT="$(git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)"
mkdir -p "$OUT_DIR"

# The display's refresh, for the row's `hz~` column. Read from the server
# rather than inferred from the cadence histogram: a binned p50 would print a
# refresh that is a bin edge.
REFRESH="$(DISPLAY="$DISPLAY_ARG" xrandr --query 2>/dev/null \
           | awk '/\*/ { for (i = 1; i <= NF; i++) if ($i ~ /\*/) { gsub(/[*+]/, "", $i); print $i; exit } }')"
REFRESH="${REFRESH:-?}"

# The adapter is NOT read here. `native_row.py` takes it off the app's own
# `wgpu selected the ...` line, because the X server's answer and wgpu's
# disagree and only one of them ran the leg: on an Xvfb-hosted leg `glxinfo`
# reports `llvmpipe` while wgpu had selected a discrete NVIDIA adapter over
# Vulkan, which enumerates without an X server at all. This value is only the
# fallback for a log that never named one.
ADAPTER="unknown(app never logged an adapter)"

echo "commit=$COMMIT scenes=[$SCENES] geom=${W}x${H} runs=$RUNS arms=[${ARMS[*]}]"
echo "counterbalance=$COUNTERBALANCE quiet_max=$QUIET_MAX display=$DISPLAY_ARG"
echo "refresh=$REFRESH out=$OUT_DIR (adapter is read per leg from the app's own log)"

# ------------------------------------------------------------- preflight ----
for tool in xdotool xrandr; do
  command -v "$tool" >/dev/null || {
    echo "missing $tool; geometry cannot be pinned by window id and a leg "\
         "that cannot show the size it asked for is INVALID" >&2
    exit 2
  }
done

# A leg started while the box is already loaded is biased before it begins;
# the during-leg sampling below is what catches load that ARRIVES mid-leg.
START_LOAD="$(awk '{ print $1 }' /proc/loadavg)"
if awk -v l="$START_LOAD" -v m="$QUIET_MAX" 'BEGIN { exit !(l >= m) }'; then
  echo "REFUSING to start: loadavg is $START_LOAD, at or over quiet_max $QUIET_MAX." >&2
  echo "Wait for the box to go quiet; a loaded leg biases ratios one-sidedly." >&2
  exit 3
fi

# --------------------------------------------------------- window helpers ----

# resolve_window <pid>: the ONE viewable X window owned by that pid.
#
# By PID, never by title. `wmctrl -r Squallar` matches any window whose title
# CONTAINS the string, which is how a leg silently ran at another window's
# size. Refuses on zero or several rather than picking one.
resolve_window() {
  local pid="$1" w found=() wh
  for w in $(DISPLAY="$DISPLAY_ARG" xdotool search --pid "$pid" 2>/dev/null); do
    # Candidates are filtered by READABLE, PLAUSIBLE geometry rather than by
    # `xwininfo`'s map state: a toolkit may own several X windows per process
    # and the input-only or 1x1 helpers must not be mistaken for the surface.
    # Geometry is what this function exists to establish, so it is also the
    # honest filter, and it needs no tool beyond the one already required.
    wh="$(read_geometry "$w")"
    [ -n "$wh" ] || continue
    [ "${wh%x*}" -ge 100 ] 2>/dev/null && [ "${wh#*x}" -ge 100 ] 2>/dev/null \
      && found+=("$w")
  done
  if [ "${#found[@]}" -eq 1 ]; then
    echo "${found[0]}"
    return 0
  fi
  echo "resolve_window: pid $pid owns ${#found[@]} viewable windows, need exactly 1" >&2
  return 1
}

# read_geometry <window-id>: the achieved WxH, read back from the server.
read_geometry() {
  DISPLAY="$DISPLAY_ARG" xdotool getwindowgeometry --shell "$1" 2>/dev/null \
    | awk -F= '/^WIDTH=/ { w = $2 } /^HEIGHT=/ { h = $2 } END { if (w && h) print w "x" h }'
}

# ------------------------------------------------------------------- leg ----

# run_leg <label> <binary> <scene> <position> <run-index>
run_leg() {
  local label="$1" bin="$2" scene="$3" position="$4" run="$5"
  local tag="$scene.$label.r$run"
  local dir="$OUT_DIR/$tag"
  local log="$dir/app.log" loadf="$dir/load.tsv"
  local script seed pid wid achieved rc=0

  echo
  echo "================ scene $scene / $label / position $position ================"

  if [ ! -x "$bin" ]; then
    echo "ROW $tag NO RESULT (no executable at $bin)"
    return 1
  fi

  rm -rf "$dir"
  mkdir -p "$dir/config/squallar" "$dir/cache"

  # 1. The scene, out of run_measure.sh's own definitions.
  script="$("$PY" "$ROW_PY" scene --scene "$scene" --panel "$PANEL" --what script)" || return 1
  seed="$("$PY" "$ROW_PY" scene --scene "$scene" --panel "$PANEL" --what seed)" || return 1
  printf '%s' "$seed" | "$PY" "$ROW_PY" seed --config-dir "$dir/config/squallar" \
    | sed 's/^/  /' || return 1

  # 2. Launch. XDG redirected, telemetry seeded, gesture armed by env (which
  #    outranks the stored key). Stderr is the readout: env_logger writes the
  #    telemetry sentences there at info.
  local -a env_args=(
    "XDG_CONFIG_HOME=$dir/config"
    "XDG_CACHE_HOME=$dir/cache"
    "RUST_LOG=info"
    "DISPLAY=$DISPLAY_ARG"
  )
  if [ "$script" != none ]; then
    env_args+=("SQUALLAR_GESTURE_SCRIPT=$script")
  fi
  env "${env_args[@]}" "$bin" > "$log" 2>&1 &
  pid=$!

  # 3. Geometry, by PID-resolved window id, then READ BACK.
  local waited=0
  while [ "$waited" -lt 60 ]; do
    wid="$(resolve_window "$pid" 2>/dev/null)" && [ -n "$wid" ] && break
    kill -0 "$pid" 2>/dev/null || { echo "  app exited during boot"; break; }
    sleep 1; waited=$((waited + 1))
  done
  if [ -z "${wid:-}" ]; then
    echo "ROW $tag NO RESULT (no window resolved for pid $pid in ${waited}s)"
    kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null
    return 1
  fi
  DISPLAY="$DISPLAY_ARG" xdotool windowsize "$wid" "$W" "$H" 2>/dev/null
  sleep 2
  achieved="$(read_geometry "$wid")"
  echo "  window id=$wid asked=${W}x${H} achieved=${achieved:-unknown}"

  # 4. Sample load for the whole leg, every 5 s.
  ( while kill -0 "$pid" 2>/dev/null; do
      printf '%s\t%s\n' "$(date +%s)" "$(awk '{ print $1 }' /proc/loadavg)" >> "$loadf"
      sleep 5
    done ) &
  local sampler=$!

  # 5. Wait for the markers the bracket needs, then hold so liveness has a
  #    reading after the window. Marker-driven, not clock-driven: a slow boot
  #    lengthens the wait instead of truncating the window.
  local need=$((SKIP_LOOPS + WINDOW_LOOPS))
  local budget=$(( (need + 2) * LOOP_SECONDS + 90 ))
  local seen=0 elapsed=0
  if [ "$script" = none ]; then
    # A gestureless scene logs no markers by design; it takes a wall-clock
    # window and says so on the row.
    budget=$(( need * LOOP_SECONDS ))
    echo "  scene $scene arms no gesture: wall-clock window of ${budget}s"
    sleep "$budget"
  else
    while [ "$elapsed" -lt "$budget" ]; do
      # `grep -c` always prints a count and exits 1 on zero matches, so the
      # exit status is discarded rather than `||`-ed into a second line of
      # output -- which would make `seen` two words and break the compare.
      seen="$(grep -c "loop complete" "$log" 2>/dev/null)"
      seen="${seen:-0}"
      [ "$seen" -ge "$need" ] && break
      kill -0 "$pid" 2>/dev/null || break
      sleep 2; elapsed=$((elapsed + 2))
    done
    echo "  $seen/$need loop markers after ${elapsed}s"
  fi
  sleep "$HOLD"

  # 6. Stop sampling, stop the app, analyse.
  kill "$sampler" 2>/dev/null; wait "$sampler" 2>/dev/null
  kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null

  # An unreadable geometry is passed as ABSENT rather than as the asked-for
  # size: the surface check must refuse a leg it cannot confirm, not inherit
  # the number it was hoping for.
  local -a geom_args=()
  [ -n "${achieved:-}" ] && geom_args=(--achieved-geom "$achieved")

  "$PY" "$ROW_PY" analyze \
    --log "$log" --scene "$scene" --script "$script" --commit "$COMMIT" \
    --asked-geom "${W}x${H}" ${geom_args[@]+"${geom_args[@]}"} \
    --refresh "$REFRESH" --adapter "$ADAPTER" --panel "$PANEL" \
    --position "$position" --load-file "$loadf" --quiet-max "$QUIET_MAX" \
    --skip-loops "$SKIP_LOOPS" --window-loops "$WINDOW_LOOPS" \
    --json "$OUT_DIR/$tag.json"
  rc=$?
  return "$rc"
}

# ------------------------------------------------------------------ order ----

# The leg order for one scene: `label:binary:position` triples.
#
# ABBA rather than base-first-twice, because un-counterbalanced order plus box
# load explained a 22.6-vs-45.3 ms disagreement that cost the campaign days.
# Order is a confound; counterbalancing is what removes it, and the position
# rides on the row so a reader can check it was removed.
leg_order() {
  local -a order=()
  local i n="${#ARMS[@]}"
  if [ "$COUNTERBALANCE" = 1 ] && [ "$n" -eq 2 ] && [ "$RUNS" -eq 2 ]; then
    order=("${ARMS[0]}:1" "${ARMS[1]}:1" "${ARMS[1]}:2" "${ARMS[0]}:2")
  else
    local r
    for ((r = 1; r <= RUNS; r++)); do
      for ((i = 0; i < n; i++)); do
        order+=("${ARMS[$i]}:$r")
      done
    done
  fi
  printf '%s\n' "${order[@]}"
}

overall=0
TAGS=()
for scene in $SCENES; do
  pos=0
  while IFS= read -r entry; do
    pos=$((pos + 1))
    run="${entry##*:}"
    armspec="${entry%:*}"
    label="${armspec%%=*}"
    bin="${armspec#*=}"
    run_leg "$label" "$bin" "$scene" "p${pos}(${label})" "$run" || overall=1
    TAGS+=("$scene.$label.r$run")
  done < <(leg_order)
done

# ---------------------------------------------------------------- summary ----
echo
echo "================ native measure summary (NOT A GATE) ================"
echo "rows above are the readout; the pair verdict below is the protocol's"
echo "step 6, adjudicated on the UNBINNED interact frame count."
for scene in $SCENES; do
  # Adjudicate the first two rows of each arm within a scene.
  for arm in "${ARMS[@]}"; do
    label="${arm%%=*}"
    a="$OUT_DIR/$scene.$label.r1.json"
    b="$OUT_DIR/$scene.$label.r2.json"
    if [ -f "$a" ] && [ -f "$b" ]; then
      echo
      echo "-- scene $scene / $label --"
      "$PY" "$ROW_PY" diverge "$a" "$b"
    fi
  done
done

echo
echo "artifacts in $OUT_DIR:"
ls -l "$OUT_DIR" | sed 's/^/  /'
exit "$overall"
