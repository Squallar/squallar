#!/usr/bin/env bash
#
# run_wall_arm.sh -- the WEB MEMORY WALL arm (M1). A device arm, NEVER a gate.
#
# The web build links wasm with a 1 GiB maximum and `heap.js` picks a page and
# a worker ceiling per device from its pointer type. Both are guesses: nobody
# had measured where any device's wall actually is. This arm measures it, per
# (device, browser, scene), in three readings that are never added together:
#
#   1. the CALIBRATE leg (`calibrate.html`, served at /rig/calibrate.html):
#      which `shared` WebAssembly.Memory maximum constructs (4096 .. 128 MiB),
#      and how many MiB of incompressible, touched linear memory the page
#      survives before the OS kills it, `grow` refuses, or the 4 GiB policy cap.
#   2. the SCENE legs, the current build until the page dies or RIG_WALL_SECONDS
#      pass (default 150): WALL1 (the Tier-2 gate's own scene), WALL4 (four
#      sites, every layer, loops playing) and WALL6 (HEAVY6, the six-pane
#      design scene) -- from `run_measure.sh`'s scene table, via `native_row.py
#      scene`. Each is `drive.py --until-death`: a reload or a crashed page is
#      the READING, and the row keeps the last sample before it.
#   3. the same scene leg read a SECOND way: every page runs with serve.py
#      `--console-beacon`, and `drive.py analyze-console` turns the beaconed log
#      into the row a phone nobody drives would produce. The summary puts the
#      driven and the beaconed columns side by side, so the no-driver route is
#      checked on every leg that has a driver.
#
# A desktop "no death in 150 s" is a legitimate reading and the summary says it
# plainly. Firefox governs and runs first.
#
# Usage:
#   run_wall_arm.sh [--skip-build] [--no-calibrate] [--scenes "WALL1 WALL6"] BROWSER...
#       BROWSER: chromium | firefox | safari | android-chromium | android-firefox
#   run_wall_arm.sh --serve-only SCENE
#       serve one scene (and the calibrate page) for a device no driver reaches
#       -- a home-screen web app -- until Ctrl-C; readings land in the report
#       log and `drive.py analyze-console` reads them. See
#       .claude/skills/iphone-safari-rig.
#
# Environment (all optional):
#   SQUALLAR_WEB_DIR     dir to serve                 (default <repo>/squallar-web)
#   RIG_OUT_DIR          output dir                   (default <rig>/out-wall)
#   RIG_CHROMEDRIVER / RIG_GECKODRIVER / RIG_SAFARIDRIVER   drivers
#   RIG_WALL_SECONDS     scene window, seconds        (default 150)
#   RIG_WALL_SCENES      scene legs                   (default "WALL1 WALL4 WALL6")
#   RIG_CAL_CAP_MIB      calibrate policy cap         (default 4096)
#   RIG_CAL_STEP_MIB     calibrate MiB per grow       (default 32)
#   RIG_CAL_TIMEOUT      calibrate leg timeout, s     (default 900)
#   RIG_ARM              software | hardware. Default: software on Linux (a
#                        headless browser, no window on the user's desktop;
#                        the arm is recorded on every row), hardware elsewhere.
#                        Android and Safari legs are always the device's own.
#   RIG_DEVICE_CLASS     the ledger's device_class    (default <os>-desktop-<arch>)
#   RIG_SAFARI_IOS_UDID  safari legs drive THIS iOS device through safaridriver
#   RIG_ADB_SERIAL       android legs: which device
#   RIG_TLS_CERT / RIG_TLS_KEY  serve https with this pair (a phone on the LAN)
#   RIG_SERVE_HOST       bind address                 (default 127.0.0.1)
#   RIG_URL_HOST         host the browser is pointed at (default 127.0.0.1)
#   RIG_SERVE_PORT       port                         (default 0; 8443 for --serve-only)
#   RIG_WALL_MEMMAX / RIG_WALL_TASKSMAX   the leg's cgroup (default 16G / 4096)
#   RIG_WALL_NO_CGROUP=1 run uncapped (a box without systemd-run --user)
#   RIG_BUILD_WRAPPER    prefix for the wasm build, e.g. a box's cargo lock
#   RIG_DRIVE_EXTRA      extra args appended to every drive.py call
#
# EVERY BROWSER LEG RUNS IN A MEMORY-CAPPED CGROUP (systemd-run --user --scope,
# MemoryMax=16G by default). The calibrate leg grows a page to 4 GiB of
# incompressible bytes on purpose, and a scene leg is run until it dies; an
# uncapped runaway on a box with no swap is a frozen desktop rather than a
# named oom-kill. Scopes are named rd-probe-wall-<leg>-<pid>.

set -u -o pipefail   # not -e: attempt every leg and still summarise

RIG_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$RIG_DIR/../.." && pwd)"
WEB_DIR="${SQUALLAR_WEB_DIR:-$REPO_ROOT/squallar-web}"
OUT_DIR="${RIG_OUT_DIR:-$RIG_DIR/out-wall}"
PY=python3
SECONDS_WINDOW="${RIG_WALL_SECONDS:-150}"
SCENES="${RIG_WALL_SCENES-WALL1 WALL4 WALL6}"
CAL_CAP="${RIG_CAL_CAP_MIB:-4096}"
CAL_STEP="${RIG_CAL_STEP_MIB:-32}"
CAL_TIMEOUT="${RIG_CAL_TIMEOUT:-900}"
MEMMAX="${RIG_WALL_MEMMAX:-16G}"
TASKSMAX="${RIG_WALL_TASKSMAX:-4096}"
SERVE_HOST="${RIG_SERVE_HOST:-127.0.0.1}"
URL_HOST="${RIG_URL_HOST:-127.0.0.1}"
IOS_UDID="${RIG_SAFARI_IOS_UDID:-}"
if [ "$(uname -s)" = Linux ]; then DEFAULT_ARM=software; else DEFAULT_ARM=hardware; fi
ARM="${RIG_ARM:-$DEFAULT_ARM}"
DEVICE_CLASS="${RIG_DEVICE_CLASS:-$(uname -s | tr '[:upper:]' '[:lower:]')-desktop-$(uname -m)}"

SKIP_BUILD=0
CALIBRATE=1
SERVE_ONLY=""
BROWSER_ARGS=()
while [ $# -gt 0 ]; do
  case "$1" in
    --skip-build)   SKIP_BUILD=1 ;;
    --no-calibrate) CALIBRATE=0 ;;
    --scenes)       shift; SCENES="${1?--scenes needs a list (empty: calibrate only)}" ;;
    --serve-only)   shift; SERVE_ONLY="${1:?--serve-only needs a scene}" ;;
    -h|--help)      sed -n '2,64p' "$0"; exit 0 ;;
    -*)             echo "unknown argument: $1" >&2; exit 64 ;;
    *)              BROWSER_ARGS+=("$1") ;;
  esac
  shift
done

mkdir -p "$OUT_DIR"

TLS_ARGS=()
SCHEME=http
if [ -n "${RIG_TLS_CERT:-}" ] || [ -n "${RIG_TLS_KEY:-}" ]; then
  TLS_ARGS=(--tls-cert "${RIG_TLS_CERT:?RIG_TLS_KEY without RIG_TLS_CERT}" \
            --tls-key "${RIG_TLS_KEY:?RIG_TLS_CERT without RIG_TLS_KEY}")
  SCHEME=https
fi

seed_of() {  # the scene's localStorage seed, out of run_measure.sh's own table
  "$PY" "$RIG_DIR/native_row.py" scene --scene "$1"
}

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
    [ -n "$pgid" ] && kill -TERM -- "-$pgid" 2>/dev/null
    rm -f "$f"
  done
  exit "$rc"
}
trap cleanup EXIT INT TERM

# start_server <tag> <port> [serve.py args...]: sets PORT and BASE_URL.
start_server() {
  local tag="$1" port="$2"
  shift 2
  local ready="$OUT_DIR/$tag.serve.ready"
  : > "$ready"
  "$PY" "$RIG_DIR/serve.py" --dir "$WEB_DIR" --port "$port" --host "$SERVE_HOST" \
      --log "$OUT_DIR/$tag.serve.log" --coep \
      ${TLS_ARGS[@]+"${TLS_ARGS[@]}"} "$@" \
      > "$ready" 2>> "$OUT_DIR/$tag.serve.stderr" &
  SERVER_PID=$!
  PORT=""
  local _tag _base
  for _ in $(seq 1 100); do
    if ! kill -0 "$SERVER_PID" 2>/dev/null; then
      echo "FATAL: serve.py exited early:" >&2
      tail -20 "$OUT_DIR/$tag.serve.stderr" >&2
      SERVER_PID=""
      return 1
    fi
    if [ -s "$ready" ]; then
      read -r _tag PORT _base < "$ready"
      break
    fi
    sleep 0.1
  done
  [ -n "$PORT" ] || { echo "FATAL: serve.py never printed its ready line" >&2; return 1; }
  BASE_URL="$SCHEME://$URL_HOST:$PORT"
  return 0
}

# ------------------------------------------------------------ serve-only ----
if [ -n "$SERVE_ONLY" ]; then
  seed="$(seed_of "$SERVE_ONLY")" || { echo "FATAL: no scene $SERVE_ONLY in run_measure.sh" >&2; exit 64; }
  [ -f "$WEB_DIR/pkg/squallar_web_bg.wasm" ] || { echo "FATAL: $WEB_DIR/pkg missing -- build first" >&2; exit 1; }
  stamp="$(date -u +%Y%m%dT%H%M%SZ)"
  log="$OUT_DIR/serve-only.$SERVE_ONLY.$stamp.report.jsonl"
  start_server "serve-only.$SERVE_ONLY" "${RIG_SERVE_PORT:-8443}" \
      --seed-local-storage "$seed" --console-beacon --instrument-index \
      --report-log "$log" || exit 1
  echo "serving scene $SERVE_ONLY (seed sha256 $(printf '%s' "$seed" | sha256sum | cut -d' ' -f1))"
  echo "  scene, Safari tab:      $BASE_URL/index-rig.html"
  echo "  scene, home-screen app: $BASE_URL/   (add THIS page to the home screen; start_url ./ is instrumented)"
  echo "  calibrate:              $BASE_URL/rig/calibrate.html?run=\$(any fresh id)&cap_mib=$CAL_CAP&step_mib=$CAL_STEP"
  echo "  report log:             $log"
  echo "  afterwards:             $PY $RIG_DIR/drive.py analyze-console --log $log --tag <tag> --out $OUT_DIR"
  echo "Ctrl-C to stop."
  wait "$SERVER_PID"
  exit 0
fi

# ------------------------------------------------------------- browsers ----
[ ${#BROWSER_ARGS[@]} -gt 0 ] || { echo "usage: run_wall_arm.sh [--skip-build] [--no-calibrate] [--scenes LIST] BROWSER..." >&2; exit 64; }
for b in "${BROWSER_ARGS[@]}"; do
  case "$b" in
    chromium|firefox|safari|android-chromium|android-firefox) ;;
    *) echo "unknown browser: $b (chromium|firefox|safari|android-chromium|android-firefox)" >&2; exit 64 ;;
  esac
done
for s in $SCENES; do
  seed_of "$s" > /dev/null || { echo "FATAL: run_measure.sh has no scene $s" >&2; exit 64; }
done

# The driver's own pins first, as run_tier2.sh and run_measure.sh do: a red
# selftest means every reader below is suspect.
"$PY" "$RIG_DIR/drive.py" --selftest > "$OUT_DIR/drive.selftest.log" 2>&1 || {
  echo "FATAL: drive.py --selftest failed; see $OUT_DIR/drive.selftest.log" >&2
  exit 1
}

CGROUP=0
if [ "${RIG_WALL_NO_CGROUP:-0}" != 1 ] && command -v systemd-run >/dev/null 2>&1; then
  CGROUP=1
fi
echo "wall arm: browsers=${BROWSER_ARGS[*]} scenes=$SCENES window=${SECONDS_WINDOW}s arm=$ARM device_class=$DEVICE_CLASS"
if [ "$CGROUP" = 1 ]; then
  echo "wall arm: every leg in a scope, MemoryMax=$MEMMAX TasksMax=$TASKSMAX"
else
  echo "wall arm: WARNING -- legs run UNCAPPED (no systemd-run, or RIG_WALL_NO_CGROUP=1)"
fi

GECKODRIVER="${RIG_GECKODRIVER:-}"
CHROMEDRIVER="${RIG_CHROMEDRIVER:-$(command -v chromedriver || echo /usr/bin/chromedriver)}"
SAFARIDRIVER="${RIG_SAFARIDRIVER:-$(command -v safaridriver || echo /usr/bin/safaridriver)}"
case " ${BROWSER_ARGS[*]} " in
  *firefox*)
    if [ -z "$GECKODRIVER" ]; then
      GECKODRIVER="$(bash "$RIG_DIR/ensure-geckodriver.sh")" || { echo "FATAL: ensure-geckodriver.sh failed" >&2; exit 1; }
    fi ;;
esac

if [ "$SKIP_BUILD" -eq 0 ]; then
  echo "building squallar-web (wasm-pack through wasm-threads.sh)"
  # shellcheck disable=SC2086
  (cd "$REPO_ROOT" && CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}" ${RIG_BUILD_WRAPPER:-} \
    .github/scripts/wasm-threads.sh \
    wasm-pack build squallar-web --target web --release --no-typescript --no-pack) || {
    echo "FATAL: wasm-pack build failed" >&2
    exit 1
  }
fi
WASM="$WEB_DIR/pkg/squallar_web_bg.wasm"
[ -f "$WASM" ] || { echo "FATAL: $WASM missing -- build first" >&2; exit 1; }

EXTRA=()
if [ -n "${RIG_DRIVE_EXTRA:-}" ]; then
  # shellcheck disable=SC2206
  EXTRA+=($RIG_DRIVE_EXTRA)
fi

new_run_id() { printf 'wall-%s-%s-%s' "$(date -u +%Y%m%dT%H%M%SZ)" "$$" "$RANDOM"; }

# capped <leg> <command...>: the command inside the leg's memory-capped scope.
capped() {
  local leg="$1"
  shift
  if [ "$CGROUP" = 1 ]; then
    local unit
    unit="rd-probe-wall-$(printf '%s' "$leg" | tr -c 'A-Za-z0-9_.-' '-')-$$"
    systemd-run --user --scope -q --unit "$unit" \
      -p "MemoryMax=$MEMMAX" -p "MemorySwapMax=0" -p "TasksMax=$TASKSMAX" -- "$@"
  else
    "$@"
  fi
}

# browser_args <browser>: sets BROWSER, DRIVER and LEG_ARGS for a browser name.
browser_args() {
  LEG_ARGS=()
  case "$1" in
    chromium)         BROWSER=chromium; DRIVER="$CHROMEDRIVER"; LEG_ARGS+=(--arm "$ARM") ;;
    firefox)          BROWSER=firefox;  DRIVER="$GECKODRIVER";  LEG_ARGS+=(--arm "$ARM") ;;
    safari)           BROWSER=safari;   DRIVER="$SAFARIDRIVER"; LEG_ARGS+=(--arm hardware)
                      [ -n "$IOS_UDID" ] && LEG_ARGS+=(--safari-ios "$IOS_UDID") ;;
    android-chromium) BROWSER=chromium; DRIVER="$CHROMEDRIVER"; LEG_ARGS+=(--arm hardware --android) ;;
    android-firefox)  BROWSER=firefox;  DRIVER="$GECKODRIVER";  LEG_ARGS+=(--arm hardware --android) ;;
  esac
  case "$1" in
    android-*) [ -n "${RIG_ADB_SERIAL:-}" ] && LEG_ARGS+=(--adb-serial "$RIG_ADB_SERIAL") ;;
  esac
}

LEGS=()
overall=0

# check_binding <tag> <run_id>: the artefact on disk is THIS run's.
check_binding() {
  "$PY" - "$OUT_DIR/$1.json" "$2" <<'EOF'
import json, sys
try:
    got = json.load(open(sys.argv[1])).get("run_id")
except (OSError, ValueError):
    sys.exit(1)
sys.exit(0 if got == sys.argv[2] else 1)
EOF
}

for b in "${BROWSER_ARGS[@]}"; do
  browser_args "$b"
  if [ "$CALIBRATE" = 1 ]; then
    tag="$b.calibrate"
    rm -f "$OUT_DIR/$tag.json" "$OUT_DIR/$tag.report.jsonl"
    run_id="$(new_run_id)"
    echo
    echo "================ $tag ================"
    if start_server "$tag" "${RIG_SERVE_PORT:-0}" --report-log "$OUT_DIR/$tag.report.jsonl"; then
      capped "$tag" "$PY" "$RIG_DIR/drive.py" \
          --browser "$BROWSER" --driver "$DRIVER" \
          --url "$BASE_URL/rig/calibrate.html?run=$run_id&cap_mib=$CAL_CAP&step_mib=$CAL_STEP" \
          --out-dir "$OUT_DIR" --tag "$tag" --run-id "$run_id" \
          --calibrate --report-log "$OUT_DIR/$tag.report.jsonl" \
          --calibrate-timeout "$CAL_TIMEOUT" \
          "${LEG_ARGS[@]}" ${EXTRA[@]+"${EXTRA[@]}"}
      rc=$?
      stop_server
      check_binding "$tag" "$run_id" || { echo "$tag: NO RESULT bound to run $run_id (rc=$rc)"; overall=1; }
      [ "$rc" -eq 0 ] || overall=1
    else
      overall=1
    fi
    LEGS+=("$tag")
  fi
  for scene in $SCENES; do
    tag="$b.$scene"
    rm -f "$OUT_DIR/$tag.json" "$OUT_DIR/$tag.samples.tsv" "$OUT_DIR/$tag.report.jsonl" \
          "$OUT_DIR/$tag.analyzed.json" "$OUT_DIR/$tag.analyzed.samples.tsv"
    seed="$(seed_of "$scene")"
    run_id="$(new_run_id)"
    echo
    echo "================ $tag (until death or ${SECONDS_WINDOW}s) ================"
    if start_server "$tag" "${RIG_SERVE_PORT:-0}" --seed-local-storage "$seed" \
        --console-beacon --report-log "$OUT_DIR/$tag.report.jsonl"; then
      {
        echo "# wall leg=$tag scene=$scene browser=$b arm=$ARM device_class=$DEVICE_CLASS"
        echo "# commit=$(git -C "$REPO_ROOT" rev-parse HEAD 2>/dev/null || echo unknown) tree_dirty=$(git -C "$REPO_ROOT" status --porcelain 2>/dev/null | wc -l)"
        echo "# seed_sha256=$(printf '%s' "$seed" | sha256sum | cut -d' ' -f1) wasm_sha256=$(sha256sum "$WASM" | cut -d' ' -f1)"
        echo "# started_host_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ) window_s=$SECONDS_WINDOW"
      } >> "$OUT_DIR/$tag.samples.tsv"
      cal_args=()
      [ -f "$OUT_DIR/$b.calibrate.json" ] && cal_args=(--calibrate-json "$OUT_DIR/$b.calibrate.json")
      capped "$tag" "$PY" "$RIG_DIR/drive.py" \
          --browser "$BROWSER" --driver "$DRIVER" \
          --url "$BASE_URL/index-rig.html" \
          --out-dir "$OUT_DIR" --tag "$tag" --run-id "$run_id" \
          --until-death --sample-tsv "$OUT_DIR/$tag.samples.tsv" --sample-interval 2 \
          --settle 0 --data-window "$SECONDS_WINDOW" --no-second-raf --frames 30 \
          --boot-timeout 120 \
          ${cal_args[@]+"${cal_args[@]}"} \
          "${LEG_ARGS[@]}" ${EXTRA[@]+"${EXTRA[@]}"}
      rc=$?
      stop_server
      check_binding "$tag" "$run_id" || { echo "$tag: NO RESULT bound to run $run_id (rc=$rc)"; overall=1; }
      [ "$rc" -eq 0 ] || overall=1
      # The same leg, read the no-driver way.
      "$PY" "$RIG_DIR/drive.py" analyze-console --log "$OUT_DIR/$tag.report.jsonl" \
          --tag "$tag.analyzed" --out "$OUT_DIR" --browser "$b" \
          ${cal_args[@]+"${cal_args[@]}"} || true
    else
      overall=1
    fi
    LEGS+=("$tag")
  done
done

# -------------------------------------------------------------- summary ----
echo
echo "================ wall arm summary ($DEVICE_CLASS, arm=$ARM) ================"
"$PY" - "$RIG_DIR" "$OUT_DIR" "$DEVICE_CLASS" "$(git -C "$REPO_ROOT" rev-parse --short=12 HEAD 2>/dev/null || echo unknown)" \
       "$(date -u +%Y-%m-%d)" "$ARM" "$WASM" "$SECONDS_WINDOW" ${LEGS[@]+"${LEGS[@]}"} <<'EOF'
import hashlib, importlib.util, json, os, subprocess, sys
rig, out, device_class, commit, date, arm, wasm, window = sys.argv[1:9]
legs = sys.argv[9:]
spec = importlib.util.spec_from_file_location("drive", os.path.join(rig, "drive.py"))
drive = importlib.util.module_from_spec(spec)
spec.loader.exec_module(drive)

def load(tag):
    try:
        return json.load(open(os.path.join(out, tag + ".json")))
    except (OSError, ValueError):
        return None

wasm_sha = hashlib.sha256(open(wasm, "rb").read()).hexdigest() if os.path.isfile(wasm) else None
# The commit is HEAD of the served checkout; a dirty tree means the rig or the
# app that ran is not exactly that commit, and the evidence says by how much.
repo = os.path.dirname(os.path.dirname(os.path.abspath(rig)))
dirty = subprocess.run(["git", "-C", repo, "status", "--porcelain"],
                       capture_output=True, text=True).stdout.count("\n")
ledger, evidence_dir = [], os.path.join(out, "wall-evidence")
for tag in legs:
    r = load(tag)
    if r is None:
        print("%-26s NO RESULT (%s.json missing)" % (tag, tag))
        continue
    browser_name = tag.rsplit(".", 1)[0]
    version = (r.get("session") or {}).get("browserVersion")
    if r.get("mode") == "calibrate":
        v = r.get("calibrate") or {}
        print("%-26s [%s %s] CALIBRATE ended_by=%s survived=%s MiB ladder_ok=%s "
              "largest=%s MiB coi=%s routes_agree=%s ok=%s"
              % (tag, browser_name, version, v.get("ended_by"), v.get("survived_mib"),
                 v.get("ladder_ok"), v.get("ladder_largest_mib"), v.get("coi"),
                 v.get("routes_agree"), v.get("ok")))
        for e in v.get("errors") or []:
            print("%-26s   error: %s" % ("", e))
        continue
    w = r.get("wall") or {}
    env = r.get("env") or {}
    scene = tag.rsplit(".", 1)[1]
    died = w.get("deaths")
    print("%-26s [%s %s, scene %s, %ss window, coi=%s] %s"
          % (tag, browser_name, version, scene, window, env.get("cross_origin_isolated"),
             ("DIED (%s)" % ",".join(w.get("death_kinds") or []) if died
              else "NO DEATH in the window -- a legitimate reading")))
    print("%-26s   driven:   %s" % ("", drive.wall_row_text(w)))
    a = load(tag + ".analyzed")
    agree = diffs = None
    if a is not None:
        aw = a.get("wall") or {}
        print("%-26s   beaconed: %s" % ("", drive.wall_row_text(aw)))
        diffs = [k for k in drive.WALL_ROW_COLUMNS if w.get(k) != aw.get(k)]
        agree = not diffs
        print("%-26s   routes agree on every column: %s%s"
              % ("", agree, "" if agree else " (differ: %s)" % ", ".join(diffs)))
    if w.get("instantiate_fallbacks"):
        print("%-26s   instantiate fallback fired %s time(s)" % ("", w.get("instantiate_fallbacks")))
    if r.get("smoke_pass") is False:
        print("%-26s   smoke verdict (not a gate here): FAIL -- traps=%s panics=%s"
              % ("", (r.get("verdict") or {}).get("wasm_trap_count"),
                 (r.get("verdict") or {}).get("panic_count")))
    leg_id = "%s-%s-%s-%s" % (date.replace("-", ""), device_class, browser_name, scene)
    cal = load(browser_name + ".calibrate")
    calv = (cal or {}).get("calibrate") or {}
    ev = {
        "leg_id": leg_id, "device_class": device_class, "browser": browser_name,
        "browser_version": version, "scene": scene, "commit": commit, "date": date,
        "arm": arm, "window_s": float(window),
        "cross_origin_isolated": env.get("cross_origin_isolated"),
        "ceiling_page_mib": w.get("page_ceiling_mib"),
        "ceiling_worker_mib": w.get("worker_ceiling_mib"),
        "wall_mib": w.get("wall_mib"),
        "wall": {k: w.get(k) for k in drive.WALL_ROW_COLUMNS},
        "wall_basis": w.get("basis"),
        "death_kinds": w.get("death_kinds"),
        "alloc_instances": w.get("alloc_instances"),
        "first_alloc_failure": w.get("first_alloc_failure"),
        "smoke_pass": r.get("smoke_pass"),
        "beaconed_route_agrees": agree,
        "beaconed_route_differs": diffs,
        "tree_dirty_files": dirty,
        "calibrate": {k: calv.get(k) for k in ("ended_by", "survived_mib", "ladder_ok",
                                               "ladder_largest_mib", "ask", "simulated",
                                               "doctored", "routes_agree", "ok")},
        "provenance": {"leg_json": os.path.join(out, tag + ".json"),
                       "samples_tsv": os.path.join(out, tag + ".samples.tsv"),
                       "report_log": os.path.join(out, tag + ".report.jsonl"),
                       "calibrate_json": os.path.join(out, browser_name + ".calibrate.json"),
                       "run_id": r.get("run_id"), "wasm_sha256": wasm_sha},
    }
    os.makedirs(evidence_dir, exist_ok=True)
    with open(os.path.join(evidence_dir, leg_id + ".json"), "w") as fh:
        json.dump(ev, fh, indent=2, sort_keys=True)
        fh.write("\n")
    missing = [k for k in ("ceiling_page_mib", "ceiling_worker_mib", "wall_mib") if ev[k] is None]
    if missing:
        print("%-26s   no ledger row: %s unread (%s)" % ("", ", ".join(missing), w.get("why_no_wall") or "no budget-state tick carried it"))
    else:
        ledger.append("\t".join(str(x) for x in (device_class, browser_name, scene,
                      ev["ceiling_page_mib"], ev["ceiling_worker_mib"], ev["wall_mib"],
                      leg_id, commit, date)))
print()
print("---- ledger rows for wall-ceilings.tsv (evidence in %s) ----" % evidence_dir)
for row in ledger:
    print(row)
EOF

echo
echo "artifacts in $OUT_DIR"
exit "$overall"
