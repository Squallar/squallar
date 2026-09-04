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
# implemented five separate times in five private scratchpads, each by a
# different lane, each dying with its session. No two lanes' native rows were
# guaranteed comparable, every lane paid to rebuild it, and a device matrix
# run on hardware there is exactly one pass at would have produced
# incomparable rows.
#
# **This file existing was not enough.** It shipped hard-gated on `xdotool`,
# `xrandr` and `/proc`, and `exit 2`-ed on anything else -- so a lane that
# needed a macOS row got no runner at all and wrote its own anyway. A refusal
# to run is not portability. Every platform-specific reading is now a named
# capability that can be ABSENT: the leg still runs, the row still prints, and
# it names what it had to do without. See `native_row.py`'s `platform_plan`.
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
#   3. **Window geometry pinned by PID-RESOLVED WINDOW and READ BACK FROM THE
#      APP**, then checked against the picture-bytes formula. See below -- this
#      is the one that has already silently corrupted a campaign.
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
# here is resolved through the PID (`xdotool search --pid`; System Events'
# `every process whose unix id is`), never by title, and refuses on anything
# other than exactly one window.
#
# The size is then READ BACK **FROM THE APP ITSELF** -- its own
# `Window resized to WxH` line, which is the surface it allocated, not the
# frame the window manager thinks it handed over. The picture bytes are read
# from the app too: its `overlay pictures:` line says what picture it
# allocated for EACH pane, in physical pixels, and the bracket's mean
# bytes/picture must sit within half a texel row plus half a column of those
# figures (a recorded six-pane bracket averaged 732 B under its pane -- a
# minority of pictures a row or a column short as the layout settled). The
# analyser used to MODEL that figure from the surface and a 40-point top bar
# -- exact at three surfaces on 2026-08-31, which were taken at display scale
# 1.0, where a point is a pixel. The bar lays out at 40 points then and now
# (40 is `MIN_BAR_HEIGHT`, a floor, and the bar sits on it). What the model
# omits is the scale factor: winit guessed 13/12 on a headed X11 leg of
# 2026-09-02, so the bar measured 43.33 px, and every multi-pane row read
# `** INVALID **` against a picture the app no longer drew. Legs now launch
# pinned to `WINIT_X11_SCALE_FACTOR=1` -- which on X11 overrides the guess
# outright (see the leg env block for the winit code) -- and, more to the
# point, EVERY ROW NOW RECORDS THE SCALE IT WAS MEASURED AT, off winit's own
# `Guessed window scale factor:` line, printed as `scale=` beside the geometry
# and carried in the JSON. That, not the pin, is what makes rows of different
# dates comparable: the pin only narrows the spread, and an old row or an
# unpinnable platform still gets read in the right unit. A log with no
# `overlay pictures:` line -- a binary older than it -- reads
# `** UNCHECKED: overlay pictures line absent **`, which is NOT `** INVALID **`:
# the bytes were checked against nothing and the row is not refused for it.
# The app's own surface line is
# the only geometry readback that exists on every platform, so it is
# also what makes an unpinnable platform (Wayland) produce a comparable row
# rather than no row. The window manager's own answer is kept as a second
# opinion; when the two disagree the row says so and the app's is used.
#
# Because the app's figure is the surface and the WM's is a frame, the target
# is reached ITERATIVELY: ask, read the app back, and spend the residual. The
# decoration and any scale factor are absorbed rather than assumed. A leg whose
# surface cannot be brought to the target is REFUSED -- it prints no ROW at
# all, because a plausible number in a comparison table is harder to catch than
# a missing one.
#
# ---- Load is measured DURING the leg, not gated at the start ---------------
#
# A start-of-leg load gate is insufficient and its error is ONE-SIDED: load
# depresses only the cheaper-frame arm, so it biases ratios rather than adding
# noise, and a compile that begins after the gate passes is invisible to it.
# Load is sampled every 5 s for the whole leg; the row carries
# `loadavg_start/end/max` and a `quiet=` stamp, and a leg loud only in its
# MIDDLE is named as such.
#
# ---- A quiet log is not a hang ---------------------------------------------
#
# A lane read a log that had stopped printing as a wedged app and only caught
# itself with a CPU-time check. When the markers do not arrive, this takes two
# `ps -o time=` readings ten seconds apart and says which it was, before
# anything concludes anything.
#
# ---- Counterbalancing ------------------------------------------------------
#
# With two arms, `--counterbalance` runs them ABBA rather than base-first-
# twice. Un-counterbalanced order plus box load is what explained a
# 22.6-vs-45.3 ms disagreement that cost days. Every row carries its
# `position=`. The order is `native_row.py`'s `leg_order`, whose property --
# equal mean position per arm -- is tested there rather than eyeballed here.
#
# Usage:
#   run_measure_native.sh [options]
#
#   --arm LABEL=/path/to/binary   an arm to measure (repeatable; default: one
#                                 arm `main` at the built release binary)
#   --arm-commit LABEL=SHA        the commit THAT arm's binary was built from
#                                 (repeatable). On a multi-arm run this is the
#                                 only way to record provenance correctly: see
#                                 `commit_for_arm`, which refuses to fall back
#                                 to the tree's HEAD when there is more than one
#                                 arm, because HEAD is then a tree neither
#                                 binary came from.
#   --scenes "A B"                default A
#   --geom WxH                    SURFACE to pin (default 1920x1080, the app's
#                                  own RENDER_WIDTH x RENDER_HEIGHT). Give the
#                                 WEB leg the same WxH via RIG_CANVAS or the
#                                 two rows are not comparable.
#   --commit SHA                  stamped on every row that has no
#                                 `--arm-commit` of its own. Defaults to the
#                                 tree's HEAD ON A SINGLE-ARM RUN ONLY; a
#                                 multi-arm run with no per-arm commits stamps
#                                 `unknown` rather than a tree neither binary
#                                 was built from. Pass it explicitly when the
#                                 binary was built elsewhere -- a shipped bundle
#                                 has no `.git` and every row it produced said
#                                 `commit=unknown`, on the row type where the
#                                 tree matters most.
#   --counterbalance              ABBA rather than in-order
#   --runs N                      repeats per arm per scene (default 2)
#   --skip-loops N                whole loops discarded first (default 2; the
#                                 player logs `begin` at construction, so the
#                                 first loops are the boot burst)
#   --window-loops N              whole loops measured (default 2)
#   --hold N                      seconds held open past the window so
#                                 liveness has a reading to compare (default 20)
#   --quiet-max F                 loadavg the leg must stay under (default:
#                                 a quarter of the online cores)
#   --allow-unpinned              produce a ROW even when the surface could not
#                                 be brought to `--geom`. The analyser still
#                                 marks it INVALID; this only stops the runner
#                                 refusing before it gets there.
#   --out-dir DIR                 default <rig>/out-native
#   --platform                    print the resolved platform plan and exit
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
ALLOW_UNPINNED=0
PANEL="${RIG_PANEL:-off}"
DISPLAY_ARG="${RIG_DISPLAY:-${DISPLAY:-:0}}"

# The scale pin, in ONE place, because the header used to RESTATE it: it
# printed `scale=1(WINIT_X11_SCALE_FACTOR)` unconditionally, and an unpinned
# control -- a copy with the env entry deleted -- printed the same header as
# the pinned arm. Every leg's env array is spliced from this one array and the
# header reports whether the array is empty, so the two cannot disagree. Set
# `RIG_SCALE_PIN=` (empty) for an unpinned control; there is then no copy of
# this script to keep in step.
#
# What the pin does, from winit 0.30.13's source rather than from belief
# (`src/platform_impl/linux/x11/util/randr.rs`, `get_output_info`): a parseable
# `WINIT_X11_SCALE_FACTOR` takes the `EnvVarDPI::Scale` arm and is RETURNED AS
# THE SCALE FACTOR, ahead of both XSETTINGS/`Xft.dpi` and the physical-size
# calculation. It is not clamped to a range -- only rejected, by panic, when it
# is not a normal positive float. So on X11 the pin decides, unconditionally.
# It decides nothing anywhere else: no other backend reads the variable.
#
# The pin is still not the record. The row records the scale the leg was
# MEASURED at, off winit's own `Guessed window scale factor:` line, whatever
# this array holds -- which is what makes a pinned row and an older unpinned
# one comparable at all.
declare -a SCALE_PIN_ENV=()
if [ -n "${RIG_SCALE_PIN-WINIT_X11_SCALE_FACTOR=1}" ]; then
  SCALE_PIN_ENV=("${RIG_SCALE_PIN-WINIT_X11_SCALE_FACTOR=1}")
fi

# A headed leg needs the display's MIT-MAGIC-COOKIE-1 key. `env` is called
# without `-i`, so a caller that already has XAUTHORITY passes it through
# anyway; naming it here is for the two cases that bite. A shell that never had
# one -- a detached session, a service, an agent -- opens no display at all:
# the app dies at boot with `Invalid MIT-MAGIC-COOKIE-1 key` and
# `XOpenDisplayFailed`, no surface line ever arrives, and the leg is REFUSED
# with no row -- an instrument failure that looks exactly like a broken build.
# And the runner's OWN X tools (xdotool, xrandr) need it for the same reason,
# which is why it is exported rather than only handed to the leg.
#
# The cookie file's name is generated per session (`xauth_XXXXXX`), so it is
# MATCHED, never hardcoded, and the newest match wins. Nothing here creates,
# copies or edits a cookie: the display is the user's, and a rig that writes to
# the thing it is measuring on has stopped measuring it.
if [ -z "${XAUTHORITY:-}" ]; then
  XAUTH_FOUND="$(ls -1t "/run/user/$(id -u)"/xauth_* 2>/dev/null | head -1)"
  if [ -n "$XAUTH_FOUND" ]; then
    export XAUTHORITY="$XAUTH_FOUND"
  fi
fi
QUIET_MAX=""
COMMIT=""
SHOW_PLATFORM=0
declare -a ARMS=()
declare -a ARM_COMMITS=()

# `GesturePlayer::LOOP_SECONDS`. Only used to size the timeout; the bracket
# itself is marker-driven, never clock-driven.
LOOP_SECONDS=20

# How many times the geometry solver may spend its residual before giving up.
# Two is enough for a decorated frame; more than that means the app is not
# converging on the target and the leg should be refused, not retried forever.
GEOM_ATTEMPTS=5

while [ $# -gt 0 ]; do
  case "$1" in
    --arm)            ARMS+=("$2"); shift 2 ;;
    --arm-commit)     ARM_COMMITS+=("$2"); shift 2 ;;
    --scenes)         SCENES="$2"; shift 2 ;;
    --geom)           GEOM="$2"; shift 2 ;;
    --commit)         COMMIT="$2"; shift 2 ;;
    --runs)           RUNS="$2"; shift 2 ;;
    --skip-loops)     SKIP_LOOPS="$2"; shift 2 ;;
    --window-loops)   WINDOW_LOOPS="$2"; shift 2 ;;
    --hold)           HOLD="$2"; shift 2 ;;
    --quiet-max)      QUIET_MAX="$2"; shift 2 ;;
    --out-dir)        OUT_DIR="$2"; shift 2 ;;
    --counterbalance) COUNTERBALANCE=1; shift ;;
    --allow-unpinned) ALLOW_UNPINNED=1; shift ;;
    --platform)       SHOW_PLATFORM=1; shift ;;
    --gates)          exec "$PY" "$ROW_PY" gates --repo-root "$REPO_ROOT" ;;
    --selftest)       exec "$PY" "$ROW_PY" selftest ;;
    -h|--help)        sed -n '2,141p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

# ---------------------------------------------------------------- platform ---
#
# Which readings this machine can supply, and a NAMED reason for each it
# cannot. The decision is `native_row.py`'s, not this file's: a rule spelled in
# bash can only be checked by running a leg, and running a leg needs a display,
# a GPU and three minutes.

# The tools the plan asks about. `command -v` for executables; `procfs` is a
# readable file rather than a program, so it is probed as one.
probe_tools() {
  local t found=()
  for t in xdotool xrandr ps sysctl osascript system_profiler; do
    command -v "$t" >/dev/null 2>&1 && found+=("$t")
  done
  [ -r /proc/loadavg ] && found+=("procfs")
  local IFS=,
  echo "${found[*]-}"
}

# `eval` rather than a parse: `cmd_plan` single-quotes every value and escapes
# embedded quotes, so a reason containing an apostrophe cannot break this.
#
# The substitution is captured and TESTED before it is evaluated. `eval ""`
# succeeds, so `eval "$(...)" || exit` would let a python that died print
# nothing and carry on with every PLAT_ variable unset -- and under `set -u`
# that is a failure a hundred lines later, blamed on the wrong thing.
PLAN_SH="$("$PY" "$ROW_PY" plan \
             --system "$(uname -s 2>/dev/null || echo unknown)" \
             --release "$(uname -r 2>/dev/null || echo '')" \
             --session "${XDG_SESSION_TYPE:-}" \
             --have "$(probe_tools)")"
case "$PLAN_SH" in
  PLAT_NAME=*) eval "$PLAN_SH" ;;
  *) echo "native_row.py produced no platform plan; it is not runnable here" >&2
     exit 2 ;;
esac

report_platform() {
  local cap up tool why
  echo "platform=$PLAT_NAME  degraded=[${PLAT_DEGRADED:-}]"
  for cap in loadavg window refresh cputime; do
    up="$(echo "$cap" | tr '[:lower:]' '[:upper:]')"
    eval "tool=\${PLAT_${up}:-}"
    eval "why=\${PLAT_WHY_${up}:-}"
    if [ -n "$tool" ]; then
      echo "  $cap: $tool"
    else
      echo "  $cap: UNAVAILABLE -- $why"
    fi
  done
}

if [ "$SHOW_PLATFORM" = 1 ]; then
  report_platform
  exit 0
fi

# ------------------------------------------------------- platform readings ---

# The 1-minute load average, or empty when this machine cannot report one.
plat_loadavg() {
  case "${PLAT_LOADAVG:-}" in
    procfs) awk '{ print $1 }' /proc/loadavg 2>/dev/null ;;
    sysctl) sysctl -n vm.loadavg 2>/dev/null | awk '{ print $2 }' ;;
    *)      echo "" ;;
  esac
}

# Online cores. `getconf` answers on both platforms; `nproc` is Linux-only and
# `sysctl hw.ncpu` is macOS-only, so neither is reached for.
plat_cores() {
  getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4
}

# Accumulated CPU time of a process, in `ps`'s own format. Both platforms'
# `ps` accept `-o time= -p <pid>`; `native_row.py` parses either spelling.
plat_cpu_time() {
  [ -n "${PLAT_CPUTIME:-}" ] || { echo ""; return; }
  ps -o time= -p "$1" 2>/dev/null | tr -d ' '
}

# The display's refresh, for the row's `hz~` column. Read from the system
# rather than inferred from the cadence histogram: a binned p50 would print a
# refresh that is a bin edge.
plat_refresh() {
  # An arm may DECLARE its refresh, and on a box with no xrandr (or no
  # system_profiler) that declaration is the only way to run at all: an empty
  # reading is now a hard INVALID, because a rig that cannot see the panel
  # cannot tell a live one from the dead one this box had on 2026-09-03.
  if [ -n "${RIG_PANEL_HZ:-}" ]; then echo "$RIG_PANEL_HZ"; return; fi
  case "${PLAT_REFRESH:-}" in
    xrandr)
      DISPLAY="$DISPLAY_ARG" xrandr --query 2>/dev/null \
        | awk '/\*/ { for (i = 1; i <= NF; i++) if ($i ~ /\*/) { gsub(/[*+]/, "", $i); print $i; exit } }'
      ;;
    system_profiler)
      # TWO spellings, because macOS changed one. Older releases print a
      # `Refresh Rate: 60 Hz` field; 26.4 prints only
      # `UI Looks like: 3440 x 1440 @ 175.00Hz`, and reading just the first
      # gives an EMPTY hz~ column on every current Mac -- which is how the
      # macOS lane came to hardcode its refresh into a sweep script.
      # First display wins, as it does under xrandr's `*`.
      system_profiler SPDisplaysDataType 2>/dev/null \
        | awk '
            /Refresh Rate/ {
              n = $0; sub(/.*Refresh Rate: */, "", n); gsub(/[^0-9.]/, "", n)
              if (n != "") { print n; exit }
            }
            /UI Looks like/ {
              if (match($0, /@ *[0-9.]+ *Hz/)) {
                s = substr($0, RSTART, RLENGTH); gsub(/[^0-9.]/, "", s)
                if (s != "") { print s; exit }
              }
            }'
      ;;
    # NO READER ON THIS ARM -- which is NOT the same thing as a display that
    # answered with nothing. `PLAT_REFRESH` is empty when `platform_plan`
    # found no `xrandr`/`system_profiler`, or did not recognise the platform
    # at all; the rig never asked, so nothing here is a finding about the
    # monitor. Emitted as `native_row.py`'s `PANEL_NO_READER` sentinel, which
    # refuses the leg with a reason naming the RIG. Until 2026-09-04 this
    # branch echoed nothing and the caller turned it into `?`, which is the
    # spelling for a DEAD DISPLAY: a box without xrandr reported a monitor
    # failure it had no way to have observed.
    *) echo "no-reader" ;;
  esac
}

# ------------------------------------------------------------ window layer ---
#
# Two implementations of one contract, and NEITHER matches by title. The
# title-substring `wmctrl -r` that silently ran legs at 3440x1440 is the reason
# this is resolved through the PID on both platforms and refuses on anything
# other than exactly one window.

# plat_window_resolve <pid>: an opaque handle, or empty with a reason on stderr.
plat_window_resolve() {
  local pid="$1"
  case "${PLAT_WINDOW:-}" in
    xdotool)
      local w wh found=()
      for w in $(DISPLAY="$DISPLAY_ARG" xdotool search --pid "$pid" 2>/dev/null); do
        # Candidates are filtered by READABLE, PLAUSIBLE geometry rather than
        # by `xwininfo`'s map state: a toolkit may own several X windows per
        # process and the input-only or 1x1 helpers must not be mistaken for
        # the surface.
        wh="$(DISPLAY="$DISPLAY_ARG" xdotool getwindowgeometry --shell "$w" 2>/dev/null \
              | awk -F= '/^WIDTH=/ { a = $2 } /^HEIGHT=/ { b = $2 } END { if (a && b) print a "x" b }')"
        [ -n "$wh" ] || continue
        [ "${wh%x*}" -ge 100 ] 2>/dev/null && [ "${wh#*x}" -ge 100 ] 2>/dev/null \
          && found+=("$w")
      done
      if [ "${#found[@]}" -eq 1 ]; then echo "${found[0]}"; return 0; fi
      echo "pid $pid owns ${#found[@]} viewable X windows, need exactly 1" >&2
      return 1
      ;;
    osascript)
      local r
      r="$(osascript <<EOF 2>&1
tell application "System Events"
  set ps to (every process whose unix id is $pid)
  if (count of ps) is 0 then return "NOPROC"
  set ws to (every window of item 1 of ps)
  if (count of ws) is 0 then return "NOWINDOW"
  if (count of ws) > 1 then return "MULTIWINDOW:" & (count of ws)
  return "OK"
end tell
EOF
)"
      if [ "$r" = OK ]; then echo "sysevents"; return 0; fi
      echo "System Events could not resolve exactly one window for pid $pid: $r" >&2
      echo 'a "not authorized" answer means Accessibility permission is not' >&2
      echo 'granted to the process running this script' >&2
      return 1
      ;;
    *)
      echo "${PLAT_WHY_WINDOW:-no window capability on this platform}" >&2
      return 1
      ;;
  esac
}

# plat_window_set <pid> <handle> <w> <h>: ask for a frame size. Prints what the
# WINDOW MANAGER says it achieved; the app's own reading is taken separately
# and is the one that decides.
plat_window_set() {
  local pid="$1" handle="$2" w="$3" h="$4"
  case "${PLAT_WINDOW:-}" in
    xdotool)
      DISPLAY="$DISPLAY_ARG" xdotool windowsize "$handle" "$w" "$h" 2>/dev/null
      sleep 1
      DISPLAY="$DISPLAY_ARG" xdotool getwindowgeometry --shell "$handle" 2>/dev/null \
        | awk -F= '/^WIDTH=/ { a = $2 } /^HEIGHT=/ { b = $2 } END { if (a && b) print a "x" b }'
      ;;
    osascript)
      osascript <<EOF 2>/dev/null
tell application "System Events"
  set ps to (every process whose unix id is $pid)
  if (count of ps) is 0 then return ""
  set ws to (every window of item 1 of ps)
  if (count of ws) is not 1 then return ""
  set win to item 1 of ws
  set position of win to {40, 40}
  set size of win to {$w, $h}
  delay 1
  set s to size of win
  return ((item 1 of s) as text) & "x" & ((item 2 of s) as text)
end tell
EOF
      ;;
    *) echo "" ;;
  esac
}

# app_surface <log>: the surface the APP says it allocated -- the reading the
# picture-bytes formula predicts, and the only one that exists everywhere.
# The app's own surface readback. TWO sentences: `Surface configured to` once
# at startup, `Window resized to` on every later resize. Reading only the
# second refused a leg whose window opened at exactly the requested size --
# no resize event, no line, nothing to confirm against -- and that is the
# CORRECT case, not a broken one. Newest of either wins.
app_surface() {
  grep -E "(Window resized to|Surface configured to) " "$1" 2>/dev/null \
    | tail -1 | sed -E 's/.*(Window resized to|Surface configured to) //'
}

# ------------------------------------------------------------------ setup ----

if [ "${#ARMS[@]}" -eq 0 ]; then
  ARMS=("main=${SQUALLAR_NATIVE_BIN:-$REPO_ROOT/target/release/squallar}")
fi
if [ -z "$QUIET_MAX" ]; then
  # A quarter of the cores: enough headroom that a sibling lane's -j 4 compile
  # trips it, which is the case that actually biases a ratio here.
  QUIET_MAX="$(awk -v n="$(plat_cores)" 'BEGIN { printf "%.1f", (n / 4.0) }')"
fi

W="${GEOM%x*}"
H="${GEOM#*x}"

# The commit rides on every row, and on a multi-arm run it is PER ARM.
#
# It used to be one value for the whole run, defaulting to the tree's HEAD. On a
# two-arm comparison that is not merely imprecise, it is FALSE: both rows get
# stamped with whatever main happens to be, which is a tree NEITHER binary was
# built from. Two lanes hit it independently on 2026-09-02 and both worked
# around it the same way -- by passing `--commit <a>+<b>`, a single field made
# to carry two values by string concatenation, which is the defect confessing.
# The row that results cannot say which arm was which tree, so its provenance
# is unreconstructable afterwards. Not a wrong number; an unrecordable one.
#
# The ROW schema did not have to change: `native_row.py` is invoked ONCE PER
# LEG and every row already carries exactly one arm. Only this script was
# putting the wrong value into it.
#
# Resolution order for an arm, first hit wins:
#   1. `--arm-commit LABEL=SHA` for that label
#   2. `--commit SHA`, the whole-run value, still accepted
#   3. the tree's HEAD -- but ONLY on a single-arm run, where the binary is
#      presumed to be this tree's build
#   4. `unknown`
# Step 3 is deliberately withheld from multi-arm runs. `unknown` is honest;
# HEAD would be a tree neither binary came from, and a plausible-looking wrong
# commit is worse than an admitted absent one.
# The decision itself lives in `native_row.py` as a pure function with tests,
# for the reason this runner's whole decision set does: a rule re-derived in
# shell by the next lane is a rule that will be re-derived DIFFERENTLY. See
# `squallar-app/tests/native_measure_logic.rs`, which is what makes those
# tests run in CI.
commit_for_arm() {
  "$PY" "$ROW_PY" commit-for-arm \
    --label "$1" --arms "${#ARMS[@]}" --commit "$COMMIT" \
    --head "$(git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || echo '')" \
    ${ARM_COMMITS[@]+"${ARM_COMMITS[@]/#/--arm-commit=}"}
}

mkdir -p "$OUT_DIR"

REFRESH="$(plat_refresh)"
REFRESH="${REFRESH:-?}"

# The adapter is NOT read here. `native_row.py` takes it off the app's own
# `wgpu selected the ...` line, because the X server's answer and wgpu's
# disagree and only one of them ran the leg: on an Xvfb-hosted leg `glxinfo`
# reports `llvmpipe` while wgpu had selected a discrete NVIDIA adapter over
# Vulkan, which enumerates without an X server at all. This value is only the
# fallback for a log that never named one.
ADAPTER="unknown(app never logged an adapter)"

echo "commit=$COMMIT scenes=[$SCENES] geom=${W}x${H} runs=$RUNS arms=[${ARMS[*]}]"
# Reported, not asserted: read off the array the legs are actually launched
# with, and the row's own measured `scale=` is the figure that settles it.
if [ "${#SCALE_PIN_ENV[@]}" -gt 0 ]; then
  SCALE_REPORT="pinned(${SCALE_PIN_ENV[*]})"
else
  SCALE_REPORT="unpinned(winit guesses; each row records what it guessed)"
fi
echo "counterbalance=$COUNTERBALANCE quiet_max=$QUIET_MAX display=$DISPLAY_ARG scale=$SCALE_REPORT xauthority=${XAUTHORITY:-<none found>}"
echo "refresh=$REFRESH out=$OUT_DIR (adapter is read per leg from the app's own log)"
report_platform

# ------------------------------------------------------------- preflight ----
#
# A leg started while the box is already loaded is biased before it begins; the
# during-leg sampling below is what catches load that ARRIVES mid-leg. A
# machine that cannot report load at all is a degradation with a name, not an
# exit -- the analyser will refuse to stamp such a leg quiet.
START_LOAD="$(plat_loadavg)"
if [ -n "$START_LOAD" ]; then
  if awk -v l="$START_LOAD" -v m="$QUIET_MAX" 'BEGIN { exit !(l >= m) }'; then
    echo "REFUSING to start: loadavg is $START_LOAD, at or over quiet_max $QUIET_MAX." >&2
    echo "Wait for the box to go quiet; a loaded leg biases ratios one-sidedly." >&2
    exit 3
  fi
else
  echo "NOTE: no load reading on this machine (${PLAT_WHY_LOADAVG:-}); every" \
       "row will be marked INVALID for want of a quiet stamp." >&2
fi

# ------------------------------------------------------------------- leg ----

# solve_geometry <pid> <handle> <log> -- bring the app's SURFACE to ${W}x${H}.
#
# Iteratively, because the number asked for is a frame and the number that
# matters is the surface inside it: the decoration and any scale factor are
# absorbed by the residual rather than assumed to be zero. Sets GEOM_MET,
# GEOM_ACHIEVED (the app's own reading) and GEOM_WHY.
GEOM_MET=no
GEOM_ACHIEVED=""
GEOM_WHY=""
GEOM_WM=""
solve_geometry() {
  local pid="$1" handle="$2" log="$3"
  local fw="$W" fh="$H" attempt inner iw ih
  GEOM_MET=no; GEOM_ACHIEVED=""; GEOM_WHY=""; GEOM_WM=""

  if [ -z "$handle" ]; then
    # No window capability. The leg is not abandoned: if the app already opened
    # at the target, the surface is right and the byte check will confirm it.
    inner="$(app_surface "$log")"
    GEOM_ACHIEVED="$inner"
    if [ "$inner" = "${W}x${H}" ]; then
      GEOM_MET=yes
      GEOM_WHY="unpinned but already correct: ${PLAT_WHY_WINDOW:-no window capability}"
    else
      GEOM_WHY="geometry cannot be pinned here (${PLAT_WHY_WINDOW:-no window capability}) and the app opened at ${inner:-unknown}, not ${W}x${H}"
    fi
    return
  fi

  for attempt in $(seq 1 "$GEOM_ATTEMPTS"); do
    GEOM_WM="$(plat_window_set "$pid" "$handle" "$fw" "$fh")"
    sleep 2
    inner="$(app_surface "$log")"
    echo "  geometry attempt $attempt: asked frame ${fw}x${fh}, wm says ${GEOM_WM:-?}, app allocated ${inner:-?}"
    GEOM_ACHIEVED="$inner"
    iw="${inner%x*}"; ih="${inner#*x}"
    case "${iw}${ih}" in
      ''|*[!0-9]*)
        GEOM_WHY="the app never reported a surface, so there is nothing to confirm a size against"
        return ;;
    esac
    if [ "$iw" = "$W" ] && [ "$ih" = "$H" ]; then
      GEOM_MET=yes
      GEOM_WHY="surface ${iw}x${ih} reached in $attempt attempt(s)"
      return
    fi
    fw=$((fw + W - iw))
    fh=$((fh + H - ih))
    if [ "$fw" -le 0 ] || [ "$fh" -le 0 ]; then
      # The residual overshot the whole frame, which is what a SCALE FACTOR
      # looks like from here: the app reports physical pixels and no logical
      # frame size can produce this surface. Named, not retried.
      GEOM_WHY="the app allocated ${iw}x${ih} for a ${W}x${H} target and the residual is not a decoration -- this looks like a scale factor of about $(awk -v a="$iw" -v b="$W" 'BEGIN { printf "%.2f", a / b }')x. Ask for a surface this display can actually produce"
      return
    fi
  done
  GEOM_WHY="did not converge on ${W}x${H} in $GEOM_ATTEMPTS attempts; last surface was ${GEOM_ACHIEVED:-unknown}"
}

# run_leg <label> <binary> <scene> <position> <run-index>
run_leg() {
  local label="$1" bin="$2" scene="$3" position="$4" run="$5"
  local tag="$scene.$label.r$run"
  # Per arm, not per run. See `commit_for_arm`.
  local leg_commit; leg_commit="$(commit_for_arm "$label")"
  local dir="$OUT_DIR/$tag"
  local log="$dir/app.log" loadf="$dir/load.tsv"
  local script seed pid handle rc=0

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
  #
  #    `SCALE_PIN_ENV` pins the leg to one physical pixel per point, and
  #    XAUTHORITY is what lets it open the display at all. Both are set up at
  #    the top of this file, in one place each, and both are written into the
  #    leg's own env.txt beside its app.log -- provenance, not assumption.
  #
  #    Left unpinned, winit reads XSETTINGS or `Xft.dpi` if either answers, and
  #    otherwise computes a factor from the monitor's physical size and
  #    quantizes it to twelfths (`calc_dpi_factor`, winit 0.30.13). This box
  #    has answered both ways: 13/12 on the six abc-native legs and on
  #    fixed-c, and 1 on fixed-b, prefix-b and the WO-23c pair -- same display,
  #    same binary family, minutes apart. At 13/12 every figure the app
  #    reports in pixels is 1.0833x its size in points, a 40-point top bar
  #    reads as 43.33 px, and a one-pane 1920x1080 leg draws 2880x1555
  #    pictures where a scale-1 leg draws 2880x1560. 13/12 is a value winit
  #    guessed on some legs, not a property of the display.
  #
  #    The remedy is the RECORD, not the pin: `native_row.py` reads winit's
  #    own `Guessed window scale factor:` line and every row prints
  #    `scale=<value>` beside its geometry (and `absent` where the leg never
  #    said), so a row can never again be quoted in unknown units. The pin
  #    narrows the spread; the record is what makes an old row readable.
  #
  #    The pin does bite where it applies. winit 0.30.13,
  #    `src/platform_impl/linux/x11/util/randr.rs`: a parseable
  #    WINIT_X11_SCALE_FACTOR takes the `EnvVarDPI::Scale` arm and is returned
  #    as the scale factor ahead of XSETTINGS, `Xft.dpi` and the physical-size
  #    calculation, unclamped (an unparseable or non-normal value panics).
  #    X11 only; no other backend reads it.
  #
  #    UNPROVEN, and it needs a display that guesses something other than 1:
  #    the WO-23c pair ran pinned and unpinned and BOTH guessed 1, so the pin
  #    was never exercised. The pair, when a 13/12 guess is available again --
  #    no copy of this script, so nothing can drift:
  #
  #      .github/browser-rig/run_measure_native.sh --scenes A --runs 1 \
  #        --out-dir /tmp/scale-pinned
  #      RIG_SCALE_PIN= .github/browser-rig/run_measure_native.sh --scenes A \
  #        --runs 1 --out-dir /tmp/scale-unpinned
  #      grep -h "Guessed window scale factor" /tmp/scale-*/*/app.log
  #      # the pin bites iff the two legs print different factors
  local -a env_args=(
    "XDG_CONFIG_HOME=$dir/config"
    "XDG_CACHE_HOME=$dir/cache"
    "RUST_LOG=info"
    "DISPLAY=$DISPLAY_ARG"
  )
  if [ -n "${XAUTHORITY:-}" ]; then
    env_args+=("XAUTHORITY=$XAUTHORITY")
  fi
  if [ "${#SCALE_PIN_ENV[@]}" -gt 0 ]; then
    env_args+=("${SCALE_PIN_ENV[@]}")
  fi
  if [ "$script" != none ]; then
    env_args+=("SQUALLAR_GESTURE_SCRIPT=$script")
  fi
  printf '%s\n' "${env_args[@]}" > "$dir/env.txt"
  echo "  launch env in $dir/env.txt ($SCALE_REPORT; the row records the scale" \
       "the leg was measured at either way)"
  env "${env_args[@]}" "$bin" > "$log" 2>&1 &
  pid=$!

  # 3. Wait for the app's own first surface line: proof a surface exists at
  #    all. Mined from the macOS lane -- it is a stronger boot signal than a
  #    mapped window, and it is the same signal on both platforms.
  local waited=0
  while [ "$waited" -lt 90 ]; do
    [ -n "$(app_surface "$log")" ] && break
    kill -0 "$pid" 2>/dev/null || { echo "  app exited during boot"; break; }
    sleep 1; waited=$((waited + 1))
  done

  # Resolved ONCE: a second call to build the failure message would be a
  # second observation, and the two can disagree.
  local resolve_err="$dir/resolve.err"
  handle="$(plat_window_resolve "$pid" 2>"$resolve_err")" || handle=""
  if [ -z "$handle" ]; then
    echo "  window not resolved: $(tr '\n' ' ' < "$resolve_err")"
  else
    echo "  window handle=$handle (resolved by pid, never by title)"
  fi

  solve_geometry "$pid" "$handle" "$log"
  echo "  geometry_met=$GEOM_MET achieved=${GEOM_ACHIEVED:-unknown} -- $GEOM_WHY"

  # 4. REFUSE rather than fabricate. A leg that could not be brought to the
  #    asked-for surface is not a slightly worse row; it is a row of another
  #    measurement, and a plausible number in a comparison table is harder to
  #    catch than a missing one.
  if [ "$GEOM_MET" != yes ] && [ "$ALLOW_UNPINNED" != 1 ]; then
    echo "ROW $tag REFUSED (geometry): $GEOM_WHY"
    echo "  pass --allow-unpinned to take the row anyway; the analyser will"
    echo "  still mark it INVALID."
    kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null
    return 1
  fi

  # 5. Sample load for the whole leg, every 5 s.
  ( while kill -0 "$pid" 2>/dev/null; do
      printf '%s\t%s\n' "$(date +%s)" "$(plat_loadavg)" >> "$loadf"
      sleep 5
    done ) &
  local sampler=$!

  # 6. Wait for the markers the bracket needs, then hold so liveness has a
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

  # 7. A QUIET LOG IS NOT A HANG. Before anything concludes the app wedged,
  #    ask the scheduler: two CPU-time readings ten seconds apart. A lane
  #    killed a healthy leg for want of exactly this.
  if [ "$seen" -lt "$need" ] && kill -0 "$pid" 2>/dev/null; then
    local t0 t1
    t0="$(plat_cpu_time "$pid")"
    sleep 10
    t1="$(plat_cpu_time "$pid")"
    echo -n "  INCOMPLETE, cpu-time probe: "
    "$PY" "$ROW_PY" cputime --before "${t0:-}" --after "${t1:-}" --wall 10
  fi

  sleep "$HOLD"

  # 8. Stop sampling, stop the app, analyse.
  kill "$sampler" 2>/dev/null; wait "$sampler" 2>/dev/null
  kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null

  # The window manager's reading is passed as the SECOND opinion. The analyser
  # takes the app's own surface line as authoritative and reports a
  # disagreement rather than silently preferring either.
  local -a geom_args=()
  [ -n "$GEOM_WM" ] && geom_args=(--achieved-geom "$GEOM_WM")

  "$PY" "$ROW_PY" analyze \
    --log "$log" --scene "$scene" --script "$script" --commit "$leg_commit" \
    --asked-geom "${W}x${H}" ${geom_args[@]+"${geom_args[@]}"} \
    --refresh "$REFRESH" --adapter "$ADAPTER" --panel "$PANEL" \
    --platform "$PLAT_NAME" --degraded "${PLAT_DEGRADED:-}" \
    --position "$position" --load-file "$loadf" --quiet-max "$QUIET_MAX" \
    --skip-loops "$SKIP_LOOPS" --window-loops "$WINDOW_LOOPS" \
    --json "$OUT_DIR/$tag.json"
  rc=$?
  return "$rc"
}

# ------------------------------------------------------------------ order ----
#
# ABBA rather than base-first-twice, because un-counterbalanced order plus box
# load explained a 22.6-vs-45.3 ms disagreement that cost the campaign days.
# Order is a confound; counterbalancing is what removes it, and the position
# rides on the row so a reader can check it was removed. The rule is
# `native_row.py`'s `leg_order` -- its property, equal mean position per arm,
# is asserted there rather than eyeballed here.

overall=0
for scene in $SCENES; do
  pos=0
  while IFS=$'\t' read -r armidx run; do
    [ -n "${armidx:-}" ] || continue
    pos=$((pos + 1))
    armspec="${ARMS[$armidx]}"
    label="${armspec%%=*}"
    bin="${armspec#*=}"
    run_leg "$label" "$bin" "$scene" "p${pos}(${label})" "$run" || overall=1
  done < <("$PY" "$ROW_PY" order --arms "${#ARMS[@]}" --runs "$RUNS" \
             $([ "$COUNTERBALANCE" = 1 ] && echo --counterbalance))
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
