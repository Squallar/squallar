#!/usr/bin/env python3
"""native_row.py -- the native half of the measurement protocol.

`run_measure.sh` measures the web target through a browser driver. This is its
native counterpart's analyser: it reads the app's own stderr log, brackets a
window with the gesture player's markers, bin-diffs the embedded histograms,
and prints a ROW line carrying the SAME columns `run_measure.sh` prints, so a
native row and a web row sit in one table.

Stdlib only, matching `drive.py`'s idiom. Nothing here gates CI.

---- Why this file owns no regex of its own -------------------------------

Every sentence it scrapes is one `drive.py` already scrapes, and `drive.py`'s
patterns are already pinned against the app's own formatters from the Rust
side (`squallar-app/src/app_render/frame_telemetry_line_tests.rs` and
`raster_telemetry_line_tests.rs` read `drive.py` at compile time). So the
patterns are READ OUT of `drive.py` at run time rather than restated here.

A copy of a literal is a second place for it to be wrong, and that seam has
already broken once: a line-continuation backslash was eaten, the Rust string
gained eighteen spaces, every Rust test stayed green, and the rig reported the
overlay reading as `null` -- indistinguishable from "the overlay path never
ran". Reading through means a pattern drift reddens the EXISTING Rust pins and
this file at once, instead of silently producing a native row with empty
fields.

The readings only this half takes -- the app's surface line and its
`overlay pictures:` line -- are the exceptions, spelled here one line each
and pinned from the Rust side (`native_seed_pin_tests.rs`) against THIS file,
for the same reason.

---- Two instrument facts this file is built around -----------------------

1. `Hist` is FOUR BINS PER OCTAVE. A one-bin difference is 0-19%, and any
   ratio in 1.68-2.38x prints as exactly 2.00x. So a run pair's divergence is
   adjudicated on the UNBINNED interact frame COUNT, never on binned
   percentiles. The binned rule failed in BOTH directions: it over-fired on
   legs agreeing to 1.8% (41.4% apparent, two bins apart) and was falsely
   reassuring at 0.0% on legs whose real throughput differed 8.4%, because
   their p99s landed in the same bin. Percentiles are printed as distribution
   SHAPE and are labelled binned.

2. Percentiles CLAMP at the top bin (`over`), which scene E legs do routinely.
   On a clamped leg the frame COUNT is the throughput figure, and the row says
   so rather than printing a ceiling as if it were a measurement.

---- Windowing --------------------------------------------------------------

Histograms are cumulative from boot. A windowed reading is the DIFFERENCE of
two of them, bracketed by the gesture player's own `loop complete` markers so
the bracket is whole script loops. Cumulative-from-boot figures contaminated
an entire early scoreboard; nothing here ever quotes one as a window.

---- The runner's decisions live here, not in the shell ---------------------

`run_measure_native.sh` is a driver: it launches a binary, moves a window and
writes files. Every DECISION it makes -- which platform tools a leg may use,
whether the box stayed quiet, whether a silent app is wedged or working, what
order the legs run in -- is a pure function in this file, called from the
shell. That split is not tidiness. A decision spelled in bash can only be
checked by running a leg, which needs a display, a GPU and three minutes; the
same decision spelled here is checked by `--selftest` in milliseconds on any
machine. Four of the five capabilities that lanes kept rebuilding privately
were decisions, not mechanism.
"""

import argparse
import json
import math
import os
import re
import sys
import unittest

RIG_DIR = os.path.dirname(os.path.abspath(__file__))
DRIVE_PY = os.path.join(RIG_DIR, "drive.py")
RUN_MEASURE_SH = os.path.join(RIG_DIR, "run_measure.sh")

# --------------------------------------------------------------- histogram --

GEOMETRIC_BINS = 40
SLOTS = GEOMETRIC_BINS + 2
# `squallar_device_profile::hist`'s first octave, nanoseconds: floor(62500 *
# 2^(j/4)) for j = 0..4. Every later edge is one of these shifted left, so
# bins exactly four apart differ by exactly a factor of two -- which is the
# whole reason a one-bin difference cannot be read as a throughput figure.
FIRST_OCTAVE_NS = (62_500, 74_325, 88_388, 105_112)


def edge_ns(i):
    """Bin edge `i` in nanoseconds, for i in 0..=GEOMETRIC_BINS."""
    return FIRST_OCTAVE_NS[i % 4] << (i // 4)


def percentile_upper_micros(counts, q):
    """`Hist::percentile_upper_micros`, replicated over a diffed count array.

    The conservative q-quantile in microseconds: the upper edge (rounded up)
    of the bin holding the ceil(q*total)-th smallest sample. `None` on an
    empty histogram, `"over"` when the ranked sample sits in the at-or-over-
    64 ms clamp, whose upper edge does not exist.
    """
    total = sum(counts)
    if total == 0:
        return None
    rank = min(max(int(math.ceil(q * total)), 1), total)
    seen = 0
    for slot, count in enumerate(counts):
        seen += count
        if seen >= rank:
            if slot == SLOTS - 1:
                return "over"
            return -(-edge_ns(slot) // 1000)
    raise AssertionError("total() counted a sample the walk did not reach")


def fmt_pctl(v):
    if v is None:
        return "none"
    return str(v)


# ------------------------------------------------------- patterns, borrowed --


def _read(path):
    with open(path, "r", encoding="utf-8") as fh:
        return fh.read()


def drive_pattern(name, source=None):
    """The body of a `var <name> = /.../;` regex literal in `drive.py`.

    Same extraction the Rust pins use, for the same reason. A moved or renamed
    probe raises here rather than yielding a row of empty fields.
    """
    text = source if source is not None else _read(DRIVE_PY)
    head = "var %s = /" % name
    at = text.find(head)
    if at < 0:
        raise SystemExit(
            "drive.py no longer declares `%s...`; the rig's probe for this "
            "line moved and native_row.py can no longer read it" % head
        )
    rest = text[at + len(head):]
    end = rest.find("/;")
    if end < 0:
        raise SystemExit(
            "the `%s` regex literal in drive.py is not closed on its own line"
            % name
        )
    return rest[:end]


# The probes this analyser needs. Every one is `drive.py`'s, unmodified: the
# JS and Python flavours agree on all of `\d`, `\(`, `\/`, `[0-9,]+`,
# `[a-z0-9]+`, `[a-z0-9-]+`.
PROBE_NAMES = (
    "svc_interact_re",
    "svc_idle_re",
    "cadence_re",
    "segments_re",
    "rasters_re",
    "uploads_re",
    "basemap_re",
    "ground_re",
    "floor_re",
    "loop_state_re",
    "budget_state_re",
    "tile_cache_re",
    "gesture_begin_re",
    "gesture_loop_re",
)


def compile_probes(source=None):
    text = source if source is not None else _read(DRIVE_PY)
    return {n: re.compile(drive_pattern(n, text)) for n in PROBE_NAMES}


# ------------------------------------------------------------- row columns --


def shared_row_keys(source=None):
    """The column keys `run_measure.sh` prints on its ROW line, in order.

    Read out of the script rather than restated, so a sibling lane changing
    the web row reddens `row_columns_match_the_web_rig` instead of silently
    producing two tables that cannot be read side by side.
    """
    text = source if source is not None else _read(RUN_MEASURE_SH)
    at = text.find('print("ROW scene=')
    if at < 0:
        raise SystemExit(
            "run_measure.sh no longer prints a `ROW scene=` line; the shared "
            "row format moved and native_row.py can no longer match it"
        )
    rest = text[at:]
    end = rest.find("% (scene")
    if end < 0:
        raise SystemExit("could not find the end of run_measure.sh's ROW format")
    fmt = rest[:end]
    # `hz~%s` is the one column spelled with a tilde rather than an equals.
    keys = re.findall(r"([A-Za-z][A-Za-z_/]*)=%s|(hz)~%s", fmt)
    out = []
    for eq, tilde in keys:
        out.append(eq if eq else "hz~")
    return out


# The columns a native leg adds. They are not decoration: a row measured while
# the box was loaded is not comparable to one measured quiet, and a start-of-
# leg gate cannot see a compile that begins after the gate passes. The error is
# ONE-SIDED -- load depresses only the cheaper-frame arm -- so it biases ratios
# rather than adding noise, which is why `quiet` is a stamp on the row and not
# a footnote in a log.
# `loadavg_start/end/max` USED to live here. They are shared now: the web row
# grew them too, because a timing row that cannot say whether the box was busy
# underneath it cannot be defended after the fact, and that is as true of a
# browser leg as of a native one. They are still printed here, in the same
# spelling and the same place -- `shared_row_keys()` reads the spelling out of
# run_measure.sh, so the two cannot drift apart without reddening a test.
#
# What stays native-only is what only the native runner can answer: `quiet` is
# a verdict over load samples taken THROUGH the leg against a ceiling, which
# the web path does not compute, and `position` is the matrix slot.
NATIVE_ONLY_ROW_KEYS = (
    "quiet",
    "position",
)

# A leg is quiet if the 1-minute load average never reached this during the
# measurement window. Chosen against the box's own core count at run time by
# the runner; this is the fallback for a machine that cannot report one.
DEFAULT_QUIET_MAX = 8.0

# `run_measure.sh` step 6: two runs, a third when they diverge. Adjudicated on
# the unbinned interact frame count -- see this module's header.
DIVERGENCE_THRESHOLD = 0.15


# ------------------------------------------------------------------ seeding --

# `squallar_web::kv::storage_key`'s prefix. The web rig seeds localStorage
# entries named `squallar.<key>`; the native `FileKvStore` reads `<key>.json`
# out of its config dir. Mapping between them is the whole of native seeding,
# and doing it HERE rather than restating the scene means the two targets can
# never measure different scenes under the same letter.
WEB_KEY_PREFIX = "squallar."


def _scene_block(source=None):
    """`run_measure.sh`'s scene definitions, as sourceable bash.

    The scenes are READ OUT of the web runner and evaluated by bash itself
    rather than reimplemented here. Reimplementing them is how scene A on
    native and scene A on the web quietly become two different scenes; and
    bash's own parser is the only thing that gets the nested quoting in those
    seeds right.
    """
    text = source if source is not None else _read(RUN_MEASURE_SH)
    lines = text.splitlines()
    start = next((i for i, l in enumerate(lines) if l.startswith("ALL_LAYERS=")), None)
    if start is None:
        raise SystemExit(
            "run_measure.sh no longer defines `ALL_LAYERS=`; the scene "
            "definitions moved and the native runner cannot reuse them"
        )
    stop = next(
        (i for i, l in enumerate(lines) if "scene-B denominator columns" in l), None
    )
    if stop is None or stop <= start:
        raise SystemExit("could not find the end of run_measure.sh's scene block")
    # Back up to the closing brace of `scene_script`, so no trailing comment
    # or partial statement rides along into the sourced text.
    end = max(i for i in range(start, stop) if lines[i] == "}")
    return "\n".join(lines[start:end + 1])


def scene_from_shell(scene, panel="off", what="seed"):
    """Evaluate `run_measure.sh`'s own `scene_seed`/`scene_script` for a scene."""
    import subprocess

    fn = "scene_seed" if what == "seed" else "scene_script"
    out = subprocess.run(
        ["bash", "-c", "%s\n%s %s" % (_scene_block(), fn, scene)],
        capture_output=True, text=True, env=dict(os.environ, PANEL=panel),
    )
    if out.returncode != 0:
        raise SystemExit(
            "run_measure.sh does not define scene %r (%s)"
            % (scene, out.stderr.strip())
        )
    return out.stdout.strip()


def cmd_scene(args):
    print(scene_from_shell(args.scene, args.panel, args.what))
    return 0


def seed_files(web_seed):
    """`{localStorage key: value}` -> `{config filename: value}`.

    `FileKvStore::path_for` is `dir.join(format!("{key}.json"))`, so the file
    name IS the key. A key that lost its prefix would silently seed nothing,
    and the leg would then measure a default app with telemetry OFF -- a
    quiet wrong answer rather than a loud failure, which is why this raises.
    """
    out = {}
    for k, v in web_seed.items():
        if not k.startswith(WEB_KEY_PREFIX):
            raise ValueError(
                "seed key %r does not carry the `%s` prefix the web store "
                "uses; the native config file it maps to cannot be derived"
                % (k, WEB_KEY_PREFIX)
            )
        out[k[len(WEB_KEY_PREFIX):] + ".json"] = v
    return out


def cmd_seed(args):
    """Seed a redirected config dir from the web rig's own scene seed.

    Into a REDIRECTED `XDG_CONFIG_HOME`, never the user's real config: a
    measurement must not be able to overwrite the config of the person
    running it, and a leg must start from a known scene rather than from
    whatever the last session persisted.
    """
    web_seed = json.load(sys.stdin)
    files = seed_files(web_seed)
    os.makedirs(args.config_dir, exist_ok=True)
    for name, value in sorted(files.items()):
        path = os.path.join(args.config_dir, name)
        with open(path, "w", encoding="utf-8") as fh:
            fh.write(value)
        print("seeded %s (%d B)" % (path, len(value)))
    return 0


# ------------------------------------------------------------------ parsing --


class Reading(object):
    __slots__ = ("idx", "n", "p50", "p90", "p99", "hist")

    def __init__(self, idx, n, p50, p90, p99, hist):
        self.idx = idx
        self.n = n
        self.p50 = p50
        self.p90 = p90
        self.p99 = p99
        self.hist = hist


def parse_hist(text):
    counts = [int(x) for x in text.split(",")]
    if len(counts) != SLOTS:
        raise ValueError(
            "a histogram carried %d slots, not %d: the instrument's shape "
            "changed and every windowed figure here is derived from it"
            % (len(counts), SLOTS)
        )
    return counts


# The app's own report of the surface it resized to, `App::handle_resized`'s
# `log::info!("Window resized to {}x{}", width, height)`.
#
# One of the three patterns in this file that is not `drive.py`'s, and it
# earns that the same way `wgpu selected the ` does -- by being the only reading
# of a quantity nothing else can supply. It is the ONE geometry readback that
# exists on every platform: `xdotool` answers on X, System Events answers on
# macOS, nothing answers on Wayland, and all three of those are the window
# manager's opinion of a frame, while THIS is the surface the app actually
# allocated. So it is what the geometry check is taken against, and the WM's
# answer is demoted to a second opinion.
#
# Pinned against the app's own formatter from the Rust side, in
# `native_seed_pin_tests.rs`, exactly as `drive.py`'s patterns are -- because a
# copy of a literal is a second place for it to be wrong.
# TWO sentences, because the app has two moments where it knows its surface
# and only one of them is a change. `Surface configured to` is printed once at
# startup; `Window resized to` on every later resize. Reading only the second
# refused a leg whose window opened at exactly the requested size -- no resize
# event, no line, no confirmable surface -- which is the CORRECT case.
SURFACE_RE = re.compile(r"(?:Window resized to|Surface configured to) (\d+)x(\d+)")

# The app's own report of the overlay picture it allocates for EACH pane:
# `overlay pictures: n=<panes>, px=<w>x<h>[;<w>x<h>...], bytes=<sum>`
# (`squallar_app::budget_telemetry::overlay_pictures_line`), said once a
# telemetry period as a LEVEL. `px` is in pane-index order, physical pixels
# (the DPR is inside them); `0x0` for a pane without a picture -- listed, not
# skipped, so position IS the pane index -- and EMPTY at `n=0`: a scene with
# no panes is a reading, not a period the scraper missed, so the empty list
# is matched and parsed to `[]` rather than left to read as an absent line.
# `bytes` is the sum of w*h*4 over the list. It is the figure the surface
# check compares a bracket's uploads against. The third pattern of this
# file's own, on `SURFACE_RE`'s terms: the harness used to MODEL this figure
# from the surface and a 40-point top bar -- a model exact when written that
# expired silently (see `surface_check`) -- so the quantity is read from the
# only thing that knows it. Every group is mandatory; no match leaves the
# family empty, which the row prints as UNCHECKED -- a binary older than the
# line, never a zero-size picture.
OVERLAY_PICTURES_RE = re.compile(r"overlay pictures: n=(\d+), px=((?:\d+x\d+(?:;\d+x\d+)*)?), bytes=(\d+)")


def overlay_pictures_reading(m):
    """An `OVERLAY_PICTURES_RE` match as `{"n", "px": [(w, h), ...], "bytes"}`."""
    listed = m.group(2)
    px = ([tuple(int(v) for v in item.split("x")) for item in listed.split(";")]
          if listed else [])
    return {"n": int(m.group(1)), "px": px, "bytes": int(m.group(3))}


def app_surface(lines):
    """The newest surface the app reported, as `(w, h)`, or None.

    Last match of either sentence wins: the startup line comes first, so a
    later resize legitimately supersedes it.
    """
    found = None
    for line in lines:
        m = SURFACE_RE.search(line)
        if m:
            found = (int(m.group(1)), int(m.group(2)))
    return found


def scrape(lines, probes):
    """Every scraped family, in line order.

    Line INDEX is the clock. The native log has env_logger timestamps, but the
    ordering is what the bracket needs and an index cannot be ambiguous the way
    a second-resolution stamp can when several readings share a second.
    """
    out = {
        "interact": [],
        "idle": [],
        "cadence": [],
        "rasters": [],
        "uploads": [],
        "basemap": [],
        "ground": [],
        "floor": [],
        "loop_state": [],
        "budget_state": [],
        "tile_cache": [],
        "overlay_pictures": [],
        "segments": [],
        "begins": [],
        "loops": [],
        "backend": None,
        "adapter": None,
        "surface": None,
        "gpu_unavailable": False,
    }
    fam = (
        ("interact", "svc_interact_re"),
        ("idle", "svc_idle_re"),
        ("cadence", "cadence_re"),
    )
    for idx, line in enumerate(lines):
        for key, probe in fam:
            m = probes[probe].search(line)
            if m:
                g = m.groups()
                # cadence has no p90; its groups are n, p50, p99, hist.
                if key == "cadence":
                    out[key].append(
                        Reading(idx, int(g[0]), g[1], None, g[2], parse_hist(g[3]))
                    )
                else:
                    out[key].append(
                        Reading(idx, int(g[0]), g[1], g[2], g[3], parse_hist(g[4]))
                    )
        # Running totals and levels: every capture group is a plain `(\d+)`,
        # so these are the families that can be differenced arithmetically.
        for key, probe in (
            ("rasters", "rasters_re"),
            ("uploads", "uploads_re"),
            ("basemap", "basemap_re"),
            ("ground", "ground_re"),
            ("floor", "floor_re"),
            ("loop_state", "loop_state_re"),
        ):
            m = probes[probe].search(line)
            if m:
                out[key].append((idx, [int(x) for x in m.groups()]))
        # `budget state` is a level too, but its first group is the bracket's
        # NAME, so it cannot ride the all-`int()` loop above: the word is kept
        # as text and the fourteen figures after it are ints. Every group is
        # mandatory; no match at all leaves the family empty, which the row
        # prints as absent -- an older binary, never a zero reading.
        m = probes["budget_state_re"].search(line)
        if m:
            g = m.groups()
            out["budget_state"].append((idx, g[0], [int(x) for x in g[1:]]))
        # `tile cache (<role>)` is running totals with a WORD first, like
        # `budget state`: its own arm, the role kept as text and the thirteen
        # figures after it as ints. No match leaves the family empty, which the
        # row prints as n/a -- a binary older than the line, never a zero.
        m = probes["tile_cache_re"].search(line)
        if m:
            g = m.groups()
            out["tile_cache"].append((idx, g[0], [int(x) for x in g[1:]]))
        # `overlay pictures` is a LEVEL with a LIST in it -- every pane's
        # picture size -- so it has its own arm and its own parser. No match
        # leaves the family empty, which the surface check reads as UNCHECKED:
        # a binary older than the line, never a zero-size picture.
        m = OVERLAY_PICTURES_RE.search(line)
        if m:
            out["overlay_pictures"].append((idx, overlay_pictures_reading(m)))
        # `frame segments` is NOT one of them: its percentile groups are
        # `(\d+|none|over)`, and `over` is the top-bin clamp, which has no
        # upper edge and is not a number. Kept as text -- it is reported as
        # shape and is never differenced.
        m = probes["segments_re"].search(line)
        if m:
            out["segments"].append((idx, list(m.groups())))
        m = probes["gesture_begin_re"].search(line)
        if m:
            out["begins"].append((idx, m.group(1)))
        m = probes["gesture_loop_re"].search(line)
        if m:
            out["loops"].append((idx, m.group(1), int(m.group(2))))
        if "wgpu selected the " in line:
            # `wgpu selected the Vulkan backend: NVIDIA GeForce RTX 3090
            # (DiscreteGpu), driver NVIDIA 610.57.04`
            #
            # The ADAPTER IS READ FROM THE APP, never from `glxinfo`. They
            # disagree: on an Xvfb-hosted leg glxinfo answers `llvmpipe`
            # (the GL renderer for that display) while wgpu had selected a
            # discrete NVIDIA adapter over Vulkan, which needs no X server to
            # enumerate. A row naming the software rasteriser for a leg that
            # ran on a 3090 is the wrong denominator in the loudest possible
            # place.
            rest = line.split("wgpu selected the ", 1)[1]
            out["backend"] = rest.split(" backend", 1)[0]
            if ": " in rest:
                tail = rest.split(": ", 1)[1]
                name = tail.split(", driver", 1)[0].strip()
                m = re.match(r"^(.*?)\s*\(([^)]+)\)\s*$", name)
                if m:
                    out["adapter"] = "%s:%s" % (m.group(2), m.group(1))
                else:
                    out["adapter"] = name
        if "gpu passes: unavailable (adapter lacks TIMESTAMP_QUERY)" in line:
            out["gpu_unavailable"] = True
    out["surface"] = app_surface(lines)
    return out


def at_or_before(series, idx):
    """The last reading at or before line `idx`, or None."""
    found = None
    for r in series:
        pos = r.idx if isinstance(r, Reading) else r[0]
        if pos <= idx:
            found = r
        else:
            break
    return found


def after(series, idx):
    """Every reading strictly after line `idx`."""
    out = []
    for r in series:
        pos = r.idx if isinstance(r, Reading) else r[0]
        if pos > idx:
            out.append(r)
    return out


# ------------------------------------------------------------------ windows --


def bracket(loops, skip_loops, window_loops):
    """The line indices of a whole-loop bracket, or a reason it does not exist.

    `skip_loops` whole loops are discarded first -- the player logs `begin` at
    construction, so the frames right after boot are the boot burst, not the
    scene. What remains is exactly `window_loops` completed loops.
    """
    need = skip_loops + window_loops
    if len(loops) < need:
        return None, (
            "only %d `loop complete` markers; a %d-loop window after "
            "skipping %d needs %d" % (len(loops), window_loops, skip_loops, need)
        )
    if skip_loops < 1:
        return None, "skip_loops must be at least 1 to have a marker to start from"
    start = loops[skip_loops - 1][0]
    end = loops[need - 1][0]
    return (start, end), None


def diff_window(series, start_idx, end_idx):
    """A family's windowed reading: the difference of two cumulative ones."""
    base = at_or_before(series, start_idx)
    final = at_or_before(series, end_idx)
    if base is None or final is None:
        return {"error": "no reading inside the bracket"}
    if final.idx == base.idx:
        return {"error": "only one reading inside the bracket; nothing to diff"}
    hist = [f - b for f, b in zip(final.hist, base.hist)]
    if any(h < 0 for h in hist):
        return {"error": "a cumulative histogram went backwards across the bracket"}
    n = final.n - base.n
    return {
        "n": n,
        "hist": hist,
        "p50_us": fmt_pctl(percentile_upper_micros(hist, 0.50)),
        "p90_us": fmt_pctl(percentile_upper_micros(hist, 0.90)),
        "p99_us": fmt_pctl(percentile_upper_micros(hist, 0.99)),
        "max_us": fmt_pctl(percentile_upper_micros(hist, 1.0)),
    }


def diff_totals(series, start_idx, end_idx):
    """A running-total line's windowed delta."""
    base = at_or_before(series, start_idx)
    final = at_or_before(series, end_idx)
    if base is None or final is None:
        return None
    return [f - b for f, b in zip(final[1], base[1])]


# ----------------------------------------------------------------- liveness --


def liveness(interact, end_idx):
    """Is the app still serving frames after the measurement window closed?

    Not hypothetical: the web build froze while rAF and a non-blank canvas both
    reported health, and only a cumulative counter check caught it. A row from
    a dead app describes nothing, so it is stamped INVALID rather than quoted.
    """
    final = at_or_before(interact, end_idx)
    later = after(interact, end_idx)
    if final is None:
        return {"ok": False, "verdict": "no interact reading at the window end"}
    if not later:
        return {
            "ok": False,
            "verdict": "no interact reading after the window closed; the app "
            "was not held open long enough to prove it was still alive",
        }
    grew = later[-1].n - final.n
    return {
        "ok": grew > 0,
        "grew_by": grew,
        "verdict": (
            "interact frames still rising (+%d after the window)" % grew
            if grew > 0
            else "interact frames FROZEN after the window: the app stopped "
            "serving and this row describes a dead app"
        ),
    }


def parse_cpu_time(text):
    """`ps -o time=`'s accumulated CPU time, in seconds.

    One parser for both platforms: BSD `ps` on macOS prints `MM:SS.ss`, procps
    on Linux prints `HH:MM:SS`, and either grows a `DD-` day field on a long
    run. Returns None for anything it cannot read, so a caller distinguishes
    "no CPU time" from "zero CPU time" -- they mean opposite things here.
    """
    text = (text or "").strip()
    if not text:
        return None
    days = 0
    if "-" in text:
        head, text = text.split("-", 1)
        try:
            days = int(head)
        except ValueError:
            return None
    parts = text.split(":")
    if not 1 <= len(parts) <= 3:
        return None
    try:
        nums = [float(p) for p in parts]
    except ValueError:
        return None
    secs = 0.0
    for n in nums:
        secs = secs * 60.0 + n
    return secs + days * 86400.0


def cpu_liveness(before, after, wall_s):
    """**A quiet log is not a hang.** Did the process burn CPU while silent?

    A lane read a log that had stopped printing as a wedged app, killed the
    leg, and only caught itself with a CPU-time check -- the app had been
    working the whole time on a step that logs nothing. The distinction cannot
    be made from the log, by construction, so it is made from the scheduler.

    `busy` is the only claim: CPU time advanced over the interval. It does not
    claim progress -- a spin loop is busy too -- and the verdict says so, so
    that nobody reads this as proof the leg is healthy.
    """
    a, b = parse_cpu_time(before), parse_cpu_time(after)
    if a is None or b is None:
        return {
            "readable": False, "busy": None, "advanced_s": None,
            "verdict": "no CPU-time readings (%r -> %r): a silent log cannot "
                       "be told from a wedged process without them"
                       % (before, after),
        }
    adv = b - a
    frac = (adv / wall_s) if wall_s else None
    return {
        "readable": True,
        "busy": adv > 0,
        "advanced_s": adv,
        "cpu_fraction": frac,
        "verdict": (
            "process burned %.2f s of CPU over %.0f s of wall (%.0f%% of one "
            "core): it is BUSY, not wedged -- though busy is not progress"
            % (adv, wall_s, 100.0 * frac) if adv > 0 else
            "process burned NO CPU over %.0f s of wall: the silent log is a "
            "wedged process, not a quiet stage" % wall_s
        ),
    }


# ------------------------------------------------------------------ surface --


def pane_count_of_seed(web_seed):
    """`pane_count` out of a web seed's `squallar.ui` JSON, 1 when unset.

    One is the app's own default. The seed is the ONLY source: a count
    restated in the runner would be a second place for the scene to be wrong.
    """
    ui = json.loads(web_seed.get("squallar.ui") or "{}")
    return int(ui.get("pane_count", 1))


def scene_pane_count(scene, panel="off"):
    """How many panes `run_measure.sh`'s own seed for `scene` lays out."""
    return pane_count_of_seed(json.loads(scene_from_shell(scene, panel)))


def resolve_pane_count(args):
    """`(panes, source)`, or `(None, why)` when nothing can say."""
    given = getattr(args, "panes", None)
    if given is not None:
        return int(given), "--panes"
    try:
        return scene_pane_count(args.scene, args.panel), "the scene %s seed" % args.scene
    except (SystemExit, ValueError, KeyError, TypeError) as e:
        return None, (
            "scene %r has no readable seed and --panes was not given (%s)"
            % (args.scene, e)
        )



def mixture_tolerance(px):
    """How far a bracket's MEAN bytes/picture may sit from the reported figure.

    Half a texel row plus half a texel column of the largest reported picture,
    in bytes: `2 * (w + h)`. The bracket's pictures are not all the size the
    app reports where the bracket closes -- a recorded six-pane bracket
    averaged 732 B under its 2,995,200 B pane, a MINORITY of pictures one row
    or one column short as the layout settled -- and a minority a row and a
    column off moves the mean by less than half of each. A majority -- every
    picture a row short, which is the layout having moved under the bracket
    -- is not a mixture and is refused. Far under any structural error: the
    next pane count is 33% or more away.
    """
    w, h = max(px, key=lambda p: p[0] * p[1])
    return 2 * (w + h)


def _in_band(observed, lo, hi, tol):
    return (lo - tol) <= observed <= (hi + tol)


def surface_check(asked, achieved, pictures, picture_bytes, reported, panes=None):
    """Are the pictures the app uploaded the pictures it says it draws, at the
    window it was given?

    The expected picture size is READ from the app, not modelled. The model
    was `(W * 1.5) * ((H - 40) * 1.5) * 4` with 40 as "the top bar in points",
    and it was EXACT when written -- verified to the byte at three surfaces
    on 2026-08-31 -- because the bar then rested on its floor: 40 is
    `MIN_BAR_HEIGHT`, `2 * VERTICAL_MARGIN + INTERACT_HEIGHT`, a minimum the
    layout may sit on or above. Then the bar rose 3.33 points off it, with no
    signal: at 1920x1080 a one-pane leg uploads 2880x1555 pictures
    (17,913,600 B) where the model still said 2880x1560 (17,971,200 B), five
    texel rows and 57,600 B on every picture, and every multi-pane row read
    `** INVALID **` against a figure the app no longer produced. A floor used
    as an equality holds exactly until content grows, then stops; and no
    constant repairs that, because the bar's height is layout over content
    and moves with DPI, font and width class, none of which the harness can
    know. So the app says what it allocated -- `overlay pictures:`, one entry
    per pane, physical pixels -- and the check compares the bracket's uploads
    against THAT. What it still refuses is real: a bracket whose pictures are
    not the size the app says it draws.

    Geometry is READ BACK, never requested and trusted: `achieved` is the
    app's own surface line, and a leg run at another size than it asked for
    is refused on it -- the catch for a window manager that silently ran legs
    at 3440x1440. The bytes no longer factorise the surface (that was the
    model); they confirm the uploads are the reported pictures. Their mean is
    allowed `mixture_tolerance` around the span of the reported per-pane
    figures -- one figure when every pane's picture is one size, a band when
    the grid has two, since which pane rastered how often is not in the log.

    `reported` is the parsed `overlay pictures:` reading, or None when the
    log has none -- a binary older than the line. None is UNCHECKED, which is
    neither INVALID nor a zero: the check did not run, and the row says so.
    `panes` is how many panes the scene seeded, when known; the app's own
    count is in the line, and the two disagreeing is a leg of another scene.
    """
    out = {
        "asked": "%dx%d" % asked if asked else None,
        "achieved": "%dx%d" % achieved if achieved else None,
        "geometry_met": bool(asked and achieved and asked == achieved),
        "panes": panes,
        "reported": reported,
        "unchecked": False,
        "met": False,
    }
    if not achieved:
        out["why"] = "no window geometry was read back"
        return out
    if reported is None:
        out["unchecked"] = True
        out["why"] = (
            "overlay pictures line absent: the app did not report its picture "
            "sizes (a binary older than the line), so the bytes were checked "
            "against nothing"
        )
        return out
    px = reported["px"]
    summed = sum(w * h * 4 for w, h in px)
    # The line is held against itself before anything is held against it. A
    # count that is not its list's length, or a sum that is not its sizes',
    # is the instrument broken -- and a broken instrument must not print as a
    # confirmed surface, which is what a lenient reader would make of it.
    if reported["n"] != len(px) or summed != reported["bytes"]:
        out["why"] = (
            "the app's overlay pictures line contradicts itself: n=%d with %d "
            "sizes listed, bytes=%d against %d summed from them"
            % (reported["n"], len(px), reported["bytes"], summed)
        )
        return out
    out["reported_panes"] = reported["n"]
    out["reported_px"] = ";".join("%dx%d" % p for p in px)
    out["reported_bytes"] = reported["bytes"]
    out["panes_met"] = panes is None or panes == reported["n"]
    if not out["panes_met"]:
        out["why"] = (
            "the scene seeds %d pane(s) and the app laid out %d: this row is "
            "of another scene" % (panes, reported["n"])
        )
        return out
    drawn_px = [p for p in px if p[0] and p[1]]  # a 0x0 pane has no picture
    drawn = [w * h * 4 for w, h in drawn_px]
    out["pane_picture_bytes"] = drawn
    if not drawn:
        out["why"] = "the app reports no pane with a picture (n=%d)" % reported["n"] + (
            ", yet %d pictures were drawn in the bracket" % pictures if pictures else ""
        )
        return out
    lo, hi = min(drawn), max(drawn)
    tol = mixture_tolerance(drawn_px)
    # One figure when every pane's picture is one size; None -- never a mean
    # over panes, which describes no picture -- when there are two.
    out["expected_picture_bytes"] = lo if lo == hi else None
    out["expected_picture_bytes_lo"] = lo
    out["expected_picture_bytes_hi"] = hi
    out["expected_label"] = ("%d" % lo) if lo == hi else "%d..%d" % (lo, hi)
    out["tolerance_bytes"] = tol
    if not pictures:
        out["why"] = (
            "no pictures were drawn in the window, so the surface cannot be "
            "confirmed from the bytes"
        )
        return out
    observed = picture_bytes / float(pictures)
    out["observed_picture_bytes"] = observed
    out["bytes_met"] = _in_band(observed, lo, hi, tol)
    if not out["bytes_met"]:
        # The ratio to the nearer reported figure names the shape of the
        # miss: 6.000x is six panes' pictures being priced as one, 1.003x is
        # a bar a few rows taller than something believed.
        nearest = lo if abs(observed - lo) <= abs(observed - hi) else hi
        out["why"] = (
            "picture bytes are not the pictures the app reports: %d B/picture "
            "observed against %s B expected (+-%d) for %d pane(s) at %s -- "
            "%.3fx the reported figure"
            % (observed, out["expected_label"], tol, reported["n"],
               out["reported_px"], observed / float(nearest))
        )
    elif not out["geometry_met"]:
        out["why"] = (
            "the app ran at %s, not the %s the leg asked for; its pictures "
            "match that surface, so this row is of another measurement"
            % (out["achieved"], out["asked"])
        )
    out["met"] = bool(out["geometry_met"] and out["bytes_met"])
    return out


# -------------------------------------------------------------------- load --


def load_samples(path):
    """The during-leg load samples the runner wrote: `<epoch> <load1>` a line.

    A START-OF-LEG GATE IS NOT ENOUGH. Its error is one-sided -- load depresses
    only the cheaper-frame arm -- so it biases ratios rather than adding noise,
    and a compile that begins after the gate passes is invisible to it. This
    reads the whole window.
    """
    if not path or not os.path.isfile(path):
        return None
    vals = []
    with open(path, "r", encoding="utf-8") as fh:
        for line in fh:
            parts = line.split()
            if len(parts) >= 2:
                try:
                    vals.append(float(parts[1]))
                except ValueError:
                    pass
    if not vals:
        return None
    return {
        "start": vals[0],
        "end": vals[-1],
        "max": max(vals),
        "samples": len(vals),
    }


# A leg sampled at 5 s needs at least this many readings before its quiet stamp
# means anything. Two samples describe the ends and say nothing about the
# middle, which is the interval the stamp exists to cover.
MIN_LOAD_SAMPLES = 4


def quiet_verdict(load, quiet_max):
    """Was the box quiet for the WHOLE leg -- not just at its ends?

    **A quiet START is not a quiet LEG.** The documented protocol defect here
    is a leg whose start-of-leg gate passed, whose end looked fine, and whose
    middle carried a sibling lane's compile. Load's error is ONE-SIDED: it
    depresses only the cheaper-frame arm, so it biases a ratio rather than
    adding noise to it, and a ratio biased by a confound reads exactly like a
    result.

    So the verdict is taken on the MAXIMUM over the whole series, and a series
    too thin to have covered the middle is `unknown` rather than `yes` --
    refusing to stamp is the point; a fabricated stamp is worse than none.
    """
    if load is None:
        return {
            "quiet": "unknown",
            "loud_mid": False,
            "why": "no during-leg load samples: this leg has no record of the "
                   "interval it was measured over, and a start-of-leg gate "
                   "cannot see a compile that began after it passed",
        }
    if load["samples"] < MIN_LOAD_SAMPLES:
        return {
            "quiet": "unknown",
            "loud_mid": False,
            "why": "only %d load samples (need %d): the series describes the "
                   "ends of the leg and says nothing about its middle"
                   % (load["samples"], MIN_LOAD_SAMPLES),
        }
    quiet = load["max"] < quiet_max
    # Named separately because it is the case a start-of-leg gate is BLIND to,
    # and a row that merely says `quiet=no` does not tell its reader that the
    # endpoints agreed with each other and were both wrong.
    loud_mid = (
        not quiet and load["start"] < quiet_max and load["end"] < quiet_max
    )
    if quiet:
        why = "loadavg stayed under %.1f for all %d samples (max %.2f)" % (
            quiet_max, load["samples"], load["max"])
    elif loud_mid:
        why = (
            "loadavg reached %.2f MID-LEG against a ceiling of %.1f while both "
            "ends were quiet (start %.2f, end %.2f): a start-of-leg gate would "
            "have passed this leg" % (
                load["max"], quiet_max, load["start"], load["end"])
        )
    else:
        why = "loadavg reached %.2f against a ceiling of %.1f" % (
            load["max"], quiet_max)
    return {"quiet": "yes" if quiet else "no", "loud_mid": loud_mid, "why": why}


# -------------------------------------------------------------- divergence --


def divergence(count_a, count_b):
    """A run pair's divergence, on the UNBINNED interact frame count.

    **Never on binned percentiles.** `Hist` is four bins per octave, so a
    one-bin difference is 0-19% and any ratio in 1.68-2.38x prints as exactly
    2.00x. Adjudicating on percentiles failed in both directions: it over-fired
    on legs agreeing to 1.8% (41.4% apparent, two bins apart) and read 0.0% on
    legs whose real throughput differed 8.4%, because their p99s shared a bin.

    Relative to the mean of the pair, so the answer does not depend on which
    run is called the baseline.
    """
    if count_a is None or count_b is None:
        return {"ok": False, "why": "a run in the pair has no interact count"}
    mean = (count_a + count_b) / 2.0
    if mean <= 0:
        return {"ok": False, "why": "the pair served no interact frames"}
    rel = abs(count_a - count_b) / mean
    return {
        "ok": True,
        "basis": "unbinned interact frame count",
        "a": count_a,
        "b": count_b,
        "divergence": rel,
        "threshold": DIVERGENCE_THRESHOLD,
        "third_run_needed": rel > DIVERGENCE_THRESHOLD,
    }


# --------------------------------------------------------------- platform ---

# What a leg needs from the machine it runs on, and what supplies it.
#
# **A platform this runner does not know is a DEGRADATION WITH A NAME, never an
# exit.** The runner used to `exit 2` on macOS on a missing `xdotool`, so every
# lane that needed a macOS row wrote a private runner in a scratchpad that died
# with its session -- five of them, and no two lanes' rows guaranteed
# comparable. An unavailable capability costs the row the columns it feeds and
# says which ones; it does not cost the row.
#
# The keys are the four things the driver cannot do in pure shell. `geometry`
# is deliberately absent: the achieved surface is read from the app's OWN
# `Window resized to WxH` line, which exists on every platform, so the one
# reading the byte cross-check depends on never needs a platform tool.
NATIVE_CAPABILITIES = ("loadavg", "window", "refresh", "cputime")

# `capability -> (tool, what the row loses without it)`. A tool named here is
# probed by the runner with `command -v` (or a file test) and reported back.
PLATFORM_TOOLS = {
    "linux": {
        "loadavg": ("procfs", "the quiet stamp: the row cannot say the box "
                              "was quiet and is marked INVALID"),
        "window":  ("xdotool", "geometry PINNING: the leg runs at whatever "
                               "size the app opened, and is refused unless "
                               "that already matches"),
        "refresh": ("xrandr", "the hz~ column, which prints ?"),
        "cputime": ("ps", "the wedged-vs-working distinction on a silent log"),
    },
    "macos": {
        "loadavg": ("sysctl", "the quiet stamp: the row cannot say the box "
                              "was quiet and is marked INVALID"),
        "window":  ("osascript", "geometry PINNING: needs Accessibility "
                                 "permission for the process running this, "
                                 "and System Events resolves the window by "
                                 "`unix id`, never by title"),
        "refresh": ("system_profiler", "the hz~ column, which prints ?"),
        "cputime": ("ps", "the wedged-vs-working distinction on a silent log"),
    },
}


def platform_name(system, release=""):
    """`uname -s`/`uname -r` -> the name this file plans for."""
    s = (system or "").strip().lower()
    if s.startswith("darwin"):
        return "macos"
    if s.startswith("linux"):
        # WSL is a Linux kernel with no X server of its own worth assuming;
        # it is still `linux` here, and its missing `xdotool` degrades by the
        # ordinary path rather than needing a name of its own.
        return "linux"
    return "unknown"


def platform_plan(system, have, release="", session=""):
    """The capabilities a leg may use here, and a NAMED reason for each it may not.

    `have` is the set of tool names the runner found. Nothing in the result is
    fatal by itself: the analyser's own refusals -- the surface byte check, the
    liveness check, the quiet stamp -- are what decide whether a row is valid,
    and each of them says which reading it was missing. A runner that exits
    instead produces no row and no reason, which is how this ended up
    reimplemented five times.
    """
    name = platform_name(system, release)
    tools = PLATFORM_TOOLS.get(name)
    caps = {}
    if tools is None:
        for cap in NATIVE_CAPABILITIES:
            caps[cap] = {
                "ok": False, "tool": None,
                "why": "no plan for platform %r: this runner knows linux and "
                       "macos. The leg still runs and the app's own "
                       "`Window resized to` line still cross-checks its "
                       "surface; every platform-fed column is absent and the "
                       "row says so" % (system or "?"),
            }
    else:
        for cap in NATIVE_CAPABILITIES:
            tool, cost = tools[cap]
            ok = tool in have
            caps[cap] = {
                "ok": ok, "tool": tool,
                "why": None if ok else
                       "%s is not on this machine; without it the leg loses %s"
                       % (tool, cost),
            }
    # Wayland is called out by name rather than left to the xdotool probe: on
    # Wayland `xdotool` may be INSTALLED and still unable to move a native
    # window, so "the tool is present" is not "the capability works", and a
    # leg that silently ran unpinned is the exact failure this file exists to
    # refuse. The runner resolves it the same way either way -- the app's own
    # surface line against the picture bytes -- but the reason is named.
    if name == "linux" and (session or "").strip().lower() == "wayland":
        caps["window"] = {
            "ok": False, "tool": "xdotool",
            "why": "XDG_SESSION_TYPE is wayland: xdotool cannot resize a "
                   "native Wayland surface even when it is installed, so "
                   "geometry cannot be pinned. Run the leg under Xwayland or "
                   "an X session, or start the app at the target size",
        }
    return {
        "platform": name,
        "system": system,
        "session": session or None,
        "caps": caps,
        "degraded": sorted(c for c, v in caps.items() if not v["ok"]),
    }


# ------------------------------------------------------------------ order ---


def leg_order(arm_count, runs, counterbalance):
    """`[(arm_index, run_index)]` -- the order the legs actually run in.

    **Base-first in every pair is a confound, not an ordering.** A previous
    comparison ran the base arm first in every pair on a box whose load was
    decaying, so every pair was biased the SAME WAY, which is the one error
    repetition cannot average out.

    Counterbalanced order alternates direction between runs -- ABBA for two
    arms, and the same rule generalised for more -- so every arm's MEAN
    POSITION is equal whenever `runs` is even. That equality is the property,
    and it is what the test asserts; ABBA is only what it looks like at n=2.
    """
    order = []
    for r in range(1, runs + 1):
        idx = range(arm_count)
        if counterbalance and r % 2 == 0:
            idx = reversed(range(arm_count))
        for i in idx:
            order.append((i, r))
    return order


def mean_positions(order, arm_count):
    """Each arm's mean 1-based position in `order` -- the counterbalance test."""
    sums = [0.0] * arm_count
    counts = [0] * arm_count
    for pos, (arm, _run) in enumerate(order, start=1):
        sums[arm] += pos
        counts[arm] += 1
    return [(sums[i] / counts[i]) if counts[i] else None for i in range(arm_count)]


# --------------------------------------------------------------- gate set ---

# The default gate set a measurement or a landing is expected to clear.
#
# In the tree because a protocol that lives only in prose gets forgotten
# exactly the way the native protocol itself was. Every entry names the
# package that OWNS the suite and a path that must exist, so a renamed or
# moved suite reddens `the_default_gate_set_still_exists` rather than being
# skipped in silence.
#
# `doc_citations_resolve` is the one that keeps catching people out: it SCANS
# THE WHOLE WORKSPACE but LIVES IN `squallar-radar`, so a lane editing a doc
# comment in any crate can redden it without ever having reason to run it.
# Naming packages individually is what let a landing through that reddened it.
DEFAULT_GATE_SET = (
    {
        "cmd": "cargo test -p squallar-app --lib",
        "proves": "the app crate's unit suites, including the loop pin-list",
        "path": "squallar-app/src/lib.rs",
    },
    {
        "cmd": "cargo test -p squallar-app --test arch_ratchets",
        "proves": "the coupling ceilings; they may only fall",
        "path": "squallar-app/tests/arch_ratchets.rs",
        "note": "the `--test` spelling is load-bearing: "
                "`cargo test -p squallar-app arch_ratchets` selects ZERO tests",
    },
    {
        "cmd": "cargo test -p squallar-radar --test doc_citations_resolve",
        "proves": "every doc-comment citation in the WORKSPACE resolves",
        "path": "squallar-radar/tests/doc_citations_resolve.rs",
        "note": "SCANS THE WHOLE WORKSPACE, LIVES IN squallar-radar. Any lane "
                "editing a comment anywhere can redden this without ever "
                "running it. It is in this set for exactly that reason",
    },
    {
        "cmd": "cargo test -p squallar-radar",
        "proves": "the radar digest suites, which pass UNEDITED",
        "path": "squallar-radar/src/lib.rs",
        "note": "a moved digest is a bug in the encoder, not a pin to re-record",
    },
)


def gate_set_report(repo_root):
    """The gate set, with each entry's owning path resolved against the tree."""
    rows = []
    for g in DEFAULT_GATE_SET:
        full = os.path.join(repo_root, g["path"])
        rows.append(dict(g, exists=os.path.exists(full)))
    return rows


# ------------------------------------------------------------------- row ----


def build_row(args, scraped, probes):
    """Everything the ROW line needs, as a dict, plus its invalidity reasons."""
    invalid = []
    notes = []

    skip = args.skip_loops
    win = args.window_loops
    # `loops`/`settled` are the shared columns the web row grew when window
    # length became a printed denominator. Native answers them differently and
    # better: `bracket()` cuts EXACTLY `window_loops` whole loops after
    # discarding `skip_loops`, so a bracketed native row is length-equalised
    # and boot-excluded by construction -- it is always the equivalent of the
    # web row's `settled` window, never of its boot-inclusive default. The two
    # unbracketed paths below are the exceptions, and they say so rather than
    # printing a loop count they do not have.
    row_loops, row_settled = win, "%d-loop" % win
    br, why = bracket(scraped["loops"], skip, win)
    if br is None:
        # A gestureless leg (scene E1) has no markers by design and takes the
        # whole log as its bracket, saying so on the row.
        if args.script == "none":
            first = scraped["interact"][0].idx if scraped["interact"] else 0
            last = scraped["interact"][-1].idx if scraped["interact"] else 0
            br = (first, last)
            basis = "unbracketed (E-scene, no gesture): whole log"
            row_loops, row_settled = 0, "NO(E-scene: whole log)"
        else:
            invalid.append("no whole-loop bracket: %s" % why)
            br = (0, len(scraped["interact"]) and scraped["interact"][-1].idx or 0)
            basis = "UNBRACKETED FALLBACK -- not a window figure"
            row_loops = len(scraped["loops"])
            row_settled = "NO(unbracketed fallback)"
    else:
        basis = "%d whole loops, %d skipped" % (win, skip)
    start_idx, end_idx = br

    windows = {}
    for family in ("interact", "idle", "cadence"):
        windows[family] = diff_window(scraped[family], start_idx, end_idx)

    rasters = diff_totals(scraped["rasters"], start_idx, end_idx)
    pictures = rasters[2] if rasters else 0
    picture_bytes = rasters[3] if rasters else 0
    mbpp = ("%.2f" % (picture_bytes / pictures / 1e6)) if pictures else "-"

    live = liveness(scraped["interact"], end_idx)
    if not live["ok"]:
        invalid.append("liveness: %s" % live["verdict"])

    # The achieved surface is the APP's own reading where there is one: it is
    # the surface the geometry check is taken against, and it is the only
    # geometry readback that exists on every platform. The window manager's answer --
    # which the runner supplies as `--achieved-geom` and which Wayland cannot
    # supply at all -- is kept as a second opinion and reported when the two
    # disagree, because a disagreement is a real finding (a scale factor, or a
    # frame counted with its decorations) and not a reason to prefer either.
    wm_geom = args.achieved_geom
    app_geom = scraped["surface"]
    achieved = app_geom or wm_geom
    # The pictures the bracket's bytes are held against are the ones the APP
    # reports allocating, per pane, in its `overlay pictures:` line: the level
    # in force where the bracket closes (the last said at or before its end),
    # or, on a log too short to have said it by then, the last said at all. A
    # log that never says it is UNCHECKED, not INVALID -- the check did not
    # run. The scene's own pane count is read out of the seed that laid the
    # panes out (`--panes` for a leg run against a seed the scene table does
    # not know) and held against the app's; a seed nothing can read leaves
    # that one comparison out and says so -- it no longer refuses the row,
    # because the app's count is in the line.
    reported_at = at_or_before(scraped["overlay_pictures"], end_idx)
    if reported_at is None and scraped["overlay_pictures"]:
        reported_at = scraped["overlay_pictures"][-1]
    reported = reported_at[1] if reported_at else None
    panes, panes_source = resolve_pane_count(args)
    if panes is None:
        notes.append("the seeded pane count was not held against the app's: %s"
                     % panes_source)
    surf = surface_check(args.asked_geom, achieved, pictures, picture_bytes, reported, panes)
    surf["panes_source"] = panes_source if panes is not None else None
    surf["reported_line"] = reported_at[0] if reported_at else None
    surf["source"] = "app" if app_geom else ("wm" if wm_geom else None)
    surf["wm_reported"] = ("%dx%d" % wm_geom) if wm_geom else None
    surf["app_reported"] = ("%dx%d" % app_geom) if app_geom else None
    if app_geom and wm_geom and app_geom != wm_geom:
        notes.append(
            "the window manager reported %dx%d and the app allocated %dx%d. "
            "The app's figure is the one the picture bytes are checked "
            "against; the gap is a scale factor or a decorated frame, and "
            "either way the two targets' rows are only comparable on the "
            "app's" % (wm_geom + app_geom)
        )
    # UNCHECKED is its own list, never an entry in `invalid`: a row whose
    # bytes could not be checked is not a row that failed the check, and the
    # two stamps are kept apart so a grep for either finds only its own.
    unchecked = []
    if surf["unchecked"]:
        unchecked.append("surface bytes not checked: %s" % surf["why"])
        if not surf["geometry_met"]:
            invalid.append(
                "surface not confirmed: the app ran at %s, not the %s the leg "
                "asked for" % (surf["achieved"], surf["asked"])
            )
    elif not surf["met"]:
        invalid.append("surface not confirmed: %s" % surf["why"])

    load = load_samples(args.load_file)
    quiet_max = args.quiet_max
    qv = quiet_verdict(load, quiet_max)
    quiet = qv["quiet"]
    if quiet != "yes":
        invalid.append("not quiet: %s" % qv["why"])

    # Basemap state, on `run_measure.sh`'s own two-counter terms.
    bt = diff_totals(scraped["basemap"], start_idx, end_idx)
    g = diff_totals(scraped["ground"], start_idx, end_idx)
    decoded = None if bt is None else (bt[0] + bt[1])
    placed = None if g is None else (g[1] or g[2] or g[3] or g[4])
    if decoded is None and placed is None:
        basemap = "unknown(no basemap or ground line)"
    else:
        basemap = "%s-decoded/%s-placed" % (
            "some" if decoded else ("none" if decoded == 0 else "?"),
            "some" if placed else ("none" if placed == 0 else "?"),
        )

    iw = windows["interact"]
    throughput = iw.get("n")
    # On a leg whose percentiles clamp at the top bin, the count is the only
    # honest throughput figure -- a ceiling is not a measurement.
    clamped = iw.get("p99_us") == "over"
    if clamped:
        notes.append(
            "p99 CLAMPED at the over-64ms bin: quote `interact n=%s` as this "
            "leg's throughput figure, not the percentiles" % throughput
        )

    aw, ah = achieved if achieved else (0, 0)
    return {
        "scene": args.scene,
        # The `browser=` column carries `native`. It is the TARGET column in
        # the shared table -- drive.py puts firefox/chromium there -- and a
        # differently-named column would split the one table this row format
        # exists to keep.
        "browser": "native",
        "arm": "native",
        # The app's own reading, with the caller's only as a fallback.
        "adapter": scraped["adapter"] or args.adapter,
        "backend": scraped["backend"] or "UNKNOWN",
        "viewport": "%dx%d" % (aw, ah),
        "px": aw * ah,
        "dpr": args.dpr,
        "cross": "yes" if surf["met"] else ("unchecked" if surf["unchecked"] else "no"),
        "hz": args.refresh or "?",
        "coi": "n/a",
        "panel": args.panel,
        "script": args.script,
        "basemap": basemap,
        "pictures": pictures,
        "mb_per_picture": mbpp,
        "commit": args.commit,
        "position": args.position,
        "load": load,
        "quiet": quiet,
        "quiet_verdict": qv,
        "quiet_max": quiet_max,
        # The platform the leg ran on and every capability it had to do
        # without. A row measured with geometry unpinned is not the same
        # measurement as one measured with it pinned, and the matrix has to be
        # able to see which it is holding.
        "platform": args.platform,
        "degraded": [d for d in (args.degraded or "").split(",") if d],
        "windows": windows,
        "window_basis": basis,
        "loops": row_loops,
        "settled": row_settled,
        "bracket": {"start_line": start_idx, "end_line": end_idx},
        "liveness": live,
        "surface": surf,
        "loop_state": (scraped["loop_state"][-1][1] if scraped["loop_state"] else None),
        # `(line, bracket, [fourteen ints])`, or None when the log has no
        # `budget state:` line -- a binary older than the line, kept apart
        # from a live binary reporting zeroes.
        "budget_state": (scraped["budget_state"][-1] if scraped["budget_state"] else None),
        # `{role: (line, [thirteen ints])}` for the LAST reading of each cache
        # role, or None when the log has no `tile cache (...)` line -- a binary
        # older than the line, kept apart from a cache that recorded nothing.
        "tile_cache": tile_cache_by_role(scraped["tile_cache"]),
        "gpu_unavailable": scraped["gpu_unavailable"],
        "throughput_interact_frames": throughput,
        "percentiles_clamped": clamped,
        "invalid": invalid,
        "unchecked": unchecked,
        "notes": notes,
    }



def commit_for_arm(label, arm_commits, global_commit, arm_count, head):
    """Which commit an arm's binary was built from, or an honest `unknown`.

    **A multi-arm row used to stamp every arm with the tree's HEAD**, which on a
    two-arm comparison is a tree NEITHER binary was built from. Two lanes hit it
    independently on 2026-09-02 and both worked around it identically, by
    passing `--commit <a>+<b>` -- one field made to carry two values by string
    concatenation, which is the defect confessing rather than a workaround. The
    row that resulted could not say which arm was which tree, so its provenance
    was unreconstructable afterwards. Not a wrong number; an unrecordable one.

    First hit wins:

    1. `arm_commits`, a list of `LABEL=SHA`, matched on `label`;
    2. `global_commit`, the whole-run `--commit`, still accepted;
    3. `head` -- **only when there is one arm**, where the binary is presumed to
       be this tree's build, which is what the single-arm default always meant;
    4. `"unknown"`.

    Step 3 is deliberately withheld from multi-arm runs. `unknown` is honest and
    a reader can go and find out; a plausible-looking wrong commit is worse than
    an admitted absent one, because nothing downstream can tell it is wrong.

    An empty `head` (no `.git`, a shipped bundle) also lands on `unknown`, which
    is the behaviour `squallar-app`'s native seed pins already describe.
    """
    for spec in arm_commits or ():
        name, sep, sha = spec.partition("=")
        if sep and name == label:
            return sha or "unknown"
    if global_commit:
        return global_commit
    if arm_count <= 1 and head:
        return head
    return "unknown"


def tile_cache_by_role(readings):
    """The last `tile cache (<role>)` reading per role, or None for none.

    Running totals, so the last line per role is the whole answer for that
    role; the roles are kept apart because the basemap's and the hillshade's
    caches hold different things at different prices.
    """
    if not readings:
        return None
    out = {}
    for idx, role, figures in readings:
        out[role] = (idx, figures)
    return out


def print_row(row):
    """The ROW line. Shared columns first, in `run_measure.sh`'s order and
    spelling, then the native-only ones. A native row and a web row are meant
    to be read in one table; a column that drifted would silently become two
    tables."""
    load = row["load"] or {}
    print(
        "ROW scene=%s browser=%s arm=%s adapter=%s backend=%s "
        "viewport=%s px=%s dpr=%s cross=%s hz~%s coi=%s panel=%s "
        "script=%s basemap=%s pictures=%s MB/picture=%s loops=%s settled=%s "
        "commit=%s "
        "loadavg_start=%s loadavg_end=%s loadavg_max=%s quiet=%s position=%s%s"
        % (
            row["scene"], row["browser"], row["arm"], row["adapter"],
            row["backend"], row["viewport"], row["px"], row["dpr"],
            row["cross"], row["hz"], row["coi"], row["panel"], row["script"],
            row["basemap"], row["pictures"], row["mb_per_picture"],
            row.get("loops"), row.get("settled"),
            row["commit"],
            load.get("start", "?"), load.get("end", "?"), load.get("max", "?"),
            row["quiet"], row["position"],
            ("  ** INVALID **" if row["invalid"] else "")
            + ("  ** UNCHECKED: overlay pictures line absent **"
               if row.get("unchecked") else ""),
        )
    )
    s = row["surface"]
    reported_panes = s.get("reported_panes", s.get("panes"))
    print(
        "ROW   surface asked=%s achieved=%s (from the %s) panes=%s "
        "app_pictures=%s expected=%s B/picture (+-%s) observed=%s B/picture -> %s"
        % (
            s.get("asked"), s.get("achieved"), s.get("source") or "?",
            reported_panes if reported_panes is not None else "?",
            s.get("reported_px") or "absent",
            s.get("expected_label", "-"),
            s.get("tolerance_bytes", "-"),
            ("%.0f" % s["observed_picture_bytes"])
            if s.get("observed_picture_bytes") is not None else "-",
            "UNCHECKED" if s.get("unchecked")
            else ("CONFIRMED" if s.get("met") else "REFUSED"),
        )
    )
    print(
        "ROW   platform %s; capabilities unavailable: %s"
        % (row.get("platform") or "?",
           ", ".join(row.get("degraded") or []) or "none")
    )
    print("ROW   quiet: %s" % row["quiet_verdict"]["why"])
    print("ROW   liveness: %s" % row["liveness"]["verdict"])
    for family in ("interact", "idle", "cadence"):
        w = row["windows"].get(family) or {}
        if w.get("error"):
            print("ROW   window %-8s ERROR: %s" % (family, w["error"]))
            continue
        note = " [settle burst]" if family == "idle" else ""
        print(
            "ROW   window %-8s n=%-6s p50=%s us p90=%s us p99=%s us max=%s us "
            "[%s; percentiles BINNED, 4 bins/octave -- shape, not throughput]%s"
            % (
                family, w.get("n"), w.get("p50_us"), w.get("p90_us"),
                w.get("p99_us"), w.get("max_us"), row["window_basis"], note,
            )
        )
    print(
        "ROW   throughput: interact n=%s over the bracket [UNBINNED -- this is "
        "the figure a run pair's divergence is adjudicated on]"
        % row["throughput_interact_frames"]
    )
    # The loop denominators, on the scenes whose question they are. A..D run
    # with loops OFF, so their `loop state` line is all zeroes and printing it
    # beside them would be a column that means nothing there.
    if row["scene"].startswith("E") and row["loop_state"]:
        ls = row["loop_state"]
        print(
            "ROW   loop %s panes, %s layers animating, %s frames listed, "
            "%s resident (%s in flight, %s failed); cap=%s held=%s; advance=%s us"
            % (ls[0], ls[1], ls[2], ls[3], ls[4], ls[5], ls[10], ls[11], ls[16])
        )
    # The machine and the bracket the budgets came from, on every scene. A
    # LEVEL at the end of the log; `pool` is the LIVE loop pool in MiB and
    # `ceiling` the bracket's constant; `cap` is the capacity in force and
    # `source` how it was learned (0 presumed, 1 measured, 2 probed); `probe`
    # is where the browser's WebGPU probe stands (0 absent -- every native
    # log, 1 skipped, 2 pending, 3 empty, 4 found, 5 found capped). Absent
    # when the log has no `budget state:` line: a binary older than the line,
    # printed as such and never as zeroes. `.get` because a row built before
    # the field existed reads the same way.
    bs = row.get("budget_state")
    if bs:
        _line, bracket_name, f = bs
        print(
            "ROW   budget bracket=%s rung=%s steps=%s pool=%s MiB ceiling=%s MiB "
            "vram=%s MiB ram=%s MiB declared=%s MiB threads=%s form=%s "
            "linear=%s/%s MiB cap=%s MiB source=%s probe=%s"
            % (bracket_name, f[0], f[1], f[2], f[3], f[4], f[5], f[6], f[7],
               f[8], f[9], f[10], f[11], f[12], f[13])
        )
    else:
        print(
            "ROW   budget: n/a (no `budget state:` line in this log -- a binary "
            "older than the line, not a zero reading)"
        )
    # Events AT the tile cache, per role: what `ground tiles:`' GPU-store
    # uploads and evictions cannot classify. `refetch` is a subset of `asks`;
    # the four put kinds are disjoint; entries/resident/parsed are LEVELS.
    # Never subtracted from uploads. Absent when the log has no line: a binary
    # older than the line, printed as such and never as zeroes.
    tc = row.get("tile_cache")
    if tc:
        for role in sorted(tc):
            _line, f = tc[role]
            print(
                "ROW   tile cache (%s): asks=%s restyle_asks=%s refetch_after_eviction=%s "
                "puts_first=%s puts_restyle=%s puts_duplicate=%s puts_orphan=%s "
                "evicted_pending=%s evicted_resident=%s evicted_bytes=%s "
                "entries=%s resident_bytes=%s parsed=%s"
                % (role, f[0], f[1], f[2], f[3], f[4], f[5], f[6], f[7], f[8],
                   f[9], f[10], f[11], f[12])
            )
    else:
        print(
            "ROW   tile cache: n/a (no `tile cache (...)` line in this log -- a "
            "binary older than the line, not a zero reading)"
        )
    if row["gpu_unavailable"]:
        print("ROW   gpu passes: unavailable (adapter lacks TIMESTAMP_QUERY)")
    for n in row["notes"]:
        print("ROW   NOTE: %s" % n)
    for why in row.get("unchecked") or []:
        print("ROW   UNCHECKED: %s" % why)
    for why in row["invalid"]:
        print("ROW   INVALID: %s" % why)


# ------------------------------------------------------------------- main ---


def geom(text):
    m = re.match(r"^(\d+)x(\d+)$", text or "")
    if not m:
        raise argparse.ArgumentTypeError("expected WxH, got %r" % text)
    return (int(m.group(1)), int(m.group(2)))


def cmd_analyze(args):
    probes = compile_probes()
    with open(args.log, "r", encoding="utf-8", errors="replace") as fh:
        lines = fh.read().splitlines()
    scraped = scrape(lines, probes)
    row = build_row(args, scraped, probes)
    print_row(row)
    if args.json:
        with open(args.json, "w", encoding="utf-8") as fh:
            json.dump(row, fh, indent=1, sort_keys=True)
    return 1 if row["invalid"] else 0


def cmd_diverge(args):
    rows = []
    for p in args.rows:
        with open(p, "r", encoding="utf-8") as fh:
            rows.append(json.load(fh))
    counts = [r.get("throughput_interact_frames") for r in rows]
    d = divergence(counts[0], counts[1])
    if not d["ok"]:
        print("DIVERGENCE undecidable: %s" % d["why"])
        return 1
    print(
        "DIVERGENCE %.1f%% on the %s (a=%s b=%s); threshold %.0f%% -> %s"
        % (
            d["divergence"] * 100.0, d["basis"], d["a"], d["b"],
            d["threshold"] * 100.0,
            "RUN A THIRD" if d["third_run_needed"] else "the pair agrees",
        )
    )
    for r, p in zip(rows, args.rows):
        w = (r.get("windows") or {}).get("interact") or {}
        print(
            "  %s: interact n=%s p99=%s us [binned] quiet=%s loadavg_max=%s"
            % (p, w.get("n"), w.get("p99_us"), r.get("quiet"),
               (r.get("load") or {}).get("max"))
        )
    return 0


def cmd_plan(args):
    """The platform plan, as `KEY=value` lines the runner can `eval`.

    Values are single-quoted with embedded quotes escaped, so a reason that
    contains an apostrophe cannot become a shell injection or a syntax error
    in the runner that evaluates it.
    """
    have = {t for t in args.have.split(",") if t}
    plan = platform_plan(args.system, have, args.release, args.session)

    def sh(value):
        return "'%s'" % str(value).replace("'", "'\\''")

    print("PLAT_NAME=%s" % sh(plan["platform"]))
    print("PLAT_DEGRADED=%s" % sh(",".join(plan["degraded"])))
    for cap in NATIVE_CAPABILITIES:
        c = plan["caps"][cap]
        up = cap.upper()
        print("PLAT_%s=%s" % (up, sh(c["tool"] if c["ok"] else "")))
        print("PLAT_WHY_%s=%s" % (up, sh(c["why"] or "")))
    return 0


def cmd_order(args):
    for arm, run in leg_order(args.arms, args.runs, args.counterbalance):
        print("%d\t%d" % (arm, run))
    return 0


def cmd_cputime(args):
    v = cpu_liveness(args.before, args.after, args.wall)
    print(v["verdict"])
    # Unreadable is not "wedged". Exit 2 keeps the caller from reading a
    # missing instrument as a finding -- the mistake this check exists to stop.
    if not v["readable"]:
        return 2
    return 0 if v["busy"] else 1


def cmd_gates(args):
    rows = gate_set_report(args.repo_root)
    print("the default gate set a measurement or a landing is expected to clear:")
    for g in rows:
        print("  %s %s" % ("OK  " if g["exists"] else "MISSING", g["cmd"]))
        print("       proves: %s" % g["proves"])
        if g.get("note"):
            print("       note:   %s" % g["note"])
    missing = [g for g in rows if not g["exists"]]
    if missing:
        print(
            "\n%d gate(s) name a path that is not in the tree; the suite moved "
            "and this set is stale." % len(missing)
        )
        return 1
    return 0


def main(argv):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = ap.add_subparsers(dest="cmd")

    a = sub.add_parser("analyze", help="turn one leg's log into a ROW")
    a.add_argument("--log", required=True)
    a.add_argument("--scene", required=True)
    a.add_argument("--script", default="-")
    a.add_argument("--commit", default="unknown")
    a.add_argument("--asked-geom", type=geom, dest="asked_geom")
    a.add_argument("--achieved-geom", type=geom, dest="achieved_geom")
    a.add_argument("--panes", type=int, default=None,
                   help="panes the scene lays out; default: read from the "
                        "scene's own seed")
    a.add_argument("--dpr", default="1")
    a.add_argument("--refresh", default="")
    a.add_argument("--adapter", default="unknown")
    a.add_argument("--panel", default="off")
    a.add_argument("--position", default="-")
    a.add_argument("--load-file", dest="load_file", default="")
    a.add_argument("--quiet-max", dest="quiet_max", type=float,
                   default=DEFAULT_QUIET_MAX)
    a.add_argument("--skip-loops", dest="skip_loops", type=int, default=2)
    a.add_argument("--window-loops", dest="window_loops", type=int, default=2)
    a.add_argument("--platform", default="unknown")
    a.add_argument("--degraded", default="",
                   help="comma-separated capabilities this leg ran without")
    a.add_argument("--json", default="")
    a.set_defaults(func=cmd_analyze)

    d = sub.add_parser("diverge", help="adjudicate a run pair")
    d.add_argument("rows", nargs=2)
    d.set_defaults(func=cmd_diverge)

    p = sub.add_parser("plan", help="the platform plan, as shell assignments")
    p.add_argument("--system", required=True, help="`uname -s`")
    p.add_argument("--release", default="", help="`uname -r`")
    p.add_argument("--session", default="", help="XDG_SESSION_TYPE, if any")
    p.add_argument("--have", default="",
                   help="comma-separated tool names found on this machine")
    p.set_defaults(func=cmd_plan)

    o = sub.add_parser("order", help="the leg order, one `armindex<TAB>run` a line")
    o.add_argument("--arms", type=int, required=True)
    o.add_argument("--runs", type=int, required=True)
    o.add_argument("--counterbalance", action="store_true")
    o.set_defaults(func=cmd_order)

    c = sub.add_parser("cputime", help="a silent process: wedged, or working?")
    c.add_argument("--before", required=True, help="`ps -o time=` before")
    c.add_argument("--after", required=True, help="`ps -o time=` after")
    c.add_argument("--wall", type=float, required=True, help="seconds between")
    c.set_defaults(func=cmd_cputime)

    sc = sub.add_parser("scene", help="run_measure.sh's own seed/script for a scene")
    sc.add_argument("--scene", required=True)
    sc.add_argument("--panel", default="off")
    sc.add_argument("--what", choices=("seed", "script"), default="seed")
    sc.set_defaults(func=cmd_scene)

    sd = sub.add_parser("seed", help="seed a redirected config dir (JSON on stdin)")
    sd.add_argument("--config-dir", dest="config_dir", required=True)
    sd.set_defaults(func=cmd_seed)

    g = sub.add_parser("gates", help="the default gate set, resolved")
    g.add_argument("--repo-root", dest="repo_root",
                   default=os.path.abspath(os.path.join(RIG_DIR, "..", "..")))
    g.set_defaults(func=cmd_gates)

    c = sub.add_parser("commit-for-arm",
                       help="which commit an arm's binary was built from")
    c.add_argument("--label", required=True)
    c.add_argument("--arm-commit", action="append", default=[],
                   help="LABEL=SHA, repeatable")
    c.add_argument("--commit", default="")
    c.add_argument("--arms", type=int, default=1)
    c.add_argument("--head", default="")
    c.set_defaults(func=lambda a: print(
        commit_for_arm(a.label, a.arm_commit, a.commit, a.arms, a.head), end=""))

    s = sub.add_parser("selftest", help="this file's own tests")
    s.set_defaults(func=lambda _a: run_selftest())

    args = ap.parse_args(argv)
    if not getattr(args, "func", None):
        ap.print_help()
        return 2
    return args.func(args)


def run_selftest():
    loader = unittest.TestLoader()
    suite = loader.loadTestsFromModule(sys.modules[__name__])
    res = unittest.TextTestRunner(verbosity=2).run(suite)
    return 0 if res.wasSuccessful() else 1


# ------------------------------------------------------------------ tests ---


class HistogramTests(unittest.TestCase):
    def test_edges_double_every_four_bins(self):
        """Four bins per octave -- the fact the divergence rule exists for."""
        for i in range(0, GEOMETRIC_BINS - 4 + 1):
            self.assertEqual(edge_ns(i + 4), edge_ns(i) * 2)

    def test_ceiling_edge_is_64ms(self):
        self.assertEqual(edge_ns(GEOMETRIC_BINS), 64_000_000)

    def test_percentile_matches_the_rust_contract(self):
        # Hand-derived from the documented edge formula, not read back from
        # the implementation. Slot 0 is the UNDER-FLOOR clamp, whose upper
        # bound is edge 0 = 62 500 ns -> 63 us. Slot 1 is geometric bin 0,
        # covering [edge(0), edge(1)) = [62 500, 74 325) ns, so its upper edge
        # is 74 325 ns -> 75 us. Getting this pair the wrong way round is the
        # off-by-one that would shift every percentile this file prints.
        counts = [0] * SLOTS
        counts[0] = 10
        self.assertEqual(percentile_upper_micros(counts, 0.5), 63)
        counts = [0] * SLOTS
        counts[1] = 10
        self.assertEqual(percentile_upper_micros(counts, 0.5), 75)
        counts = [0] * SLOTS
        counts[SLOTS - 1] = 3  # the over-ceiling clamp has no upper edge
        self.assertEqual(percentile_upper_micros(counts, 0.99), "over")
        self.assertIsNone(percentile_upper_micros([0] * SLOTS, 0.5))


class DivergenceTests(unittest.TestCase):
    """The rule, and both directions the binned one failed in.

    Every test here REDDENS an implementation that adjudicates on binned
    percentiles. That is the bug being institutionalised against, so it is
    asserted directly rather than left to a comment.
    """

    def _hist_in_bin(self, slot, n):
        h = [0] * SLOTS
        h[slot] = n
        return h

    def test_the_basis_is_the_unbinned_count(self):
        d = divergence(1000, 1100)
        self.assertEqual(d["basis"], "unbinned interact frame count")
        self.assertAlmostEqual(d["divergence"], 100 / 1050.0)

    def test_does_not_over_fire_on_legs_two_bins_apart(self):
        """The over-fire direction: counts agree to 1.8%, p99s are two bins
        apart (41.4% apparent). The pair AGREES; a binned rule would demand a
        third run and did, repeatedly."""
        a_n, b_n = 1000, 1018  # 1.8% apart
        a_h = self._hist_in_bin(10, a_n)
        b_h = self._hist_in_bin(12, b_n)  # two bins up == 2^(2/4) == 41.4%
        a_p99 = percentile_upper_micros(a_h, 0.99)
        b_p99 = percentile_upper_micros(b_h, 0.99)
        self.assertAlmostEqual(b_p99 / float(a_p99), 2 ** 0.5, places=2)
        d = divergence(a_n, b_n)
        self.assertLess(d["divergence"], 0.02)
        self.assertFalse(
            d["third_run_needed"],
            "adjudicated on binned percentiles: a 1.8% agreement was called a "
            "41.4% divergence",
        )

    def test_is_not_reassured_by_legs_sharing_a_bin(self):
        """The false-reassurance direction, and the worse one: two legs whose
        p99s land in the SAME bin -- 0.0% apparent -- whose real throughput is
        22% apart. A binned rule reports agreement and the pair is quoted."""
        a_n, b_n = 1000, 1247  # 22.0% apart on the mean
        a_h = self._hist_in_bin(15, a_n)
        b_h = self._hist_in_bin(15, b_n)
        self.assertEqual(
            percentile_upper_micros(a_h, 0.99),
            percentile_upper_micros(b_h, 0.99),
            "the fixture is wrong: these must share a bin for the test to bite",
        )
        d = divergence(a_n, b_n)
        self.assertGreater(d["divergence"], DIVERGENCE_THRESHOLD)
        self.assertTrue(
            d["third_run_needed"],
            "adjudicated on binned percentiles: two legs 22% apart shared a "
            "bin and were called identical",
        )

    def test_reports_the_real_gap_when_a_bin_hides_it(self):
        """The 8.4% case: below the threshold either way, so the VERDICT does
        not discriminate -- but the reported FIGURE must still be 8.4% and not
        the 0.0% a shared bin shows."""
        a_n, b_n = 1000, 1088
        self.assertAlmostEqual(divergence(a_n, b_n)["divergence"], 0.0841, places=3)

    def test_is_symmetric(self):
        self.assertAlmostEqual(
            divergence(900, 1100)["divergence"],
            divergence(1100, 900)["divergence"],
        )


# The app's `overlay pictures:` line, as a one-pane leg at 1920x1080 said it on
# 2026-09-02 -- 2880x1555 is the picture that leg uploaded, five rows short of
# the deleted model's 2880x1560 -- and as a six-pane leg at the same window is
# expected to: six 960x777 pictures. The `bytes` are the sums, computed here,
# and the sizes are what the check READS; nothing here prices a pane.
ONE_PANE_PICTURE = (2880, 1555)
ONE_PANE_PICTURE_BYTES = 2880 * 1555 * 4  # 17,913,600
SIX_PANE_PICTURE = (960, 777)
SIX_PANE_PICTURE_BYTES = 960 * 777 * 4  # 2,983,680
OVERLAY_PICTURES_ONE = (
    "[2026-09-02T00:00:00Z INFO  squallar_app::app::render] overlay pictures: "
    "n=1, px=2880x1555, bytes=17913600"
)
OVERLAY_PICTURES_SIX = (
    "[2026-09-02T00:00:00Z INFO  squallar_app::app::render] overlay pictures: "
    "n=6, px=960x777;960x777;960x777;960x777;960x777;960x777, bytes=17902080"
)
# What the model said a 1920x1080 window's pictures were: one 2880x1560, or
# six 960x780. Kept ONLY as the figures a leg must not be confirmed against.
MODEL_ONE_PANE_BYTES = 17_971_200
MODEL_SIX_PANE_BYTES = 2_995_200
# The two literals the app's own pin (`squallar_app::budget_telemetry`) holds
# its formatter to, VERBATIM: three panes with the middle one pictureless, and
# a scene with no panes at all. A drift in either file reddens both boards.
OVERLAY_PICTURES_PEER_PIN = "overlay pictures: n=3, px=2880x1555;0x0;1440x780, bytes=22406400"
OVERLAY_PICTURES_NONE = "overlay pictures: n=0, px=, bytes=0"


def _reported(px):
    """A parsed `overlay pictures:` reading for these pane pictures."""
    return {"n": len(px), "px": list(px), "bytes": sum(w * h * 4 for w, h in px)}


class SurfaceTests(unittest.TestCase):
    def test_the_line_scrapes_into_its_own_arm_with_every_group_mandatory(self):
        probes = compile_probes()
        s = scrape([OVERLAY_PICTURES_ONE, OVERLAY_PICTURES_SIX], probes)
        self.assertEqual(len(s["overlay_pictures"]), 2)
        _idx, one = s["overlay_pictures"][0]
        self.assertEqual(one, {"n": 1, "px": [(2880, 1555)], "bytes": 17_913_600})
        _idx, six = s["overlay_pictures"][1]
        self.assertEqual(six["n"], 6)
        self.assertEqual(six["px"], [(960, 777)] * 6)
        self.assertEqual(six["bytes"], 6 * SIX_PANE_PICTURE_BYTES)
        self.assertEqual(six["bytes"], 17_902_080, "the fixture's sum is what it says")
        for truncated in (
            OVERLAY_PICTURES_SIX.rsplit(", bytes", 1)[0],
            OVERLAY_PICTURES_SIX.replace(" px=960x777;", " px=;"),
            OVERLAY_PICTURES_SIX.replace("n=6, ", ""),
        ):
            self.assertEqual(
                scrape([truncated], probes)["overlay_pictures"], [],
                "a line missing a group matched: every group is mandatory\n%s"
                % truncated)

    def test_the_apps_own_pinned_line_parses_pane_for_pane(self):
        """The literal `squallar_app::budget_telemetry` pins its formatter to,
        verbatim: three panes, the middle one without a picture. `0x0` is
        kept at index 1 -- position IS the pane index -- and the app's
        `bytes` is exactly what the check's self-consistency clause recomputes
        over the list, the empty pane costing nothing."""
        probes = compile_probes()
        s = scrape([OVERLAY_PICTURES_PEER_PIN], probes)
        self.assertEqual(len(s["overlay_pictures"]), 1)
        _idx, r = s["overlay_pictures"][0]
        self.assertEqual(r["n"], 3)
        self.assertEqual(r["px"], [(2880, 1555), (0, 0), (1440, 780)])
        self.assertEqual(r["bytes"], 22_406_400)
        self.assertEqual(2880 * 1555 * 4 + 1440 * 780 * 4, 22_406_400)
        lo, hi = 1440 * 780 * 4, 2880 * 1555 * 4
        s = surface_check((1920, 1080), (1920, 1080), 10, (lo + hi) * 5, r, panes=3)
        self.assertNotIn("contradicts", s.get("why", ""))
        self.assertTrue(s["met"], s.get("why"))
        self.assertEqual(s["pane_picture_bytes"], [hi, lo], "the 0x0 pane is not priced")
        self.assertEqual(s["expected_label"], "%d..%d" % (lo, hi))

    def test_a_scene_with_no_panes_is_a_reading_not_an_absent_line(self):
        """The app prints `n=0, px=, bytes=0` for an empty scene rather than
        nothing -- its own pin says why: a line that vanished is
        indistinguishable from a period the scraper missed. An arm that
        rejected the empty list would read that period as UNCHECKED; it
        parses to `[]`, and the row is refused for having no picture to
        confirm, which is what it has."""
        probes = compile_probes()
        s = scrape([OVERLAY_PICTURES_NONE], probes)
        self.assertEqual(len(s["overlay_pictures"]), 1)
        _idx, r = s["overlay_pictures"][0]
        self.assertEqual(r, {"n": 0, "px": [], "bytes": 0})
        s = surface_check((1920, 1080), (1920, 1080), 0, 0, r, panes=None)
        self.assertFalse(s["unchecked"], "a present line is never absent")
        self.assertFalse(s["met"])
        self.assertIn("no pane with a picture (n=0)", s["why"])

    def test_a_matching_surface_is_confirmed(self):
        s = surface_check((1920, 1080), (1920, 1080), 7, ONE_PANE_PICTURE_BYTES * 7,
                          _reported([ONE_PANE_PICTURE]), panes=1)
        self.assertTrue(s["met"], s.get("why"))
        self.assertFalse(s["unchecked"])
        self.assertEqual(s["expected_picture_bytes"], 17_913_600)
        self.assertEqual(s["tolerance_bytes"], 2 * (2880 + 1555),
                         "half a texel row plus half a texel column, in bytes")
        self.assertEqual(s["reported_px"], "2880x1555")

    def test_the_forty_point_model_figure_is_refused_on_the_leg_that_exposed_it(self):
        """2026-09-02: a one-pane leg at 1920x1080 uploaded 2880x1555 pictures
        and read INVALID against the model's 2880x1560. Read the other way --
        the app reporting 2880x1555 while the uploads were the model's size --
        the check refuses just the same: 57,600 B a picture is six times the
        8,870 B tolerance, and the refusal names the ratio."""
        s = surface_check((1920, 1080), (1920, 1080), 12, MODEL_ONE_PANE_BYTES * 12,
                          _reported([ONE_PANE_PICTURE]), panes=1)
        self.assertFalse(s["met"])
        self.assertFalse(s["bytes_met"])
        self.assertGreater(MODEL_ONE_PANE_BYTES - ONE_PANE_PICTURE_BYTES,
                           s["tolerance_bytes"])
        self.assertIn("1.003x", s["why"], s["why"])
        self.assertIn("2880x1555", s["why"], s["why"])

    def test_an_absent_line_is_unchecked_which_is_not_refused(self):
        """A binary older than the line reports nothing to check against. That
        is the check not running, and it is stamped as such -- never as the
        bytes having failed, and never as a zero-size picture."""
        s = surface_check((1920, 1080), (1920, 1080), 10, ONE_PANE_PICTURE_BYTES * 10,
                          None, panes=1)
        self.assertTrue(s["unchecked"])
        self.assertFalse(s["met"])
        self.assertIn("absent", s["why"])
        self.assertNotIn("expected_picture_bytes", s)
        self.assertNotIn("observed_picture_bytes", s)

    def test_geometry_alone_does_not_confirm(self):
        s = surface_check((1920, 1080), (1920, 1080), 0, 0,
                          _reported([ONE_PANE_PICTURE]), panes=1)
        self.assertFalse(s["met"])
        self.assertFalse(s["unchecked"])
        self.assertIn("no pictures were drawn", s["why"])

    def test_a_wrong_window_is_refused_on_the_apps_own_surface(self):
        """The failure that actually happened: a title-substring `wmctrl -r`
        ran legs at 3440x1440 while the leg believed it asked for 1920x1080.
        The app's surface line says 3440x1440; the pictures match THAT surface
        and the row is refused for the reason it deserves, on geometry."""
        big = (5160, 2835)
        s = surface_check((1920, 1080), (3440, 1440), 10, 5160 * 2835 * 4 * 10,
                          _reported([big]), panes=1)
        self.assertTrue(s["bytes_met"], "the uploads ARE the reported pictures")
        self.assertFalse(s["geometry_met"])
        self.assertFalse(s["met"])
        self.assertIn("3440x1440", s["why"])

    def test_the_recorded_six_pane_mean_is_inside_the_mixture_tolerance(self):
        """2026-09-02: a six-pane leg at 1920x1080 averaged 2,994,468 B a
        picture over panes the app allocated 960x780 pictures for -- 732 B
        under, a minority of pictures one row or one column short -- and the
        tolerance, half a row (960*4/2 = 1,920 B) plus half a column
        (780*4/2 = 1,560 B) = 3,480 B, admits it. The one-picture figure the
        same leg was once priced at is refused on the same reading, and the
        refusal names the shape of the miss: 6.000x."""
        six = [(960, 780)] * 6
        n = 21
        s = surface_check((1920, 1080), (1920, 1080), n, 2_994_468 * n,
                          _reported(six), panes=6)
        self.assertEqual(s["expected_picture_bytes"], 2_995_200)
        self.assertEqual(s["tolerance_bytes"], 3_480, "2 * (960 + 780)")
        self.assertTrue(s["bytes_met"] and s["met"], s.get("why"))
        one = surface_check((1920, 1080), (1920, 1080), n, MODEL_ONE_PANE_BYTES * n,
                            _reported(six), panes=6)
        self.assertFalse(one["met"])
        self.assertIn("6.000x", one["why"], one["why"])

    def test_a_majority_at_a_neighbouring_size_is_not_a_mixture(self):
        """Every picture one texel row short of the reported 960x780 is 3,840
        B under -- more than the 3,480 B band. That is the layout having moved
        under the bracket, not a few pictures off, and it is refused so the
        row says so instead of averaging it away. A sixth of the pictures at
        that size (640 B off the mean) is inside the band."""
        six = _reported([(960, 780)] * 6)
        short = 960 * 779 * 4
        s = surface_check((1920, 1080), (1920, 1080), 12, short * 12, six, panes=6)
        self.assertFalse(s["bytes_met"], "3,840 B short on every picture")
        mixed = 10 * 2_995_200 + 2 * short
        s = surface_check((1920, 1080), (1920, 1080), 12, mixed, six, panes=6)
        self.assertTrue(s["bytes_met"], "2 of 12 a row short: mean 640 B under")

    def test_two_reported_sizes_price_to_a_band(self):
        """A [2, 1] grid reports two 1440x777 pictures over one 2880x777.
        Which pane rastered how often is not in the log, so the mean is
        confirmed anywhere in the band and refused outside it; no single
        `expected_picture_bytes` is printed, because a mean over two pane
        sizes describes no picture."""
        three = _reported([(1440, 777), (1440, 777), (2880, 777)])
        lo, hi = 1440 * 777 * 4, 2880 * 777 * 4
        s = surface_check((1920, 1080), (1920, 1080), 10, 6_000_000 * 10, three, panes=3)
        self.assertIsNone(s["expected_picture_bytes"])
        self.assertEqual(s["expected_label"], "%d..%d" % (lo, hi))
        self.assertEqual(s["tolerance_bytes"], 2 * (2880 + 777), "of the LARGEST picture")
        self.assertTrue(s["met"], s.get("why"))
        for outside in (MODEL_ONE_PANE_BYTES, SIX_PANE_PICTURE_BYTES):
            s = surface_check((1920, 1080), (1920, 1080), 10, outside * 10, three, panes=3)
            self.assertFalse(s["met"], "%d is outside the band" % outside)

    def test_a_mixture_of_reported_sizes_within_tolerance_is_confirmed(self):
        """Six panes whose pictures the app reports in two sizes one texel row
        apart -- thirds and halves of a surface are not exact, and egui's f32
        additions land a pane on either side. The band spans both sizes plus
        the tolerance, so a mean anywhere between them is confirmed, and one
        past the band by a byte is not."""
        px = [(960, 777)] * 3 + [(960, 778)] * 3
        rep = _reported(px)
        lo, hi = 960 * 777 * 4, 960 * 778 * 4
        self.assertEqual(hi - lo, 3_840, "one texel row of a 960-wide picture")
        tol = 2 * (960 + 778)
        for mean in (lo, (lo + hi) // 2, hi, lo - tol, hi + tol):
            s = surface_check((1920, 1080), (1920, 1080), 12, mean * 12, rep, panes=6)
            self.assertTrue(s["met"], "mean %d: %s" % (mean, s.get("why")))
            self.assertEqual(s["tolerance_bytes"], tol)
        for mean in (lo - tol - 12, hi + tol + 12):
            s = surface_check((1920, 1080), (1920, 1080), 12, mean * 12, rep, panes=6)
            self.assertFalse(s["met"], "mean %d is a byte past the band" % mean)

    def test_a_self_contradicting_line_is_refused_not_trusted(self):
        """A count that is not its list's length, or a sum that is not its
        sizes', is the instrument broken; a lenient reader would confirm a
        surface on it."""
        bad_n = {"n": 6, "px": [(960, 777)] * 5, "bytes": 5 * SIX_PANE_PICTURE_BYTES}
        s = surface_check((1920, 1080), (1920, 1080), 10, SIX_PANE_PICTURE_BYTES * 10,
                          bad_n, panes=None)
        self.assertFalse(s["met"])
        self.assertFalse(s["unchecked"])
        self.assertIn("contradicts itself", s["why"])
        bad_sum = {"n": 1, "px": [ONE_PANE_PICTURE], "bytes": ONE_PANE_PICTURE_BYTES + 4}
        s = surface_check((1920, 1080), (1920, 1080), 10, ONE_PANE_PICTURE_BYTES * 10,
                          bad_sum, panes=1)
        self.assertFalse(s["met"])
        self.assertIn("contradicts itself", s["why"])

    def test_the_seeded_pane_count_is_held_against_the_apps(self):
        """Scene C seeds six panes. An app that laid out one ran another
        scene, and the row says so; a seed nothing could read (None) leaves
        that comparison out rather than refusing the row."""
        one = _reported([ONE_PANE_PICTURE])
        s = surface_check((1920, 1080), (1920, 1080), 10, ONE_PANE_PICTURE_BYTES * 10,
                          one, panes=6)
        self.assertFalse(s["met"])
        self.assertFalse(s["panes_met"])
        self.assertIn("another scene", s["why"])
        s = surface_check((1920, 1080), (1920, 1080), 10, ONE_PANE_PICTURE_BYTES * 10,
                          one, panes=None)
        self.assertTrue(s["met"], s.get("why"))

    def test_a_pane_without_a_picture_is_not_priced(self):
        """`0x0` is a pane with nothing to raster; it is not a picture the
        bracket's mean can contain. Every pane empty with pictures drawn is a
        contradiction between two of the app's own lines, and refused."""
        two = _reported([ONE_PANE_PICTURE, (0, 0)])
        s = surface_check((1920, 1080), (1920, 1080), 10, ONE_PANE_PICTURE_BYTES * 10,
                          two, panes=2)
        self.assertTrue(s["met"], s.get("why"))
        self.assertEqual(s["pane_picture_bytes"], [ONE_PANE_PICTURE_BYTES])
        empty = _reported([(0, 0), (0, 0)])
        s = surface_check((1920, 1080), (1920, 1080), 10, ONE_PANE_PICTURE_BYTES * 10,
                          empty, panes=2)
        self.assertFalse(s["met"])
        self.assertIn("no pane with a picture", s["why"])

    def test_the_pane_count_comes_from_the_scene_seed(self):
        """Scene C seeds six panes and scene A one; the count is read out of
        `run_measure.sh`'s own seed rather than restated, so the two targets
        cannot drift onto different scenes under one letter."""
        self.assertEqual(scene_pane_count("C"), 6)
        self.assertEqual(scene_pane_count("A"), 1)
        self.assertEqual(pane_count_of_seed({"squallar.ui": "{}"}), 1)


def _hist_first_bin(n):
    counts = [0] * SLOTS
    counts[0] = n
    return ",".join(str(c) for c in counts)


def _leg_log(per_picture, pictures_line):
    """A whole leg's log, four loops, ten pictures a loop at `per_picture` B.

    `pictures_line` is the app's `overlay pictures:` sentence, or None for a
    binary older than it. Skip 2, window 2: the bracket is loops 2..4, and
    across it the app drew 20 pictures whose mean is exactly `per_picture`.
    """
    t = "[2026-09-02T00:00:00Z INFO  squallar_app::app::render] "
    lines = [
        t + "Surface configured to 1920x1080",
        t + "wgpu selected the Vulkan backend: NVIDIA GeForce RTX 3090 "
            "(DiscreteGpu), driver NVIDIA 610.57.04",
        t + "gesture script pan-zoom-2d begin",
    ]
    for k in range(1, 5):
        n = 100 * k
        lines.append(
            t + "frame service (interact): n=%d, p50=63 us, p90=63 us, p99=63 us, "
            "hist=%s" % (n, _hist_first_bin(n)))
        pics = 10 * k
        lines.append(
            t + "overlay rasters: %d dispatched, %d arrived, %d pictures of %d B, "
            "%d inked, %d shown, 0 promoted, 0 dropped, 0 superseded, 0 cancelled"
            % (pics, pics, pics, pics * per_picture, pics, pics))
        if k == 4 and pictures_line:
            lines.append(pictures_line)
        lines.append(t + "gesture script pan-zoom-2d loop complete: 50 frames")
    lines.append(
        t + "frame service (interact): n=500, p50=63 us, p90=63 us, p99=63 us, "
        "hist=%s" % _hist_first_bin(500))
    return lines


def _leg_args(load_file, panes, log="", json_out=""):
    return argparse.Namespace(
        log=log, scene="A", script="pan-zoom-2d", commit="deadbeef",
        asked_geom=(1920, 1080), achieved_geom=None, panes=panes, dpr="1",
        refresh="60", adapter="unknown", panel="off", position="p1(a)",
        load_file=load_file, quiet_max=8.0, skip_loops=2, window_loops=2,
        platform="linux", degraded="", json=json_out,
    )


class RowVerdictTests(unittest.TestCase):
    """The stamps, at the layer they are printed: a whole log through
    `scrape`, `build_row` and `print_row`. A quiet, live, bracketed leg, so the
    only thing that can stamp the row is the surface check."""

    def setUp(self):
        import tempfile
        self._tmp = tempfile.TemporaryDirectory()
        self.load = os.path.join(self._tmp.name, "load")
        with open(self.load, "w", encoding="utf-8") as fh:
            for i in range(6):
                fh.write("%d\t1.0\n" % (1_000_000 + 5 * i))
        self.probes = compile_probes()

    def tearDown(self):
        self._tmp.cleanup()

    def _row(self, per_picture, pictures_line, panes):
        lines = _leg_log(per_picture, pictures_line)
        row = build_row(_leg_args(self.load, panes), scrape(lines, self.probes), self.probes)
        head = _capture(lambda: print_row(row)).splitlines()
        return row, head[0], "\n".join(head)

    def test_the_leg_fixture_is_quiet_live_and_bracketed(self):
        row, _first, _text = self._row(ONE_PANE_PICTURE_BYTES, OVERLAY_PICTURES_ONE, 1)
        self.assertEqual(row["quiet"], "yes", row["quiet_verdict"]["why"])
        self.assertTrue(row["liveness"]["ok"], row["liveness"]["verdict"])
        self.assertEqual(row["window_basis"], "2 whole loops, 2 skipped")
        self.assertEqual(row["pictures"], 20)

    def test_an_absent_line_stamps_unchecked_and_not_invalid(self):
        row, first, text = self._row(ONE_PANE_PICTURE_BYTES, None, 1)
        self.assertEqual(row["invalid"], [], row["invalid"])
        self.assertEqual(len(row["unchecked"]), 1)
        self.assertEqual(row["cross"], "unchecked")
        self.assertIn("** UNCHECKED: overlay pictures line absent **", first)
        self.assertNotIn("INVALID", first)
        self.assertIn("ROW   UNCHECKED: surface bytes not checked", text)
        self.assertNotIn("ROW   INVALID", text)
        self.assertIn("app_pictures=absent", text)
        self.assertIn("-> UNCHECKED", text)

    def test_uploads_matching_the_reported_pictures_stamp_nothing(self):
        row, first, text = self._row(ONE_PANE_PICTURE_BYTES, OVERLAY_PICTURES_ONE, 1)
        self.assertEqual(row["invalid"], [], row["invalid"])
        self.assertEqual(row["unchecked"], [])
        self.assertEqual(row["cross"], "yes")
        self.assertNotIn("**", first, first)
        self.assertIn("app_pictures=2880x1555 expected=17913600 B/picture (+-8870) "
                      "observed=17913600 B/picture -> CONFIRMED", text)

    def test_uploads_off_the_reported_pictures_stamp_invalid_and_not_unchecked(self):
        row, first, text = self._row(MODEL_ONE_PANE_BYTES, OVERLAY_PICTURES_ONE, 1)
        self.assertEqual(len(row["invalid"]), 1, row["invalid"])
        self.assertEqual(row["unchecked"], [])
        self.assertEqual(row["cross"], "no")
        self.assertIn("** INVALID **", first)
        self.assertNotIn("UNCHECKED", first)
        self.assertIn("ROW   INVALID: surface not confirmed: picture bytes are not the "
                      "pictures the app reports", text)
        self.assertNotIn("ROW   UNCHECKED", text)

    def test_a_six_pane_leg_whose_uploads_match_its_six_pictures_is_valid(self):
        """The row every scene C/D leg printed INVALID: six panes, priced by
        the model as one picture. Read from the app it is valid."""
        row, first, text = self._row(SIX_PANE_PICTURE_BYTES, OVERLAY_PICTURES_SIX, 6)
        self.assertEqual(row["invalid"], [], row["invalid"])
        self.assertEqual(row["unchecked"], [])
        self.assertEqual(row["cross"], "yes")
        self.assertNotIn("**", first, first)
        self.assertIn("panes=6 app_pictures=960x777;960x777;960x777;960x777;960x777;"
                      "960x777 expected=2983680 B/picture (+-3474)", text)
        self.assertIn("-> CONFIRMED", text)

    def test_a_six_pane_leg_priced_as_one_picture_is_what_was_refused(self):
        """And the old failure, spelled forwards: uploads of six-pane pictures
        against a one-picture expectation are refused, naming 6.0x."""
        row, first, _text = self._row(SIX_PANE_PICTURE_BYTES, OVERLAY_PICTURES_ONE, 6)
        self.assertIn("** INVALID **", first)
        self.assertIn("another scene", row["invalid"][0],
                      "the seeded six against the app's one is caught first")

    def test_the_analyser_exits_zero_on_unchecked_and_one_on_invalid(self):
        """`run_measure_native.sh` takes the exit code as the leg's verdict."""
        for per_picture, line, want in (
            (ONE_PANE_PICTURE_BYTES, None, 0),
            (ONE_PANE_PICTURE_BYTES, OVERLAY_PICTURES_ONE, 0),
            (MODEL_ONE_PANE_BYTES, OVERLAY_PICTURES_ONE, 1),
        ):
            log = os.path.join(self._tmp.name, "leg.log")
            with open(log, "w", encoding="utf-8") as fh:
                fh.write("\n".join(_leg_log(per_picture, line)) + "\n")
            rc = [None]
            _capture(lambda: rc.__setitem__(0, cmd_analyze(_leg_args(self.load, 1, log))))
            self.assertEqual(rc[0], want, "line=%r per_picture=%d" % (line, per_picture))


class WindowTests(unittest.TestCase):
    def test_bracket_skips_and_spans_whole_loops(self):
        loops = [(10, "s", 1), (20, "s", 1), (30, "s", 1), (40, "s", 1)]
        br, why = bracket(loops, 2, 2)
        self.assertIsNone(why)
        self.assertEqual(br, (20, 40))

    def test_bracket_refuses_when_there_are_too_few_loops(self):
        br, why = bracket([(10, "s", 1)], 2, 2)
        self.assertIsNone(br)
        self.assertIn("only 1", why)

    def test_a_window_is_a_difference_not_a_cumulative_reading(self):
        base = Reading(5, 100, "1", "1", "1", [0] * SLOTS)
        base.hist[10] = 100
        final = Reading(50, 160, "1", "1", "1", [0] * SLOTS)
        final.hist[10] = 100
        final.hist[20] = 60
        w = diff_window([base, final], 10, 60)
        self.assertEqual(w["n"], 60)
        self.assertEqual(sum(w["hist"]), 60)
        self.assertEqual(
            w["hist"][10], 0,
            "the pre-window samples leaked into the window: this is the "
            "cumulative-from-boot contamination the bracket exists to stop",
        )

    def test_liveness_catches_a_frozen_app(self):
        rs = [Reading(1, 10, "1", "1", "1", [0] * SLOTS),
              Reading(5, 90, "1", "1", "1", [0] * SLOTS),
              Reading(9, 90, "1", "1", "1", [0] * SLOTS)]
        self.assertFalse(liveness(rs, 5)["ok"])
        rs[2] = Reading(9, 120, "1", "1", "1", [0] * SLOTS)
        self.assertTrue(liveness(rs, 5)["ok"])

    def test_liveness_refuses_when_nothing_followed_the_window(self):
        rs = [Reading(1, 10, "1", "1", "1", [0] * SLOTS),
              Reading(5, 90, "1", "1", "1", [0] * SLOTS)]
        v = liveness(rs, 5)
        self.assertFalse(v["ok"])
        self.assertIn("not held open", v["verdict"])


class SharedFormatTests(unittest.TestCase):
    """The pin: this file's row and the web rig's row are one table.

    Non-vacuous by construction -- the key list is READ from `run_measure.sh`
    and the test asserts a positive population before comparing, so a parse
    that found nothing fails rather than passing empty.
    """

    def test_the_web_row_keys_are_readable_and_populous(self):
        keys = shared_row_keys()
        self.assertGreaterEqual(
            len(keys), 15,
            "only %d columns were parsed out of run_measure.sh's ROW line; "
            "the parse broke and this pin would otherwise pass vacuously"
            % len(keys),
        )
        for expect in ("scene", "browser", "viewport", "px", "dpr", "cross",
                       "coi", "panel", "script", "basemap", "pictures",
                       "MB/picture", "commit"):
            self.assertIn(expect, keys)

    def test_the_native_row_carries_every_shared_column(self):
        keys = shared_row_keys()
        row = _fixture_row()
        printed = _capture(lambda: print_row(row)).splitlines()[0]
        for k in keys:
            token = k if k.endswith("~") else (k + "=")
            self.assertIn(
                token, printed,
                "the native ROW line is missing the shared column `%s`; a "
                "native row and a web row can no longer be read in one table"
                % k,
            )

    def test_the_native_row_carries_its_own_columns(self):
        printed = _capture(lambda: print_row(_fixture_row())).splitlines()[0]
        for k in NATIVE_ONLY_ROW_KEYS:
            self.assertIn(k + "=", printed)

    def test_a_clamped_segments_line_scrapes_without_raising(self):
        """`over` is the top-bin clamp, not a number.

        Coercing every scraped group to `int` crashed the analyser on the
        first real leg, AFTER the app had run: the whole measurement was
        already spent by the time the reader failed. A family with
        `(\\d+|none|over)` groups is text.
        """
        probes = compile_probes()
        # Verbatim from a real scene A leg on 2026-08-31, `over` and all.
        line = (
            "[2026-08-31T15:16:40Z INFO  squallar_app::app::render] frame "
            "segments (interact, p99 us): pre=over, pump=2000, ui=19028, "
            "prepare=2000, finish=38055, post=13455; acquire n=1, p50=63 us, "
            "p99=63 us"
        )
        s = scrape([line], probes)
        self.assertEqual(len(s["segments"]), 1)
        self.assertEqual(s["segments"][0][1][0], "over")

    def test_the_adapter_comes_from_the_app_not_the_x_server(self):
        """glxinfo and wgpu disagree, and wgpu is the one that ran the leg.

        On an Xvfb-hosted leg glxinfo answers `llvmpipe` while wgpu selected a
        discrete NVIDIA adapter over Vulkan. Taking the X server's answer put
        the software rasteriser on a row measured on a 3090.
        """
        probes = compile_probes()
        line = (
            "[t INFO squallar_app::app_state] wgpu selected the Vulkan "
            "backend: NVIDIA GeForce RTX 3090 (DiscreteGpu), driver NVIDIA "
            "610.57.04"
        )
        s = scrape([line], probes)
        self.assertEqual(s["backend"], "Vulkan")
        self.assertEqual(s["adapter"], "DiscreteGpu:NVIDIA GeForce RTX 3090")

    def test_every_probe_is_still_declared_by_drive_py(self):
        probes = compile_probes()
        self.assertEqual(len(probes), len(PROBE_NAMES))
        # A positive: the interact probe must actually match a real sentence.
        sample = (
            "[2026-08-31T00:00:00Z INFO  squallar_app] frame service "
            "(interact): n=7, p50=100 us, p90=200 us, p99=300 us, hist=%s"
            % ",".join(["0"] * SLOTS)
        )
        m = probes["svc_interact_re"].search(sample)
        self.assertIsNotNone(
            m, "drive.py's interact probe no longer matches the app's sentence"
        )
        self.assertEqual(m.group(1), "7")

    def test_the_tile_cache_line_scrapes_into_its_own_arm(self):
        """A word group first, thirteen ints after -- `budget state`'s shape.

        The all-`int()` loop would die on the role word, which is why the arm
        is its own; and the sentence here is the app's exactly, so a drift in
        either file reddens this before a leg is spent.
        """
        probes = compile_probes()
        line = (
            "[2026-09-02T00:00:00Z INFO  squallar_app::app::render] tile cache "
            "(base): 1001 asks, 12 restyle asks, 103 refetch after eviction, "
            "904 puts first, 15 restyle, 26 duplicate, 37 orphan, 48 evicted "
            "pending, 59 evicted resident of 6000060 B, 71 entries, 8000082 B "
            "resident, 93 parsed"
        )
        s = scrape([line], probes)
        self.assertEqual(len(s["tile_cache"]), 1)
        idx, role, figures = s["tile_cache"][0]
        self.assertEqual(role, "base")
        self.assertEqual(
            figures,
            [1001, 12, 103, 904, 15, 26, 37, 48, 59, 6000060, 71, 8000082, 93],
        )
        by_role = tile_cache_by_role(s["tile_cache"])
        self.assertEqual(set(by_role), {"base"})
        self.assertEqual(by_role["base"][1][2], 103)
        self.assertIsNone(tile_cache_by_role([]))

    def test_the_budget_state_line_scrapes_into_its_own_arm(self):
        """The bracket word first, fourteen ints after -- the app's exact
        sentence, so a drift in either file reddens this before a leg is
        spent. The last three are the capacity in force, its source and the
        WebGPU probe's state; a binary older than any of those groups matches
        nothing and reads as absent, never as `cap 0`."""
        probes = compile_probes()
        line = (
            "[2026-09-02T00:00:00Z INFO  squallar_app::app::render] budget state: "
            "bracket desktop, rung 1, steps 3, pool 3072 MiB, ceiling 3840 MiB, "
            "vram 24576 MiB, ram 65536 MiB, declared 8192 MiB, threads 32, form 2, "
            "linear 300/700 MiB, cap 5120 2, probe 5"
        )
        s = scrape([line], probes)
        self.assertEqual(len(s["budget_state"]), 1)
        _idx, bracket, figures = s["budget_state"][0]
        self.assertEqual(bracket, "desktop")
        self.assertEqual(
            figures,
            [1, 3, 3072, 3840, 24576, 65536, 8192, 32, 2, 300, 700, 5120, 2, 5],
        )
        older = line.rsplit(", cap", 1)[0]
        self.assertEqual(
            scrape([older], probes)["budget_state"], [],
            "a line without the capacity groups matched: every group is mandatory",
        )
        before_probe = line.rsplit(", probe", 1)[0]
        self.assertEqual(
            scrape([before_probe], probes)["budget_state"], [],
            "a line without the probe group matched: every group is mandatory",
        )

    def test_a_log_without_the_budget_state_line_prints_n_a_not_zero(self):
        row = _fixture_row()
        self.assertIsNone(row["budget_state"])
        text = _capture(lambda: print_row(row))
        self.assertIn("budget: n/a", text)
        self.assertNotIn("cap=0", text)
        row["budget_state"] = (
            7, "desktop",
            [1, 3, 3072, 3840, 24576, 65536, 8192, 32, 2, 300, 700, 5120, 2, 5],
        )
        text = _capture(lambda: print_row(row))
        self.assertIn("budget bracket=desktop rung=1 steps=3 pool=3072 MiB", text)
        self.assertIn("linear=300/700 MiB cap=5120 MiB source=2 probe=5", text)

    def test_a_log_without_the_tile_cache_line_prints_n_a_not_zero(self):
        row = _fixture_row()
        self.assertIsNone(row["tile_cache"])
        text = _capture(lambda: print_row(row))
        self.assertIn("tile cache: n/a", text)
        self.assertNotIn("tile cache (base)", text)
        row["tile_cache"] = {"base": (7, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13])}
        text = _capture(lambda: print_row(row))
        self.assertIn("tile cache (base): asks=1 restyle_asks=2 refetch_after_eviction=3", text)
        self.assertIn("parsed=13", text)

    def test_the_two_stamps_are_distinct_and_each_greppable(self):
        """`** INVALID **` and `** UNCHECKED: ... **` never stand in for each
        other: a row whose bytes could not be checked did not fail the check,
        and a grep for either word finds only its own rows."""
        text = _capture(lambda: print_row(_fixture_row(reported=None)))
        first = text.splitlines()[0]
        self.assertIn("** UNCHECKED: overlay pictures line absent **", first)
        self.assertNotIn("INVALID", text)
        self.assertIn("cross=unchecked", first)
        self.assertIn("ROW   UNCHECKED: surface bytes not checked: overlay pictures "
                      "line absent", text)
        text = _capture(lambda: print_row(_fixture_row(reported="six", panes=6)))
        self.assertNotIn("**", text.splitlines()[0])
        self.assertIn("cross=yes", text)
        self.assertIn("panes=6 app_pictures=960x777;", text)
        self.assertIn("-> CONFIRMED", text)

    def test_a_clamped_leg_is_told_to_quote_the_count(self):
        row = _fixture_row(clamp=True)
        text = _capture(lambda: print_row(row))
        self.assertIn("CLAMPED", text)
        self.assertIn("throughput", text)


class SeedTests(unittest.TestCase):
    def test_localstorage_keys_become_config_filenames(self):
        files = seed_files({
            "squallar.ui": "{}",
            "squallar.frame_telemetry": "1",
            "squallar.raster_telemetry": "1",
        })
        self.assertEqual(
            sorted(files),
            ["frame_telemetry.json", "raster_telemetry.json", "ui.json"],
        )
        self.assertEqual(files["ui.json"], "{}")

    def test_an_unprefixed_key_is_refused_not_dropped(self):
        """Dropping it would seed nothing and measure a default app with
        telemetry off -- a quiet wrong answer."""
        with self.assertRaises(ValueError):
            seed_files({"ui": "{}"})

    def test_the_web_rig_scenes_map_cleanly(self):
        """Every scene `run_measure.sh` defines must be seedable natively.

        Read out of the shell script by sourcing its own definitions, so the
        two targets cannot drift onto different scenes under one letter.
        """
        for scene in ("A", "B", "C", "D", "E1", "E2", "E3"):
            files = seed_files(json.loads(scene_from_shell(scene)))
            self.assertIn("ui.json", files)
            self.assertEqual(files["frame_telemetry.json"], "1")
            self.assertEqual(files["raster_telemetry.json"], "1")
            # The seeded UI must be the scene, not an empty object.
            self.assertIn("pane_count", files["ui.json"])


class QuietTests(unittest.TestCase):
    """The quiet stamp covers the WHOLE leg, or it is not a stamp."""

    @staticmethod
    def _load(vals):
        return {"start": vals[0], "end": vals[-1], "max": max(vals),
                "samples": len(vals)}

    def test_a_leg_loud_only_in_the_middle_is_refused(self):
        """The documented protocol defect: quiet start, quiet end, a sibling
        lane's compile in between. A start-of-leg gate passes this leg, and
        load's error is one-sided, so the ratio it feeds is biased rather than
        noisy."""
        v = quiet_verdict(self._load([0.4, 0.5, 11.2, 9.7, 0.6, 0.4]), 3.0)
        self.assertEqual(v["quiet"], "no")
        self.assertTrue(
            v["loud_mid"],
            "the middle of the leg was over the ceiling while both ends were "
            "under it, and the verdict did not say so -- which is the one "
            "case a start-of-leg gate is blind to",
        )
        self.assertIn("MID-LEG", v["why"])

    def test_a_quiet_leg_is_stamped_quiet(self):
        v = quiet_verdict(self._load([0.4, 0.5, 0.9, 1.1, 0.6]), 3.0)
        self.assertEqual(v["quiet"], "yes")
        self.assertFalse(v["loud_mid"])

    def test_a_leg_loud_at_its_ends_is_refused_but_not_called_mid(self):
        """Non-vacuity for the flag above: `loud_mid` must distinguish, not
        just fire whenever a leg is loud."""
        v = quiet_verdict(self._load([9.0, 8.0, 0.5, 9.5]), 3.0)
        self.assertEqual(v["quiet"], "no")
        self.assertFalse(v["loud_mid"])

    def test_a_series_too_thin_to_cover_the_middle_is_unknown(self):
        v = quiet_verdict(self._load([0.1, 0.1]), 3.0)
        self.assertEqual(
            v["quiet"], "unknown",
            "two samples describe the ends of the leg. Stamping them `yes` is "
            "the fabrication this refuses: a bad row is worse than no row",
        )

    def test_no_samples_at_all_is_unknown_not_quiet(self):
        self.assertEqual(quiet_verdict(None, 3.0)["quiet"], "unknown")


class CpuTimeTests(unittest.TestCase):
    """A quiet log is not a hang."""

    def test_both_platforms_ps_formats_parse(self):
        self.assertAlmostEqual(parse_cpu_time("0:12.34"), 12.34)     # macOS
        self.assertAlmostEqual(parse_cpu_time("00:01:05"), 65.0)     # Linux
        self.assertAlmostEqual(parse_cpu_time("2-01:00:00"), 176400.0)
        self.assertIsNone(parse_cpu_time(""))
        self.assertIsNone(parse_cpu_time("?"))

    def test_a_silent_but_busy_process_is_not_called_wedged(self):
        v = cpu_liveness("0:10.00", "0:19.50", 10.0)
        self.assertTrue(v["busy"])
        self.assertNotIn("wedged process", v["verdict"])

    def test_a_silent_process_burning_no_cpu_is_called_wedged(self):
        v = cpu_liveness("0:10.00", "0:10.00", 10.0)
        self.assertFalse(v["busy"])
        self.assertIn("wedged", v["verdict"])

    def test_an_unreadable_reading_is_not_a_verdict(self):
        v = cpu_liveness("", "0:10.00", 10.0)
        self.assertFalse(v["readable"])
        self.assertIsNone(
            v["busy"],
            "a missing instrument was reported as a finding; `busy=False` "
            "here would read as `wedged` and kill a healthy leg",
        )


class CommitProvenanceTests(unittest.TestCase):
    """Which commit a row is stamped with, per arm.

    The bug these pin: a two-arm run stamped BOTH rows with the tree's HEAD, a
    tree neither binary was built from, and the field had no shape for two
    values. Every case below fails on that behaviour.
    """

    def test_an_arms_own_commit_wins(self):
        self.assertEqual(
            commit_for_arm("ringon", ["ringoff=aaaa", "ringon=bbbb"], "", 2, "head"),
            "bbbb")

    def test_each_arm_of_a_pair_gets_its_own(self):
        specs = ["ringon=bbbb", "ringoff=aaaa"]
        got = [commit_for_arm(x, specs, "", 2, "head") for x in ("ringon", "ringoff")]
        self.assertEqual(got, ["bbbb", "aaaa"],
                         "the two arms of a pair must not share a commit; that "
                         "is the whole defect")

    def test_a_multi_arm_run_never_falls_back_to_head(self):
        # THE CENTRAL ONE. Before the fix this returned `head` for both arms.
        for label in ("ringon", "ringoff"):
            self.assertEqual(
                commit_for_arm(label, [], "", 2, "deadbee"), "unknown",
                "a multi-arm run stamped an arm with the tree's HEAD, which is "
                "a tree neither binary was built from")

    def test_a_single_arm_run_still_defaults_to_head(self):
        self.assertEqual(commit_for_arm("main", [], "", 1, "deadbee"), "deadbee")

    def test_no_git_lands_on_unknown_not_empty(self):
        # `squallar-app`'s native seed pins describe `commit=unknown` for a
        # shipped bundle with no `.git`; an empty string would break that.
        self.assertEqual(commit_for_arm("main", [], "", 1, ""), "unknown")

    def test_the_whole_run_commit_is_still_accepted(self):
        self.assertEqual(commit_for_arm("a", [], "cafe", 2, "head"), "cafe")

    def test_an_arms_own_commit_outranks_the_whole_run_one(self):
        self.assertEqual(
            commit_for_arm("a", ["a=aaaa"], "cafe", 2, "head"), "aaaa")

    def test_a_label_that_was_not_named_is_unknown_not_another_arms(self):
        self.assertEqual(
            commit_for_arm("c", ["a=aaaa", "b=bbbb"], "", 3, "head"), "unknown",
            "an unnamed arm must not inherit a named arm's commit")

    def test_a_malformed_spec_is_ignored_rather_than_matched(self):
        self.assertEqual(
            commit_for_arm("a", ["a"], "", 2, "head"), "unknown",
            "`a` with no `=` is not a LABEL=SHA pair and must not match")


class OrderTests(unittest.TestCase):
    """Counterbalancing is an equal mean position, not a four-letter word."""

    def test_two_arms_two_runs_is_abba(self):
        self.assertEqual(
            leg_order(2, 2, True), [(0, 1), (1, 1), (1, 2), (0, 2)])

    def test_counterbalanced_arms_share_a_mean_position(self):
        for arms in (2, 3, 4):
            for runs in (2, 4):
                means = mean_positions(leg_order(arms, runs, True), arms)
                self.assertEqual(
                    len(set(round(m, 6) for m in means)), 1,
                    "with %d arms over %d counterbalanced runs the arms' mean "
                    "positions were %s; unequal means are an order confound, "
                    "and a decaying box biases every pair the same way"
                    % (arms, runs, means),
                )

    def test_uncounterbalanced_order_is_biased(self):
        """Non-vacuity: the property above must be able to fail, and the order
        this replaced is exactly what fails it."""
        means = mean_positions(leg_order(2, 2, False), 2)
        self.assertNotEqual(
            round(means[0], 6), round(means[1], 6),
            "base-first-in-every-pair produced equal mean positions, so the "
            "counterbalance test above proves nothing",
        )
        self.assertLess(means[0], means[1])


class PlatformTests(unittest.TestCase):
    """A platform gap is a named degradation, never an exit."""

    ALL = {"procfs", "xdotool", "xrandr", "ps", "sysctl", "osascript",
           "system_profiler"}

    def test_macos_is_planned_for_rather_than_refused(self):
        p = platform_plan("Darwin", self.ALL)
        self.assertEqual(p["platform"], "macos")
        self.assertEqual(
            p["degraded"], [],
            "macOS with every tool present still reported missing "
            "capabilities; the hard `exit 2` on the wrong platform is the bug "
            "this replaced, and five lanes wrote private runners because of it",
        )
        self.assertEqual(p["caps"]["window"]["tool"], "osascript")

    def test_every_missing_capability_carries_a_reason(self):
        p = platform_plan("Linux", {"procfs", "ps"})
        self.assertEqual(sorted(p["degraded"]), ["refresh", "window"])
        for cap in p["degraded"]:
            why = p["caps"][cap]["why"]
            self.assertTrue(why and len(why) > 20,
                            "capability %r degraded without a usable reason: "
                            "%r" % (cap, why))
            self.assertIn(p["caps"][cap]["tool"], why)

    def test_an_unknown_platform_still_gets_a_plan(self):
        p = platform_plan("FreeBSD", set())
        self.assertEqual(p["platform"], "unknown")
        self.assertEqual(sorted(p["degraded"]), sorted(NATIVE_CAPABILITIES))
        self.assertIn("FreeBSD", p["caps"]["window"]["why"])

    def test_wayland_cannot_pin_geometry_even_with_xdotool_installed(self):
        """`command -v xdotool` succeeding is not the capability. A leg that
        silently ran unpinned is what the byte cross-check caught by
        factorisation and nothing else caught at all."""
        p = platform_plan("Linux", self.ALL, session="wayland")
        self.assertFalse(p["caps"]["window"]["ok"])
        self.assertIn("wayland", p["caps"]["window"]["why"].lower())
        # And an X session with the same tools keeps it.
        self.assertTrue(
            platform_plan("Linux", self.ALL, session="x11")["caps"]["window"]["ok"])


class AppSurfaceTests(unittest.TestCase):
    """The app's own resize line is the portable geometry readback."""

    def test_the_last_resize_is_the_surface(self):
        lines = [
            "[..] INFO Window resized to 1280x720",
            "[..] INFO something else",
            "[..] INFO Window resized to 1920x1080",
        ]
        self.assertEqual(app_surface(lines), (1920, 1080))

    def test_a_log_with_no_resize_line_has_no_surface(self):
        self.assertIsNone(app_surface(["[..] INFO nothing here"]))

    def test_a_window_that_opened_at_the_target_still_reports_a_surface(self):
        """**The false negative this reader shipped with.**

        A resize line fires on a resize EVENT. An app whose window opens at
        exactly the size asked for is never resized -- under a bare X server
        with no window manager, sizing a window to the size already in force
        produces no event -- so the log carried no resize line, `app_surface`
        answered None, and the runner REFUSED the leg for having "never
        reported a surface". That is the correct case being thrown away, and
        it cost two scene-A legs on 2026-08-31; the surface was confirmed
        right afterwards, by hand, from the picture bytes.

        The startup line is unconditional, so this case now reads back.
        """
        opened_at_target = [
            "[..] INFO Surface configured to 1920x1080",
            "[..] INFO wgpu selected the Vulkan backend",
        ]
        self.assertEqual(
            app_surface(opened_at_target), (1920, 1080),
            "a window that opened at the requested size reports no surface, "
            "so the runner refuses the leg that got the geometry right")
        # And the byte check the refusal was standing in front of agrees.
        self.assertTrue(
            surface_check((1920, 1080), (1920, 1080), 8, ONE_PANE_PICTURE_BYTES * 8,
                          _reported([ONE_PANE_PICTURE]), panes=1)["met"])

    def test_a_later_resize_supersedes_the_startup_line(self):
        """Two sentences, one answer: newest wins. Were the startup line to
        win, a leg the runner successfully resized would be judged against
        the size it opened at rather than the size it ran at -- the same
        wrong-surface failure from the other direction."""
        lines = [
            "[..] INFO Surface configured to 1280x720",
            "[..] INFO Window resized to 1920x1080",
        ]
        self.assertEqual(app_surface(lines), (1920, 1080))

    def test_the_app_line_is_what_the_geometry_check_is_taken_against(self):
        """The window manager says 1920x1080 and the app allocated 3440x1440.
        Trusting the WM would confirm a leg that ran at another size -- the
        exact failure a title-matching `wmctrl -r` produced -- and the bytes
        can no longer catch it on their own: the picture is read from the
        app, not factorised from a surface, so BOTH sides of the byte check
        are the app's. The catch is the app's own surface line, which is why
        it is the achieved geometry whenever the log has one."""
        real = _reported([(5160, 2835)])
        app = surface_check((1920, 1080), (3440, 1440), 10, 5160 * 2835 * 4 * 10, real, 1)
        self.assertFalse(app["met"])
        self.assertFalse(app["geometry_met"])
        self.assertTrue(
            app["bytes_met"],
            "checked against the app's own surface the bytes agree exactly, "
            "so the leg is refused for the reason it deserves -- it ran at "
            "the wrong size -- rather than for an unexplained byte mismatch",
        )
        self.assertIn("3440x1440", app["why"])


class GateSetTests(unittest.TestCase):
    def test_the_default_gate_set_still_exists(self):
        root = os.path.abspath(os.path.join(RIG_DIR, "..", ".."))
        rows = gate_set_report(root)
        self.assertGreaterEqual(len(rows), 4)
        for g in rows:
            self.assertTrue(
                g["exists"],
                "the default gate set names `%s`, whose owning path %s is not "
                "in the tree: the suite moved and the set is stale"
                % (g["cmd"], g["path"]),
            )

    def test_the_workspace_scanning_suite_is_in_the_set(self):
        """The one that keeps catching lanes out: it scans the whole workspace
        and lives in squallar-radar, so naming packages individually skips
        it."""
        cmds = " ".join(g["cmd"] for g in DEFAULT_GATE_SET)
        self.assertIn("doc_citations_resolve", cmds)
        self.assertIn("squallar-radar", cmds)

    def test_the_arch_ratchet_spelling_is_the_working_one(self):
        """`cargo test -p squallar-app arch_ratchets` selects ZERO tests."""
        for g in DEFAULT_GATE_SET:
            if "arch_ratchets" in g["cmd"]:
                self.assertIn("--test arch_ratchets", g["cmd"])
                return
        self.fail("the arch ratchets gate left the default set")


# --------------------------------------------------------- test fixtures ---


def _capture(fn):
    import io
    import contextlib

    buf = io.StringIO()
    with contextlib.redirect_stdout(buf):
        fn()
    return buf.getvalue()


def _fixture_row(clamp=False, panes=1, reported="one"):
    """A printed row's dict. `reported` is "one" or "six" for the app's line
    at that pane count, or None for a log without it."""
    if reported == "six":
        rep, b = _reported([SIX_PANE_PICTURE] * 6), SIX_PANE_PICTURE_BYTES
    elif reported == "one":
        rep, b = _reported([ONE_PANE_PICTURE]), ONE_PANE_PICTURE_BYTES
    else:
        rep, b = None, ONE_PANE_PICTURE_BYTES
    surf = surface_check((1920, 1080), (1920, 1080), 10, b * 10, rep, panes)
    w = {
        "n": 1234,
        "hist": [0] * SLOTS,
        "p50_us": "1000",
        "p90_us": "2000",
        "p99_us": "over" if clamp else "3000",
        "max_us": "over" if clamp else "4000",
    }
    return {
        "scene": "A", "browser": "native", "arm": "native",
        "adapter": "DiscreteGpu:NVIDIA", "backend": "Vulkan",
        "viewport": "1920x1080", "px": 1920 * 1080, "dpr": "1",
        "cross": "yes" if surf["met"] else ("unchecked" if surf["unchecked"] else "no"),
        "hz": "60", "coi": "n/a", "panel": "off",
        "script": "pan-zoom-2d", "basemap": "some-decoded/some-placed",
        "pictures": 10, "mb_per_picture": "%.2f" % (b / 1e6), "commit": "deadbeef",
        "position": "A1",
        "load": {"start": 1.0, "end": 1.2, "max": 1.4, "samples": 9},
        "quiet": "yes", "quiet_max": 8.0,
        "quiet_verdict": quiet_verdict(
            {"start": 1.0, "end": 1.2, "max": 1.4, "samples": 9}, 8.0),
        "platform": "linux", "degraded": [],
        "windows": {"interact": w, "idle": dict(w), "cadence": dict(w)},
        "window_basis": "2 whole loops, 2 skipped",
        "loops": 2, "settled": "2-loop",
        "bracket": {"start_line": 10, "end_line": 90},
        "liveness": {"ok": True, "grew_by": 40, "verdict": "interact frames "
                     "still rising (+40 after the window)"},
        "surface": surf,
        "loop_state": None, "budget_state": None, "tile_cache": None,
        "gpu_unavailable": False,
        "throughput_interact_frames": 1234,
        "percentiles_clamped": clamp,
        "invalid": [] if surf["met"] or surf["unchecked"] else ["surface not confirmed: %s" % surf["why"]],
        "unchecked": ["surface bytes not checked: %s" % surf["why"]] if surf["unchecked"] else [],
        "notes": (["p99 CLAMPED at the over-64ms bin: quote `interact n=1234` "
                   "as this leg's throughput figure, not the percentiles"]
                  if clamp else []),
    }


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
