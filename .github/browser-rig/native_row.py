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

The per-family lines (`frame segment (pre):`, `frame post (dispatch):`,
`tile take (vector):`, and every sibling the app grows) are read through ONE
pattern DERIVED from `drive.py`'s: the tail after the `(<name>): ` group is
read out of `drive.py`, every `drive.py` probe carrying that tail is checked
to agree, and the prefix is generalised. So which families a row carries is
discovered from the log lines present, never from a table. A fixed table
that must equal what the app emits is what silently dropped `prepare:` and
`dispatch:` from the browser rig's summary; here a family nobody listed is a
printed row.

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
an entire early scoreboard; nothing here ever quotes one as a window. That
holds for EVERY family: the per-family lines carry a running `sum`, which is
differenced exactly (the percentiles are bin-quantised and are never
differenced), and a family absent at the LEFT bracket is a genuine zero --
the counters start at zero -- while one absent at the right is an error.

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
SERVE_PY = os.path.join(RIG_DIR, "serve.py")
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
    "tile_bodies_re",
    "gesture_begin_re",
    "gesture_loop_re",
)


# The per-family lines share ONE shape, the app's `named_hist_line`:
# `<prefix> (<name>): n=, sum= us, p50=, p90=, p99=, hist=`. `drive.py`
# spells it once per prefix it knows; this half reads the shape out of the
# seed probe below, checks every other `drive.py` probe carrying that tail
# agrees, and generalises the prefix into a capture. The families a log
# carries are then whatever lines are present.
NAMED_HIST_SEED = "frame_segment_re"
NAMED_HIST_NAME_GROUP = r"\(([a-z0-9-]+)\): "
NAMED_HIST_PREFIX = r"(?<![A-Za-z0-9_:.\-])([a-z]+(?: [a-z]+)*) "


def named_hist_pattern(source=None):
    """The one per-family regex, derived from `drive.py`'s.

    Groups: prefix, name, n, sum, p50, p90, p99, hist. Refuses when the seed
    lost its name group or when two of `drive.py`'s per-family probes no
    longer share a tail -- the shape stopped being one, and a row windowed
    on the wrong shape would print empty families as absences.
    """
    text = source if source is not None else _read(DRIVE_PY)
    seed = drive_pattern(NAMED_HIST_SEED, text)
    at = seed.find(NAMED_HIST_NAME_GROUP)
    if at < 0:
        raise SystemExit(
            "drive.py's `%s` no longer carries a `(<name>): ` group; the "
            "per-family line shape cannot be read out of it" % NAMED_HIST_SEED
        )
    tail = seed[at + len(NAMED_HIST_NAME_GROUP):]
    for needed in (r"n=(\d+), sum=(\d+) us", r"hist=([0-9,]+)"):
        if needed not in tail:
            raise SystemExit(
                "drive.py's `%s` no longer carries `%s`; every windowed "
                "per-family figure here is a difference of it"
                % (NAMED_HIST_SEED, needed)
            )
    for m in re.finditer(r"var ([a-z_]+_re) = /(.*)/;", text):
        body = m.group(2)
        if body.endswith(tail) and not body[:-len(tail)].endswith(NAMED_HIST_NAME_GROUP):
            raise SystemExit(
                "drive.py's `%s` carries the per-family tail behind something "
                "other than a `(<name>): ` group; the shape is no longer one "
                "and this file cannot read it as one" % m.group(1)
            )
    return re.compile(NAMED_HIST_PREFIX + NAMED_HIST_NAME_GROUP + tail)


def named_key(prefix, name):
    """`frame segment (pre)` -> `segment:pre`; `tile take (vector)` ->
    `take:vector`: the last prefix word, then the name -- the browser rig's
    spelling, derived rather than listed."""
    return "%s:%s" % (prefix.split()[-1], name)


def compile_probes(source=None):
    text = source if source is not None else _read(DRIVE_PY)
    out = {n: re.compile(drive_pattern(n, text)) for n in PROBE_NAMES}
    out["named_hist"] = named_hist_pattern(text)
    return out


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
    # `sum` is the running sum of samples in whole microseconds, carried by
    # the per-family lines and by nothing else: None on interact/idle/cadence.
    __slots__ = ("idx", "n", "p50", "p90", "p99", "hist", "sum")

    def __init__(self, idx, n, p50, p90, p99, hist, sum=None):
        self.idx = idx
        self.n = n
        self.p50 = p50
        self.p90 = p90
        self.p99 = p99
        self.hist = hist
        self.sum = sum


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

# winit's own line, not the app's:
# `winit-0.30.13/src/platform_impl/linux/x11/window.rs:192`
#   info!("Guessed window scale factor: {}", scale_factor);
#
# It is the pixels-per-point ratio EVERY pixel figure on this row was measured
# in, and without it a picture size is uninterpretable. The same binary on the
# same display drew one-pane pictures of 2880x1555 on legs that guessed 13/12
# and 2880x1560 on legs that guessed 1, minutes apart, with nothing on either
# row saying which unit it was in.
#
# X11 only -- the line exists in no other winit backend -- so a Wayland or
# macOS leg leaves the field None. None, never 0 and never a default of 1:
# "the leg did not say" and "the leg ran at one" are different facts and only
# one of them is a measurement. The value is winit's `{}` of an f64, so plain
# decimal, and `1` and `1.0833333333333333` are both real observed spellings.
WINIT_SCALE_RE = re.compile(r"Guessed window scale factor: (\d+(?:\.\d+)?)")

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


def window_scale(lines):
    """Every window scale factor winit guessed in this leg, or None.

    `{"scale": <the last>, "seen": [...], "differ": <bool>}`. The LAST is the
    row's, on `app_surface`'s reasoning: a later window supersedes an earlier
    one. `differ` is kept rather than averaged away -- a leg whose windows were
    guessed at two scales measured its pixels in two units, which is a finding
    about the row and not a detail.
    """
    seen = []
    for line in lines:
        m = WINIT_SCALE_RE.search(line)
        if m:
            seen.append(float(m.group(1)))
    if not seen:
        return None
    return {"scale": seen[-1], "seen": seen, "differ": len(set(seen)) > 1}


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
        "tile_bodies": [],
        "overlay_pictures": [],
        "segments": [],
        # `{key: [Reading]}` for every per-family line present, keyed the
        # browser rig's way (`segment:pre`, `dispatch:hitmap`), and the line
        # prefix each key was read from. Discovered, never listed.
        "named": {},
        "named_prefixes": {},
        "begins": [],
        "loops": [],
        "backend": None,
        "adapter": None,
        "surface": None,
        "window_scale": None,
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
            ("tile_bodies", "tile_bodies_re"),
            ("loop_state", "loop_state_re"),
        ):
            m = probes[probe].search(line)
            if m:
                out[key].append((idx, [int(x) for x in m.groups()]))
        # `budget state` is a level too, but its first group is the bracket's
        # NAME, so it cannot ride the all-`int()` loop above: the word is kept
        # as text and the fifteen figures after it are ints. Every group is
        # mandatory; no match at all leaves the family empty, which the row
        # prints as absent -- an older binary, never a zero reading.
        m = probes["budget_state_re"].search(line)
        if m:
            g = m.groups()
            out["budget_state"].append((idx, g[0], [int(x) for x in g[1:]]))
        # `tile cache (<role>)` is running totals with a WORD first, like
        # `budget state`: its own arm, the role kept as text and the fourteen
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
        # The per-family lines, whichever the app wrote: each carries the
        # running `sum` the windowed mean is exact from. A family nobody
        # listed is a family, not a dropped line.
        m = probes["named_hist"].search(line)
        if m:
            g = m.groups()
            key = named_key(g[0], g[1])
            seen = out["named_prefixes"].setdefault(key, g[0])
            if seen != g[0]:
                raise ValueError(
                    "two per-family lines, `%s` and `%s`, share the key `%s`; "
                    "the app grew a prefix this keying cannot tell apart"
                    % (seen, g[0], key)
                )
            out["named"].setdefault(key, []).append(
                Reading(idx, int(g[2]), g[4], g[5], g[6], parse_hist(g[7]),
                        sum=int(g[3]))
            )
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
    out["window_scale"] = window_scale(lines)
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


def diff_window(series, start_idx, end_idx, absent_left_is_zero=False):
    """A family's windowed reading: the difference of two cumulative ones.

    With `absent_left_is_zero`, a family with no reading at or before the
    bracket start is differenced from zero and says so in `basis`: the
    counters start at zero, so a family first written inside the bracket
    (a `tile take (put)` whose first put landed there) is a window from
    boot, not a missing measurement. No reading at or before the END is an
    error either way. Where the readings carry a running `sum` it is
    differenced exactly into `sum_us`, and `mean_us` is derived from it --
    the percentiles are bin-quantised and are never differenced.
    """
    base = at_or_before(series, start_idx)
    final = at_or_before(series, end_idx)
    basis = None
    if final is None:
        return {"error": "no reading inside the bracket"}
    if base is None:
        if not absent_left_is_zero:
            return {"error": "no reading inside the bracket"}
        base = Reading(-1, 0, None, None, None, [0] * SLOTS,
                       sum=0 if final.sum is not None else None)
        basis = ("boot: no reading at or before the bracket start; the "
                 "counters start at zero, so the window is from boot")
    if final.idx == base.idx:
        return {"error": "only one reading inside the bracket; nothing to diff"}
    hist = [f - b for f, b in zip(final.hist, base.hist)]
    if any(h < 0 for h in hist):
        return {"error": "a cumulative histogram went backwards across the bracket"}
    n = final.n - base.n
    out = {
        "n": n,
        "hist": hist,
        "p50_us": fmt_pctl(percentile_upper_micros(hist, 0.50)),
        "p90_us": fmt_pctl(percentile_upper_micros(hist, 0.90)),
        "p99_us": fmt_pctl(percentile_upper_micros(hist, 0.99)),
        "max_us": fmt_pctl(percentile_upper_micros(hist, 1.0)),
    }
    if final.sum is not None and base.sum is not None:
        total = final.sum - base.sum
        if total < 0:
            return {"error": "a cumulative sum went backwards across the bracket"}
        out["sum_us"] = total
        out["mean_us"] = (total // n) if n > 0 else None
    if basis:
        out["basis"] = basis
    return out


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


# ------------------------------------------------------------------ vblank ---
#
# WHY THIS EXISTS. On 2026-09-03 at 11:34:52 this box's panel stopped being
# probed as connected and the X server logged `Setting mode "NULL"`. X kept the
# stale 3440x1440 framebuffer, so nothing downstream failed: the server stayed
# up, browsers still opened headed, the GPU was still real, and screenshots
# still came out with content in them. But with no output there is no vblank,
# so a headed browser gets no animation callback at a display cadence and a
# headed native app presents into nothing. Two sessions then measured on that
# arm for hours -- one of them bisected a 65x "frame-time regression" across 24
# commits that did not exist -- and every void leg carries `invalid: []`. The
# harness never said a word, which is the defect this section is.
#
# TWO READINGS, AND THEY ARE NOT THE SAME FIGURE. Confusing them is what makes
# a naive version of this check wrong.
#
#   `panel_hz` is what the DISPLAY advertises: xrandr's starred mode, or
#   macOS's `system_profiler`. It is a property of the output, not of the app.
#   On a native leg it is what the `hz~` column ALREADY holds --
#   `run_measure_native.sh`'s `plat_refresh` feeds it straight into
#   `--refresh` -- which is why the native arm needs no new reading to be
#   checked, only a rule.
#
#   `hz` on a WEB row is something else: `1000 / p50(rAF)`, the cadence the
#   page ACHIEVED. rAF is clamped to vblank, so on a healthy leg the two agree
#   -- but a slow app on a live panel and a fast app on a dead one both read
#   low, so the achieved figure ALONE cannot carry this check without red-
#   gating legitimately heavy scenes.
#
# So the check is on the PANEL wherever there is a panel reading, and falls
# back to the achieved cadence only where there is not.
#
# WHY THERE IS NO CADENCE THRESHOLD HERE, AND WHY THERE USED TO BE.
# Losing vblank does not stop an engine: it drives rAF off a software timer at
# a nominal 60 Hz. On the void run of 2026-09-04,
# `rd-0f-encodesplit-after-.../d-ff` read p50 = 17.06 ms -> 59 Hz on a box
# whose live Firefox legs read 165-166, so a check spelled "is hz present?"
# passes a leg that measured nothing. The first cut of this gate answered that
# with a ceiling -- 62 Hz, the fallback's own rate plus jitter -- applied to
# legs carrying no panel reading.
#
# THAT CEILING WAS RETIRED ON 2026-09-04, and its own message text said why:
# on an arm whose panel really is 60 Hz the fallback timer and the panel are
# THE SAME NUMBER and no threshold separates them. That is not an edge case,
# it is the platform default -- most laptops, the Mac mini in this fleet,
# plenty of phones -- and every one of them was unquotable on that path.
# Measured on this box the same day, both halves of the pair, one hour apart:
# a genuine `xrandr`-selected 59.96 Hz mode read `hz~59.96`, and the void
# Firefox leg read `hz~59`. Nothing in the two cadences tells them apart. What
# tells them apart is the PANEL READING, which both supported platforms can now
# take -- Linux through `xrandr`, macOS through `system_profiler`, both wired
# into both runners -- so the reading is required rather than approximated.
#
# THE COST, STATED. This is stricter than the threshold it replaces, and it
# marks INVALID some historical legs that were probably fine: every browser leg
# recorded before `run_measure.sh` learned to read the panel, including the
# 175 Hz Chromium leg in `~/.cache/rd-0f-encodesplit-2026-09-02-2155/`, which
# almost certainly ran against this box's live monitor. The trade is deliberate:
# a rule that refuses a leg it cannot vouch for beats one that passes a leg on a
# threshold that cannot do the job, and the escape for an arm with no reader is
# `RIG_PANEL_HZ`, which is exact.

# THE THIRD PANEL STATE. Two were designed; there are three.
#
#   `''` (also `?`, `0`, `n/a`)  the rig ASKED and the display answered with no
#                                active mode. The dead panel of 2026-09-03.
#   `-` (or no field at all)     the rig MEANT to ask and did not get an answer:
#                                the display would not open, or the artefact
#                                predates the reading entirely.
#   `no-reader`                  the rig NEVER ASKED, because this arm has no
#                                reader to ask with -- an unknown platform, a
#                                box without `xrandr`/`system_profiler`, or a
#                                branch that reads no panel at all.
#
# The third wore the second's clothes and that made A RIG BUG INDISTINGUISHABLE
# FROM A DEAD DISPLAY. It cost every macOS browser leg: that arm is headed
# without an X display, `run_measure.sh` keyed the panel read off "needs an X
# display" and so never asked, and a healthy 60 Hz Mac was then refused with a
# message about a monitor that was fine. The reader was wired up; the CLASS had
# no stamp, so the next arm without one would have read the same way.
#
# All three states refuse a panel-backed leg. They refuse it for three
# different reasons, and only one of them is about the monitor.
PANEL_NO_READER = "no-reader"

# A chromium `binary.gpu_mode` / firefox `binary.ff_mode` that means "this leg
# presented onto the machine's own display".
PANEL_BACKED_GPU_MODES = ("headed-host-display", "macos-quartz-headed")
PANEL_BACKED_FF_MODES = ("host",)


def parse_hz(text):
    """A refresh reading -> float, or None for every way of saying "nothing".

    `''` is xrandr finding no starred mode, `?` is the ROW column's own
    spelling for the same, and `-` is `run_measure.sh`'s. None of the three is
    a rate.
    """
    if text is None:
        return None
    s = str(text).strip()
    if not s or s in ("?", "-", "n/a", "unknown", "None"):
        return None
    m = re.match(r"^([0-9]+(?:\.[0-9]+)?)", s)
    if not m:
        return None
    v = float(m.group(1))
    return v if v > 0 else None


def panel_backed_leg(arm, gpu_mode="", ff_mode=""):
    """Did this leg present onto a display this box drives?

    The arms that answer NO, and why none of them is an oversight:

      `headless-swiftshader` is TIER-2. It never opens X, has no display and
      no panel, and legitimately has no refresh rate to read. A vblank gate
      that fired there would break the one arm that still worked on the night
      the panel died, so it is excluded by name and by test.

      `headless-angle-egl` is the headless hardware arm: a real GPU presenting
      to nothing, by design rather than by accident.

      `xvfb` is a virtual display. It advertises a mode of its own and was
      never driven by this box's monitor, so the monitor's absence cannot void
      it. (It has its own well-known artefacts -- a native pan-lag figure on
      this repo turned out to be an Xvfb `present()` artefact -- but that is a
      different defect and not one a refresh reading can see.)

    A native leg answers YES: `run_measure_native.sh` runs the app headed on
    whatever `DISPLAY` it is given, and reads that display's own refresh.
    """
    if (arm or "").strip().lower() == "native":
        return True
    if (ff_mode or "").strip().lower() in PANEL_BACKED_FF_MODES:
        return True
    return (gpu_mode or "").strip().lower() in PANEL_BACKED_GPU_MODES


def panel_state(panel_hz):
    """Which of the FOUR things a panel reading can be.

    `live` a rate. `dead` a display asked and answering with no active mode.
    `no-reader` the rig never asked -- see `PANEL_NO_READER`. `absent` the rig
    meant to ask and carries no answer, which is also what an artefact recorded
    before the reading existed looks like.

    `absent` and `no-reader` produce the same VERDICT and never the same
    REASON: one is a run that lost its reading, the other is an arm that has no
    reader, and telling a lane to fix the wrong one is what this split exists
    to stop.
    """
    if panel_hz is None:
        return "absent"
    s = str(panel_hz).strip()
    if s == PANEL_NO_READER:
        return "no-reader"
    if s == "-":
        return "absent"
    return "live" if parse_hz(s) is not None else "dead"


PANEL_BASIS = {
    "live": "the display's own advertised mode",
    "dead": "the display was read and reported no active mode",
    "absent": "no panel reading reached this leg",
    "no-reader": "this arm has no panel reader, so none was attempted",
}

_WHY_DEAD = (
    "the display advertises no active mode (read back %r). With no output "
    "there is no vblank: a headed browser gets no animation callback at a "
    "display cadence and a headed native app presents into nothing, so this "
    "leg measured nothing -- whatever figures it printed")

_WHY_ABSENT = (
    "this leg presented onto a display and carries NO PANEL READING, so the "
    "rig cannot say whether it ran against a monitor or against the ~60 Hz "
    "software timer both engines fall back to when they lose vblank. A "
    "threshold on the achieved cadence used to stand in for the reading and "
    "was retired on 2026-09-04: a genuine 60 Hz panel and that fallback are "
    "the same number, so it made every 60 Hz machine unquotable. Record the "
    "panel -- a runner new enough to read it, or RIG_PANEL_HZ")

_WHY_NO_READER = (
    "the rig NEVER READ A PANEL for this leg: this arm has no reader to read "
    "one with (an unknown platform, or a box without xrandr / "
    "system_profiler). This says nothing about the monitor, which may be "
    "perfectly alive -- it is a fact about the RIG. It still refuses the leg, "
    "because a cadence the rig cannot vouch for is not a denominator. Give the "
    "arm a reader, or declare RIG_PANEL_HZ, which is exact")

_WHY_NO_CADENCE = (
    "no refresh rate could be read for a leg that presented onto a display. "
    "This is not a missing column: it is a leg with no measured cadence, and "
    "a cadence is the denominator of every frame figure on the row")


def refresh_verdict(panel_backed, hz, panel_hz=None):
    """Did this leg run against a real vblank?

    `panel_hz` is the display's own advertised rate in one of the four
    spellings `panel_state` names. `hz` is the row's own column: the panel rate
    on a native row, the achieved rAF cadence on a web one.

    Every reason that applies is collected in `reasons`; `why` is the first of
    them, which is the one worth printing. A leg with no panel reading AND no
    cadence has two things wrong with it and the row should not have to pick.
    """
    achieved = parse_hz(hz)
    state = panel_state(panel_hz)
    out = {"ok": True, "why": None, "reasons": [], "panel_state": state,
           "panel_hz": parse_hz(panel_hz), "hz": achieved}
    if not panel_backed:
        out["basis"] = ("not a panel-backed leg: it presents to no display, "
                        "so there is no refresh rate to hold it to")
        return out
    out["basis"] = PANEL_BASIS[state]
    reasons = []
    # Most specific first: a display with no mode explains everything else on
    # the row, so it is the reason worth printing when it applies. The two
    # no-reading states come next -- ahead of the cadence, because on a native
    # row the panel reading IS the cadence column and a missing cadence there
    # is the SYMPTOM of the missing reading rather than a second finding.
    if state == "dead":
        reasons.append(_WHY_DEAD % (panel_hz,))
    elif state == "no-reader":
        reasons.append(_WHY_NO_READER)
    elif state == "absent":
        reasons.append(_WHY_ABSENT)
    # ABSENT IS A HARD REFUSAL EVEN UNDER A LIVE PANEL. A leg whose rAF sample
    # timed out measured no cadence at all, and a declared panel does not
    # supply one: `rd-0f-cr-retake-0132` is that shape.
    if achieved is None:
        reasons.append(_WHY_NO_CADENCE)
    # A live panel and a measured cadence need not MATCH: rAF falling below
    # vblank is the app being slow, which is the thing these legs exist to
    # measure, not a reason to refuse one.
    if reasons:
        out["ok"] = False
        out["reasons"] = reasons
        out["why"] = reasons[0]
    return out


def flat_panes_of_seed(web_seed):
    """How many of a seed's panes raster WHOLE-PICTURE overlays.

    A pane with `"render": "Volume"` is the 3D view, which draws no
    whole-picture overlay raster at all; every other pane is the 2D map, which
    does. The seed is the ONLY source, for the same reason `pane_count_of_seed`
    gives: a list restated in the runner is a second place for a scene to be
    wrong. Scene B is one Volume pane (no pictures, by design); scene C is
    three Volume and three 2D (1068-1080 pictures on a recorded leg).
    """
    ui = json.loads(web_seed.get("squallar.ui") or "{}")
    panes = ui.get("panes")
    if not isinstance(panes, list):
        # `pane_count` with no list is the app's default pane, which is 2D.
        return int(ui.get("pane_count", 1))
    return sum(1 for p in panes
               if not (isinstance(p, dict) and p.get("render") == "Volume"))


def scene_draws_pictures(scene, panel="off"):
    """True/False, or None when the scene's own seed cannot be read."""
    try:
        seed = json.loads(scene_from_shell(scene, panel))
    except (SystemExit, ValueError, KeyError, TypeError):
        return None
    try:
        return flat_panes_of_seed(seed) > 0
    except (ValueError, KeyError, TypeError):
        return None


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
    on 2026-08-31, all of them at display scale 1.0, where a point is a pixel.
    The 40 is right and has not moved: it is `MIN_BAR_HEIGHT`,
    `2 * VERTICAL_MARGIN + INTERACT_HEIGHT`, and the bar lays out on it. What
    the model has no term for is the scale factor, and no leg recorded one. A
    headed X11 leg on 2026-09-02 ran at 13/12 -- winit quantizes an X11 scale
    to twelfths -- which is 43.333 px of bar: at 1920x1080 a one-pane leg
    uploaded 2880x1555 pictures (17,913,600 B) where the model still said
    2880x1560 (17,971,200 B), five texel rows and 57,600 B on every picture,
    and every multi-pane row read `** INVALID **` against a figure the app no
    longer produced. `run_measure_native.sh` now pins the scale at 1 so a
    native leg's points are its pixels, but the check does not lean on that:
    a figure in points cannot predict pixels on a surface whose scale the
    harness never sees. So the app says what it allocated -- `overlay pictures:`, one entry
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

    THREE stamps, and they never co-fire. UNCHECKED `line_absent` is the
    above. UNCHECKED `no_pictures` is a well-formed line with panes (n > 0)
    and NO picture drawn in the bracket: a 3D orbit (scene B) draws no
    whole-picture overlay raster, so `pictures=0` on every such row, and
    until 2026-09-02 that read `** INVALID **  surface not confirmed: no
    pictures were drawn in the window` on every 3D row forever -- a
    permanently-on validity marker stops being read. It fires whether the
    line lists sized panes (the app allocates a picture it never rasters
    into) or every pane as `0x0`; the list is the app's allocation, not its
    drawing, and either way there are no bytes to hold against it. What it
    does NOT cover: `n=0`, which stays a refusal -- a scene with no panes is
    another scene -- and pictures drawn (`pictures > 0`) with no pane
    reported to hold one, which is two of the app's own lines contradicting
    each other. INVALID stays for those and for a real mismatch.
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
        out["unchecked_kind"] = "line_absent"
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
        if reported["n"] and not pictures:
            out["unchecked"] = True
            out["unchecked_kind"] = "no_pictures"
            out["why"] = (
                "the app reports no pane with a picture (n=%d) and none was "
                "drawn in the bracket: the scene draws no whole-picture "
                "overlay raster, so there are no bytes to hold against the "
                "line" % reported["n"]
            )
            return out
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
        out["unchecked"] = True
        out["unchecked_kind"] = "no_pictures"
        out["why"] = (
            "no pictures were drawn in the window though the app allocates "
            "%s: the scene draws no whole-picture overlay raster (a 3D orbit "
            "draws none), so the surface cannot be confirmed from the bytes "
            "and they were checked against nothing" % out["reported_px"]
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


# ---------------------------------------------------------------- subject ---
#
# THE SUBJECT OF A MEASUREMENT: which build of which thing the leg ran.
#
# A browser updated itself out from under a live campaign on 2026-09-04. The
# Mac's `/Applications/Firefox.app` moved 155.0 -> 155.0.1: the bundle's mtime
# moved at 12:12 and the version string moved with it, both observed. That
# alone is what this section is for, and it is enough -- a leg that DIES is
# obvious, but a leg that SUCCEEDS on a browser build its counterpart never ran
# prints a delta, and the delta gets attributed to the code change.
#
# **What this section does not claim is a cause for the leg failures that
# followed.** They were first read as the updater re-execing under the rig's
# launch, and that reading was wrong: the rig's own Firefox had re-exec'd and
# was still RUNNING under a pid the watcher had lost, holding the profile lock,
# so every later launch into that profile directory exited 0 in about four
# seconds with the identical message. Two different defects printing one line,
# and a THIRD state between them -- "succeeded, under a pid nobody is
# watching" -- which is indistinguishable from the failure to a reader that
# only has the exit code. That defect is somebody else's; only the version
# change is this gate's, and writing the wrong mechanism into a durable file
# is the failure this campaign keeps paying for.
#
# Every field needed to catch it was ALREADY RECORDED and nothing read it.
# `binary.browser_version` and `binary.driver_version` ride on every browser
# leg; `commit` rides on every native row. No comparison printed either and no
# comparison asserted either.
#
# WHAT CONSTITUTES A SUBJECT IS A TABLE -- `SUBJECT_FIELDS` below -- and
# deliberately neither of the two things it could have been:
#
#   * not a NAMING RULE ("every field whose name contains `version` must
#     agree"). That demands `driver_version` match across a Firefox pair whose
#     geckodriver is versioned independently of Firefox and is `None` in
#     `version_match` for exactly that reason -- it would refuse every honest
#     Firefox comparison -- and it misses `commit`, which carries no `version`
#     in its name and is the more common error of the two.
#   * not a BLANKET check ("some code somewhere compares builds"), which is
#     satisfiable by anything and pins nothing.
#
# The table names each field, where it is read from, whether a DIFFERENCE in it
# is a defect or the declared axis of the comparison, and whether it may carry
# the positive verdict. Because the table names what must be PRESENT, two
# absent fields cannot satisfy a match -- the sibling-positive property, for
# free. That property is the one this file keeps having to relearn: a count of
# 0 beside siblings at 14, 7 and 5 is a finding, and the same 0 alone is a
# story.
#
# OVER-FIRING IS THE WORSE DIRECTION. A gate here that refused every valid
# comparison would cost more than the silent defect it was built for, and the
# panel gate that landed hours earlier over-fired twice before that was written
# down. So a difference is only a defect IN SCOPE:
#
#   * a browser build may differ freely when the two rows name DIFFERENT
#     browsers -- firefox against chromium is a comparison, not a moved
#     subject;
#   * an app commit may differ freely when the two rows are DIFFERENT ARMS --
#     `before` against `after` is the entire point of an A/B pair. It may not
#     differ between two runs of the SAME arm, which is what the mid-campaign
#     update produces.

# Spellings that mean "this field was not recorded", not "this field is empty".
# `unknown` is `commit_for_arm`'s honest fallback and `?`/`-` are the row
# printer's; all of them must read as absent, or a pair of `unknown`s would
# satisfy an equality check and print a match nobody earned.
SUBJECT_ABSENT = ("", "-", "?", "none", "null", "unknown", "unrecorded", "n/a")

# A commit field naming MORE THAN ONE BUILD. `--commit <a>+<b>` and
# `<a>-vs-<b>` are the two spellings two lanes independently invented on
# 2026-09-02 to force one field to carry a two-arm run, which `commit_for_arm`
# exists to replace. They are still on disk in recorded rows, and they are the
# false green this whole check would otherwise print: `A.before` and `A.after`
# both carry the literal string `6e936c6a-vs-9fcccaae`, so a plain equality
# test calls a two-commit comparison MATCHED. Two sha-shaped tokens in one
# field is the tell; one sha plus a lane's suffix (`59f08766-p1abl`) is not.
_SHA_TOKEN = re.compile(r"(?<![0-9a-zA-Z])[0-9a-f]{7,40}(?![0-9a-zA-Z])")


class SubjectField(object):
    """One row of the subject table.

    `read` pulls the value off a leg artefact -- native rows and browser legs
    are different shapes and both are read here, so one rule adjudicates either
    kind of pair. `gated_when` names the relation under which a DIFFERENCE is a
    defect rather than the comparison's declared axis. `may_pin` says whether
    this field is allowed to carry the positive verdict on its own.

    `shape` PINS THE SPELLING the value is recorded in, and it is a stronger
    gate than equality on its own. Equality only asks whether two recorded
    values agree; a field that quietly changes SHAPE -- a string becoming an
    object, a version losing its dotted number -- can go on comparing equal, or
    stop being read at all, while every comparison across the change is
    invalid. The rig has already had to work around this one level down: an
    app line keeps `since_boot=N us` as a literal token specifically so that
    prefix matches survive a change to the rest of the sentence. A shape that
    no longer holds costs the field its verdict (UNCHECKED, never INVALID --
    an unfamiliar spelling is not evidence of a defect), so a reshaped field
    stops reading as agreement.
    """

    __slots__ = ("name", "read", "gated_when", "may_pin", "shape", "shape_is",
                 "why")

    def __init__(self, name, read, gated_when, may_pin, shape, shape_is, why):
        self.name = name
        self.read = read
        self.gated_when = gated_when
        self.may_pin = may_pin
        self.shape = shape
        self.shape_is = shape_is
        self.why = why

    def misshapen(self, value):
        """Is this a value the table no longer recognises the spelling of?"""
        return value is not None and not self.shape.search(value)


def _clean(value):
    """A recorded value, or None for one of the absent spellings."""
    if value is None:
        return None
    text = str(value).strip()
    if not text or text.lower() in SUBJECT_ABSENT:
        return None
    return text


def _read_browser_version(row):
    """`binary.browser_version`, falling back to the w3c session's own answer.

    Two independent recordings of the same fact. `binary` is what the rig ran;
    `session.browserVersion` is what the browser told the driver it was. A leg
    that carries only one of them is still pinnable.
    """
    binary = row.get("binary") or {}
    got = _clean(binary.get("browser_version"))
    if got:
        return got
    session = row.get("session") or {}
    return _clean(session.get("browserVersion"))


def _read_driver_version(row):
    binary = row.get("binary") or {}
    # geckodriver's `--version` is a paragraph with a licence in it. The first
    # line is the version; the rest would make every comparison unreadable.
    got = _clean(binary.get("driver_version"))
    return got.splitlines()[0].strip() if got else None


def _read_commit(row):
    return _clean(row.get("commit"))


def subject_browser(row):
    """Which browser the leg ran in: `firefox`, `chromium`, `native`, ..."""
    return _clean(row.get("browser"))


_POSITION_LABEL = re.compile(r"^p\d+\((.+)\)$")


def subject_arm_label(row):
    """Which ARM of a multi-arm run this leg is, or None.

    `run_measure_native.sh` writes `$scene.$label.rN.json` and stamps the leg's
    matrix slot as `pN(label)`, so the arm label is recoverable from the row
    itself rather than from the filename a caller happened to pass. A browser
    leg has no matrix; its `tag` is the closest thing it has to an arm.
    """
    position = _clean(row.get("position"))
    if position:
        m = _POSITION_LABEL.match(position)
        if m:
            return m.group(1)
    return _clean(row.get("tag"))


def subject_composite(value):
    """Does this field name more than one build? See `_SHA_TOKEN`."""
    if not value:
        return False
    return len(set(_SHA_TOKEN.findall(value))) > 1


# The shapes, pinned. Loose enough that every spelling on disk today passes --
# `Mozilla Firefox 154.0`, `Chromium 151.0.7922.173 Arch Linux`, the bare
# `155.0` the w3c session reports -- and tight enough that a value which stops
# naming a build stops counting as one.
SHAPE_DOTTED_VERSION = re.compile(r"\d+\.\d+")
SHAPE_NONEMPTY_LINE = re.compile(r"\S")
SHAPE_SHA = re.compile(r"(?<![0-9a-zA-Z])[0-9a-f]{7,40}(?![0-9a-zA-Z])")

SUBJECT_FIELDS = (
    SubjectField(
        "browser_version", _read_browser_version, "same-browser", True,
        SHAPE_DOTTED_VERSION, "one line carrying a dotted version number",
        "the browser build the leg ran in. It moves WITHOUT anyone asking -- "
        "an installed browser applies a staged update on its next launch -- "
        "so it is checked on every pair of legs that named the same browser. "
        "Across DIFFERENT browsers a difference is the comparison itself",
    ),
    SubjectField(
        "driver_version", _read_driver_version, None, False,
        SHAPE_NONEMPTY_LINE, "one non-empty line",
        "the automation driver. REPORTED, NEVER GATED, and never the positive "
        "verdict: geckodriver is versioned independently of Firefox, which is "
        "why `binary.version_match` is None on every Firefox leg and True on "
        "Chromium ones. Gating it would refuse every honest Firefox pair",
    ),
    SubjectField(
        "commit", _read_commit, "same-arm", True,
        SHAPE_SHA, "a 7-40 character hex sha, possibly with a lane suffix",
        "the app build the leg measured. Two runs of the SAME arm on "
        "different commits is the defect; two DIFFERENT arms on different "
        "commits is what an A/B pair is for",
    ),
)


def subject_relation(a, b):
    """How two legs are related, which is what decides scope.

    Unknown reads as NOT the same, in both directions, so an unreadable row
    can only ever relax a gate -- never invent one. A missing field must not
    be able to manufacture a refusal.
    """
    a_browser, b_browser = subject_browser(a), subject_browser(b)
    a_arm, b_arm = subject_arm_label(a), subject_arm_label(b)
    return {
        "a_browser": a_browser,
        "b_browser": b_browser,
        "same-browser": bool(a_browser and b_browser and a_browser == b_browser),
        "a_arm": a_arm,
        "b_arm": b_arm,
        "same-arm": bool(a_arm and b_arm and a_arm == b_arm),
    }


def subject_pin(a, b):
    """Adjudicate the SUBJECT of a two-row comparison.

    Four states, and like this file's other stamp sets they never co-fire:

      `pinned`       at least one may-pin field was recorded on BOTH rows and
                     agreed. This is the positive, and it is not satisfiable by
                     absence: a field missing on either side is `unrecorded`
                     and cannot pin.
      `declared`     nothing moved out of scope, nothing pinned, but a field
                     differs WITHIN scope-to-differ -- the pair's own axis, an
                     A/B on two commits. A result, not a complaint.
      `moved`        a field differed where a difference is a defect. The
                     comparison is INVALID: whatever delta it prints, the
                     subject changed underneath it.
      `unrecorded` / `unresolvable` / `misshapen`
                     the rig cannot vouch either way. UNCHECKED, never
                     INVALID -- and all three are kept apart because they are
                     different findings with different remedies. `unrecorded`
                     is a rig that did not write the field down; `unresolvable`
                     is a field that was written down and names two builds, so
                     it cannot say which one this row measured; `misshapen` is
                     a field recorded in a spelling the table no longer pins,
                     which is the failure equality alone cannot see -- two
                     reshaped values go on comparing equal while nothing that
                     reads the old spelling is reading a version at all.
    """
    relation = subject_relation(a, b)
    fields, invalid, unchecked, pinned_by, declared_by = [], [], [], [], []

    for field in SUBJECT_FIELDS:
        a_val, b_val = field.read(a), field.read(b)
        gated = bool(field.gated_when) and relation[field.gated_when]
        if a_val is None or b_val is None:
            state = "unrecorded"
        elif field.misshapen(a_val) or field.misshapen(b_val):
            state = "misshapen"
        elif subject_composite(a_val) or subject_composite(b_val):
            state = "unresolvable"
        elif a_val == b_val:
            state = "pinned" if field.may_pin else "reported"
        elif gated:
            state = "moved"
        else:
            state = "declared" if field.may_pin else "reported"
        fields.append({"name": field.name, "a": a_val, "b": b_val,
                       "state": state, "gated": gated, "why": field.why})
        if state == "pinned":
            pinned_by.append(field.name)
        elif state == "declared":
            declared_by.append(field.name)
        elif state == "moved":
            invalid.append(
                "%s moved under the comparison: a=%r b=%r. These two rows did "
                "not measure the same subject, so any delta between them is "
                "unattributable" % (field.name, a_val, b_val))
        elif state == "unresolvable":
            unchecked.append(
                "%s names more than one build (a=%r b=%r), so it cannot say "
                "which build either row measured" % (field.name, a_val, b_val))
        elif state == "misshapen":
            unchecked.append(
                "%s is recorded (a=%r b=%r) in a spelling the table does not "
                "pin -- it should be %s. A field that changed shape may still "
                "compare equal while every comparison across the change is "
                "invalid, so it stops carrying a verdict until the table is "
                "updated deliberately"
                % (field.name, a_val, b_val, field.shape_is))

    if invalid:
        state = "moved"
    elif pinned_by:
        state = "pinned"
    elif declared_by:
        state = "declared"
    elif any(f["state"] == "misshapen" for f in fields):
        state = "misshapen"
    elif unchecked:
        state = "unresolvable"
    else:
        state = "unrecorded"
        unchecked.append(
            "no subject field was recorded on both rows (%s), so the rig "
            "cannot tell whether these legs ran the same build"
            % ", ".join(f.name for f in SUBJECT_FIELDS))

    return {
        "state": state,
        "relation": relation,
        "fields": fields,
        "pinned_by": pinned_by,
        "declared_by": declared_by,
        "invalid": invalid,
        "unchecked": unchecked,
    }


SUBJECT_BANNER = {
    "moved": "  ** INVALID **",
    "unrecorded": "  ** UNCHECKED: subject not recorded **",
    "unresolvable": "  ** UNCHECKED: subject not resolvable **",
    "misshapen": "  ** UNCHECKED: subject field changed shape **",
    "pinned": "",
    "declared": "",
}


def print_subject(verdict, a_path="a", b_path="b"):
    """The SUBJECT block: the table first, then the verdict it follows from.

    The table is printed on EVERY verdict, green included. A gate that prints
    only when it fires cannot be read as evidence that it looked -- and the
    positive here is the whole point, because `pinned` has to be visibly
    carried by a value rather than by two blanks.
    """
    rel = verdict["relation"]
    print("SUBJECT a=%s b=%s" % (a_path, b_path))
    print("SUBJECT   relation: browser a=%s b=%s (%s); arm a=%s b=%s (%s)"
          % (rel["a_browser"] or "-", rel["b_browser"] or "-",
             "same" if rel["same-browser"] else "differ",
             rel["a_arm"] or "-", rel["b_arm"] or "-",
             "same" if rel["same-arm"] else "differ"))
    for f in verdict["fields"]:
        print("SUBJECT   %-16s %-12s a=%s b=%s%s"
              % (f["name"], f["state"],
                 ("%r" % f["a"]) if f["a"] is not None else "NOT RECORDED",
                 ("%r" % f["b"]) if f["b"] is not None else "NOT RECORDED",
                 "" if (f["gated"] or f["a"] is None or f["b"] is None)
                 else "  [a difference here is not a defect: %s]"
                 % ("different browsers" if f["name"] == "browser_version"
                    else "different arms" if f["name"] == "commit"
                    else "reported, never gated")))
    for why in verdict["invalid"]:
        print("SUBJECT   INVALID: %s" % why)
    for why in verdict["unchecked"]:
        print("SUBJECT   UNCHECKED: %s" % why)
    if verdict["state"] == "pinned":
        print("SUBJECT %s -> the pair agrees on %s"
              % (verdict["state"], ", ".join(verdict["pinned_by"])))
    elif verdict["state"] == "declared":
        print("SUBJECT %s -> %s differs BY DESIGN across these arms; it is "
              "the axis of the comparison, not a moved subject"
              % (verdict["state"], ", ".join(verdict["declared_by"])))
    else:
        print("SUBJECT %s%s" % (verdict["state"], SUBJECT_BANNER[verdict["state"]]))
    return 1 if verdict["state"] == "moved" else 0


def subject_census(legs):
    """One RUN's legs, grouped by browser: which builds did this run use?

    The pairwise check adjudicates a comparison someone assembled. This one
    catches the incident that produced this whole section, which happened
    INSIDE a single run: a browser that updates itself between leg 3 and leg 4
    leaves that run holding two populations, and nobody assembled a pair to
    notice.

    `legs` is an iterable of `(label, row)`.
    """
    seen = {}
    for label, row in legs:
        browser = subject_browser(row) or "?"
        version = _read_browser_version(row)
        seen.setdefault(browser, {}).setdefault(version, []).append(label)
    out = []
    for browser in sorted(seen):
        builds = seen[browser]
        recorded = {v: t for v, t in builds.items() if v is not None}
        if len(recorded) > 1:
            state = "moved"
        elif recorded:
            state = "pinned"
        else:
            state = "unrecorded"
        out.append({"browser": browser, "state": state,
                    "builds": {("NOT RECORDED" if v is None else v): tags
                               for v, tags in builds.items()}})
    return out


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
        "refresh": ("xrandr", "the vblank check AND the hz~ column: with no reading the rig cannot tell a live panel from a dead one, so the row is marked INVALID rather than quietly trusted. Declare RIG_PANEL_HZ to run without it"),
        "cputime": ("ps", "the wedged-vs-working distinction on a silent log"),
    },
    "macos": {
        "loadavg": ("sysctl", "the quiet stamp: the row cannot say the box "
                              "was quiet and is marked INVALID"),
        "window":  ("osascript", "geometry PINNING: needs Accessibility "
                                 "permission for the process running this, "
                                 "and System Events resolves the window by "
                                 "`unix id`, never by title"),
        "refresh": ("system_profiler", "the vblank check AND the hz~ column: with no reading the rig cannot tell a live panel from a dead one, so the row is marked INVALID rather than quietly trusted. Declare RIG_PANEL_HZ to run without it"),
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
    # Every per-family line the log carries, on the SAME bracket. Before this
    # loop existed the only native reading for a cut family was the last
    # tick's cumulative-from-boot total -- the contamination the bracket
    # exists to remove, re-imported for exactly the families the split was
    # built to read.
    for key in sorted(scraped["named"]):
        w = diff_window(scraped["named"][key], start_idx, end_idx,
                        absent_left_is_zero=True)
        w["line"] = scraped["named_prefixes"][key]
        windows[key] = w

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
    # Native legs open a real window on whatever DISPLAY they were given.
    panel_backed = panel_backed_leg("native")
    # The unit every pixel figure above was measured in, read from winit's own
    # line and never assumed. It rides with the geometry because it is part of
    # the geometry: `achieved=1920x1080` at 13/12 and at 1 are two different
    # surfaces in pixels, and two rows that do not both carry this are not
    # comparable however alike their columns look.
    ws = scraped["window_scale"]
    scale = ws["scale"] if ws else None
    surf["scale"] = scale
    surf["scale_seen"] = ws["seen"] if ws else None
    if ws and ws["differ"]:
        notes.append(
            "winit guessed more than one window scale factor in this leg (%s). "
            "The row carries the last; the pixel figures on it are not all in "
            "one unit" % ", ".join("%s" % v for v in ws["seen"])
        )
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
    #
    # ONE PROMOTION, and it is the only way the `no_pictures` UNCHECKED can
    # become a refusal. That stamp exists because scene B is one 3D pane and
    # draws no whole-picture overlay raster BY DESIGN, so `pictures=0` there is
    # a fact about the scene rather than about the leg. It is NOT a fact about
    # a scene whose seed lays out a 2D pane: scene C drew 1068-1080 pictures on
    # every live leg and 0 on every leg taken after the panel died. A 2D scene
    # that drew none measured nothing, and on a panel-backed leg that is a
    # refusal. The stamps still never co-fire -- the promotion REPLACES the
    # UNCHECKED rather than joining it.
    unchecked = []
    promoted = None
    if (surf["unchecked"] and surf.get("unchecked_kind") == "no_pictures"
            and panel_backed and scene_draws_pictures(args.scene, args.panel)):
        promoted = (
            "no pictures drawn: the scene %s seed lays out a 2D pane, which "
            "rasters whole-picture overlays, and the bracket holds none. That "
            "is a leg that drew nothing, not a 3D scene with nothing to draw"
            % args.scene
        )
        surf["unchecked"] = False
        surf["unchecked_kind"] = None
    if promoted:
        invalid.append(promoted)
        if not surf["geometry_met"]:
            invalid.append(
                "surface not confirmed: the app ran at %s, not the %s the leg "
                "asked for" % (surf["achieved"], surf["asked"])
            )
    elif surf["unchecked"]:
        unchecked.append("surface bytes not checked: %s" % surf["why"])
        if not surf["geometry_met"]:
            invalid.append(
                "surface not confirmed: the app ran at %s, not the %s the leg "
                "asked for" % (surf["achieved"], surf["asked"])
            )
    elif not surf["met"]:
        invalid.append("surface not confirmed: %s" % surf["why"])

    # THE VBLANK CHECK. A native leg is always panel-backed and its `--refresh`
    # IS the panel's own advertised mode, so it takes the exact branch and the
    # achieved-cadence fallback never runs here. An empty reading means xrandr
    # found no starred mode, which on 2026-09-03 meant the monitor was gone.
    refresh = refresh_verdict(panel_backed, args.refresh, args.refresh)
    if not refresh["ok"]:
        invalid.append("no vblank: %s" % refresh["why"])

    load = load_samples(args.load_file)
    quiet_max = args.quiet_max
    qv = quiet_verdict(load, quiet_max)
    quiet = qv["quiet"]
    if quiet != "yes":
        invalid.append("not quiet: %s" % qv["why"])

    # Where this leg's vector tile bodies were paid for, as a WINDOWED
    # difference over the bracket like every other running total here. It is
    # the denominator the `phase:parse` / `phase:style` families are read
    # against and is never added to them -- those are microseconds on the
    # frame thread, this is bodies. None when the log has no `tile bodies:`
    # line, which on this line means a binary older than it: the app emits it
    # unconditionally, so a leg that decoded nothing says `0 offloaded,
    # 0 inline` rather than going quiet. That asymmetry is the whole reason
    # the line exists -- the two phase families DO go quiet when the work
    # leaves the frame thread, which is exactly when a reader most needs to
    # know whether it left or never happened.
    #
    # MEASURED 2026-09-04, and the row is printing it rather than vouching for
    # it: on every native leg replayed (rd-wo28-legs/fixed-c/C.main.r1 among
    # them) this line reads `0 offloaded, 0 decoded on the frame thread` while
    # the SAME log says `tile phase (parse): n=155`, `tile phase (style):
    # n=155`, `tile take (put): n=155` and `basemap tiles: 155 vector`. 155
    # bodies were parsed and styled on the frame thread and the disposition
    # counters saw none of them. So a `0, 0` reading here is currently NOT
    # evidence that no body was decoded, and must not be quoted as one; the
    # counters and the phase families disagree, and that is a defect in the
    # ledger, not in this arm. Scraping the line is what makes the
    # disagreement visible on every row instead of nowhere at all.
    tb = diff_totals(scraped["tile_bodies"], start_idx, end_idx)
    tile_bodies = (None if tb is None
                   else {"offloaded": tb[0], "inline": tb[1]})

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
        # MEASURED, not defaulted. `--dpr` is the fallback for a leg
        # whose log carries no scale line (Wayland, macOS); the native
        # runner never passes it, so a hardcoded `dpr=1` used to be
        # printed on rows that were measured at 13/12.
        "dpr": ("%s" % scale) if scale is not None else args.dpr,
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
        # OBSERVED, flat, under the ROW line's own spellings -- so a JSON
        # reader and a ROW reader get the same figure under the same name.
        # The ceiling is under its own name. This row used to carry the
        # ceiling as `quiet_max`, the only flat field with `max` in its
        # name, and a reader quoted `loadavg_max=8.0` beside an INVALID
        # stamp that said the load reached 10.28.
        "loadavg_start": load["start"] if load else None,
        "loadavg_end": load["end"] if load else None,
        "loadavg_max": load["max"] if load else None,
        "quiet_ceiling": quiet_max,
        # The platform the leg ran on and every capability it had to do
        # without. A row measured with geometry unpinned is not the same
        # measurement as one measured with it pinned, and the matrix has to be
        # able to see which it is holding.
        "platform": args.platform,
        "degraded": [d for d in (args.degraded or "").split(",") if d],
        "windows": windows,
        "named_families": sorted(scraped["named"]),
        "window_basis": basis,
        "loops": row_loops,
        "settled": row_settled,
        "bracket": {"start_line": start_idx, "end_line": end_idx},
        "liveness": live,
        "surface": surf,
        "loop_state": (scraped["loop_state"][-1][1] if scraped["loop_state"] else None),
        "tile_bodies": tile_bodies,
        # `(line, bracket, [fifteen ints])`, or None when the log has no
        # `budget state:` line -- a binary older than the line, kept apart
        # from a live binary reporting zeroes.
        "budget_state": (scraped["budget_state"][-1] if scraped["budget_state"] else None),
        # `{role: (line, [fourteen ints])}` for the LAST reading of each cache
        # role, or None when the log has no `tile cache (...)` line -- a binary
        # older than the line, kept apart from a cache that recorded nothing.
        "tile_cache": tile_cache_by_role(scraped["tile_cache"]),
        "panel_backed": panel_backed,
        "refresh": refresh,
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
    # Three stamps, never co-firing: INVALID, and the two UNCHECKED kinds --
    # a line the binary predates, and a scene that drew no overlay picture.
    # "Never co-firing" is about the SURFACE check's three outcomes, which are
    # three answers to one question. An INVALID raised elsewhere -- a loud box,
    # a dead panel -- can and should stand beside a surface UNCHECKED: the
    # recorded scene B legs of 2026-09-03 have no bytes to check AND no vblank,
    # and suppressing either reading would hide a real fact about the leg.
    if not row.get("unchecked"):
        unchecked_banner = ""
    elif (row["surface"] or {}).get("unchecked_kind") == "no_pictures":
        unchecked_banner = "  ** UNCHECKED: scene drew no overlay pictures **"
    else:
        unchecked_banner = "  ** UNCHECKED: overlay pictures line absent **"
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
            ("  ** INVALID **" if row["invalid"] else "") + unchecked_banner,
        )
    )
    s = row["surface"]
    reported_panes = s.get("reported_panes", s.get("panes"))
    print(
        "ROW   surface asked=%s achieved=%s (from the %s) scale=%s panes=%s "
        "app_pictures=%s expected=%s B/picture (+-%s) observed=%s B/picture -> %s"
        % (
            s.get("asked"), s.get("achieved"), s.get("source") or "?",
            s["scale"] if s.get("scale") is not None else "absent",
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
    # Every per-family line the log carried, windowed on the same bracket.
    # Read off the row rather than off a list, so a family nobody listed is
    # a printed row and an arm that never wrote one is an absence.
    named = sorted(k for k in row["windows"] if k not in ("interact", "idle", "cadence"))
    if named:
        print(
            "ROW   families: %d per-family lines windowed on the same bracket "
            "[`sum` differenced exactly; percentiles BINNED]: %s"
            % (len(named), ", ".join(named))
        )
        for key in named:
            w = row["windows"][key] or {}
            if w.get("error"):
                print("ROW   family %-18s ERROR: %s" % (key, w["error"]))
                continue
            print(
                "ROW   family %-18s n=%-6s sum=%s us mean=%s us p50=%s us "
                "p90=%s us p99=%s us max=%s us%s"
                % (
                    key, w.get("n"), w.get("sum_us"), w.get("mean_us"),
                    w.get("p50_us"), w.get("p90_us"), w.get("p99_us"),
                    w.get("max_us"),
                    (" [%s]" % w["basis"]) if w.get("basis") else "",
                )
            )
    else:
        print(
            "ROW   families: none (no `<prefix> (<name>): n=, sum=` line in "
            "this log -- a binary older than the per-family lines, not a "
            "zero reading)"
        )
    # The loop denominators, on the scenes whose question they are. A..D run
    # with loops OFF, so their `loop state` line is all zeroes and printing it
    # beside them would be a column that means nothing there.
    if row["scene"].startswith("E") and row["loop_state"]:
        ls = row["loop_state"]
        # `shared` is pictures more than one pane holds -- a third
        # denominator beside slots and textured frames, never added to
        # `resident`.
        print(
            "ROW   loop %s panes, %s layers animating, %s frames listed, "
            "%s resident (%s in flight, %s failed); cap=%s held=%s; advance=%s us; "
            "shared=%s"
            % (ls[0], ls[1], ls[2], ls[3], ls[4], ls[5], ls[10], ls[11], ls[16], ls[17])
        )
    # The machine and the bracket the budgets came from, on every scene. A
    # LEVEL at the end of the log; `pool` is the LIVE loop pool in MiB and
    # `ceiling` the bracket's constant; `cap` is the capacity in force and
    # `source` how it was learned (0 presumed, 1 measured, 2 probed); `probe`
    # is where the browser's WebGPU probe stands (0 absent -- every native
    # log, 1 skipped, 2 pending, 3 empty, 4 found, 5 found capped); `balloon`
    # is what the loops hold above their base in MiB, a subset of `pool` and
    # never added to it. Absent when the log has no `budget state:` line: a
    # binary older than the line, printed as such and never as zeroes. `.get`
    # because a row built before the field existed reads the same way.
    bs = row.get("budget_state")
    if bs:
        _line, bracket_name, f = bs
        print(
            "ROW   budget bracket=%s rung=%s steps=%s pool=%s MiB ceiling=%s MiB "
            "vram=%s MiB ram=%s MiB declared=%s MiB threads=%s form=%s "
            "linear=%s/%s MiB cap=%s MiB source=%s probe=%s balloon=%s MiB"
            % (bracket_name, f[0], f[1], f[2], f[3], f[4], f[5], f[6], f[7],
               f[8], f[9], f[10], f[11], f[12], f[13], f[14])
        )
    else:
        print(
            "ROW   budget: n/a (no `budget state:` line in this log -- a binary "
            "older than the line, not a zero reading)"
        )
    # Where the vector tile bodies were paid for, over the same bracket. The
    # `phase:parse` / `phase:style` families above go SILENT once the work
    # leaves the frame thread -- a take family with no samples is not printed
    # -- so this count is what tells a silent phase family from a phase that
    # never ran. Never added to those families: they are microseconds, this is
    # bodies. Absent when the log has no line, which means a binary older than
    # it and never a leg that decoded nothing.
    tb = row.get("tile_bodies")
    if tb is None:
        print(
            "ROW   tile bodies: n/a (no `tile bodies:` line in this log -- a "
            "binary older than the line. The app emits it unconditionally, so "
            "this is never a leg that decoded nothing)"
        )
    else:
        print(
            "ROW   tile bodies: %s offloaded, %s decoded on the frame thread "
            "[over the bracket; the denominator the tile phase families are "
            "read against, never added to them]"
            % (tb["offloaded"], tb["inline"])
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
                "entries=%s resident_bytes=%s parsed=%s snap=%s"
                % (role, f[0], f[1], f[2], f[3], f[4], f[5], f[6], f[7], f[8],
                   f[9], f[10], f[11], f[12], f[13])
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


def _load_rows(paths):
    rows = []
    for p in paths:
        with open(p, "r", encoding="utf-8") as fh:
            rows.append(json.load(fh))
    return rows


def cmd_subject(args):
    """Adjudicate the SUBJECT of any two leg artefacts.

    Separate from `diverge` because `diverge` needs a native row's
    `throughput_interact_frames` and a browser leg has none -- and the browser
    arm is where a browser build moves. `run_measure.sh` assembles no pair at
    all, so without this subcommand there is nothing a web A/B could be run
    through.
    """
    rows = _load_rows(args.rows)
    return print_subject(subject_pin(rows[0], rows[1]), args.rows[0], args.rows[1])


def cmd_diverge(args):
    rows = _load_rows(args.rows)
    # The subject FIRST, and its refusal outranks the figure. A pair that did
    # not measure the same build has no divergence worth adjudicating: the
    # number would still print, and it is the printing that gets transcribed.
    subject_rc = print_subject(subject_pin(rows[0], rows[1]),
                               args.rows[0], args.rows[1])
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
               r.get("loadavg_max", (r.get("load") or {}).get("max")))
        )
    return subject_rc


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

    sj = sub.add_parser("subject",
                        help="did these two legs measure the same build?")
    sj.add_argument("rows", nargs=2)
    sj.set_defaults(func=cmd_subject)

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
        """A well-formed line with a sized pane and NO picture drawn in the
        bracket: the 3D-orbit row (scene B draws no whole-picture overlay
        raster). Not confirmed -- and not refused either, since 2026-09-02:
        it is the check having nothing to run on, stamped as its own kind
        of UNCHECKED, where before it was `** INVALID **` on every 3D row
        forever."""
        s = surface_check((1920, 1080), (1920, 1080), 0, 0,
                          _reported([ONE_PANE_PICTURE]), panes=1)
        self.assertFalse(s["met"])
        self.assertTrue(s["unchecked"])
        self.assertEqual(s["unchecked_kind"], "no_pictures")
        self.assertIn("no pictures were drawn", s["why"])
        self.assertNotIn("observed_picture_bytes", s)

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
        self.assertFalse(s["unchecked"], "pictures drawn but not reported is a refusal")
        self.assertIn("no pane with a picture", s["why"])
        self.assertIn("yet 10 pictures were drawn", s["why"])
        # Every pane empty and NOTHING drawn is the other 3D shape: the line
        # is the app's allocation, not its drawing, and there is nothing to
        # hold against it -- UNCHECKED, the same kind as a sized pane never
        # rastered into.
        s = surface_check((1920, 1080), (1920, 1080), 0, 0, empty, panes=2)
        self.assertFalse(s["met"])
        self.assertTrue(s["unchecked"])
        self.assertEqual(s["unchecked_kind"], "no_pictures")
        # And `n=0` keeps its meaning: a scene with no panes is another scene.
        s = surface_check((1920, 1080), (1920, 1080), 0, 0, _reported([]), panes=None)
        self.assertFalse(s["met"])
        self.assertFalse(s["unchecked"])
        self.assertIn("no pane with a picture (n=0)", s["why"])

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


def _leg_log(per_picture, pictures_line, per_loop=10):
    """A whole leg's log, four loops, `per_loop` pictures a loop at
    `per_picture` B.

    `pictures_line` is the app's `overlay pictures:` sentence, or None for a
    binary older than it. Skip 2, window 2: the bracket is loops 2..4, and
    across it the app drew 20 pictures whose mean is exactly `per_picture`
    -- or none at all at `per_loop=0`, the 3D-orbit scene's shape.
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
        pics = per_loop * k
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


def _leg_args(load_file, panes, log="", json_out="", scene="A", refresh="60"):
    # SCENE IS LOAD-BEARING NOW, not decoration. `pictures=0` is a fact about
    # scene B (one 3D pane, nothing to raster) and a refusal on scene A (a 2D
    # pane that drew nothing), so a fixture that says A while its docstring
    # says B is testing the wrong branch.
    return argparse.Namespace(
        log=log, scene=scene, script="pan-zoom-2d", commit="deadbeef",
        asked_geom=(1920, 1080), achieved_geom=None, panes=panes, dpr="1",
        refresh=refresh, adapter="unknown", panel="off", position="p1(a)",
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

    def test_a_loud_leg_carries_the_observed_maximum_under_the_maximum_name(self):
        """A smoke row's JSON was quoted as `quiet=no loadavg_max=8.0` while
        its INVALID line said the load reached 10.28. The only flat field
        with `max` in its name was `quiet_max`, and it held the CEILING
        (`--quiet-max`); the observed maximum lived only inside `load`. Now
        every flat field named as a maximum holds the observed figure, under
        the ROW line's own spelling, and the ceiling has its own name."""
        loud = os.path.join(self._tmp.name, "loud")
        with open(loud, "w", encoding="utf-8") as fh:
            for i, v in enumerate((1.0, 2.0, 10.28, 9.5, 1.5, 1.0)):
                fh.write("%d\t%s\n" % (1_000_000 + 5 * i, v))
        lines = _leg_log(ONE_PANE_PICTURE_BYTES, OVERLAY_PICTURES_ONE)
        args = _leg_args(loud, 1)
        self.assertEqual(args.quiet_max, 8.0, "the fixture's ceiling")
        row = build_row(args, scrape(lines, self.probes), self.probes)
        text = _capture(lambda: print_row(row))
        first = text.splitlines()[0]
        self.assertEqual(row["quiet"], "no")
        self.assertEqual(row["loadavg_max"], 10.28)
        self.assertEqual(row["loadavg_start"], 1.0)
        self.assertEqual(row["loadavg_end"], 1.0)
        self.assertEqual(row["quiet_ceiling"], 8.0)
        self.assertNotIn("quiet_max", row, "the ceiling under a maximum's name")
        for key, value in row.items():
            if "max" in key:
                self.assertEqual(
                    value, 10.28,
                    "flat field `%s` is named as a maximum and holds %r, not "
                    "the observed maximum" % (key, value))
        self.assertIn("loadavg_max=10.28 quiet=no", first)
        self.assertIn("** INVALID **", first)
        self.assertIn("reached 10.28 MID-LEG against a ceiling of 8.0", text)
        # And the JSON the runner keeps says the same as the ROW line.
        log = os.path.join(self._tmp.name, "loud.log")
        out = os.path.join(self._tmp.name, "loud.json")
        with open(log, "w", encoding="utf-8") as fh:
            fh.write("\n".join(lines) + "\n")
        _capture(lambda: cmd_analyze(_leg_args(loud, 1, log, out)))
        with open(out, "r", encoding="utf-8") as fh:
            j = json.load(fh)
        self.assertEqual(j["loadavg_max"], 10.28)
        self.assertEqual(j["quiet_ceiling"], 8.0)
        self.assertNotIn("quiet_max", j)

    def test_a_scene_that_draws_no_overlay_pictures_is_unchecked_not_invalid(self):
        """The peer's scene B rows: `pictures=0`, `quiet=yes`, geometry met,
        the app's line present and well-formed. Every such row read
        `** INVALID **` with exit 1, forever. Now the third stamp."""
        lines = _leg_log(ONE_PANE_PICTURE_BYTES, OVERLAY_PICTURES_ONE, per_loop=0)
        row = build_row(_leg_args(self.load, 1, scene="B"),
                        scrape(lines, self.probes), self.probes)
        text = _capture(lambda: print_row(row))
        first = text.splitlines()[0]
        self.assertEqual(row["pictures"], 0)
        self.assertEqual(row["quiet"], "yes")
        self.assertTrue(row["surface"]["geometry_met"])
        self.assertEqual(row["invalid"], [], row["invalid"])
        self.assertEqual(len(row["unchecked"]), 1)
        self.assertEqual(row["cross"], "unchecked")
        self.assertIn("pictures=0", first)
        self.assertIn("** UNCHECKED: scene drew no overlay pictures **", first)
        self.assertNotIn("line absent", first)
        self.assertNotIn("INVALID", text)
        self.assertIn("-> UNCHECKED", text)
        self.assertIn("ROW   UNCHECKED: surface bytes not checked: no pictures were "
                      "drawn in the window though the app allocates 2880x1555", text)

    def test_the_three_stamps_are_distinct_and_never_co_fire(self):
        """One case per stamp; each first line carries exactly its own
        banner and neither of the other two."""
        absent = "** UNCHECKED: overlay pictures line absent **"
        none_drawn = "** UNCHECKED: scene drew no overlay pictures **"
        invalid = "** INVALID **"
        cases = (
            (absent, ONE_PANE_PICTURE_BYTES, None, 10, "A"),
            (none_drawn, ONE_PANE_PICTURE_BYTES, OVERLAY_PICTURES_ONE, 0, "B"),
            (invalid, MODEL_ONE_PANE_BYTES, OVERLAY_PICTURES_ONE, 10, "A"),
        )
        for want, per_picture, line, per_loop, scene in cases:
            lines = _leg_log(per_picture, line, per_loop=per_loop)
            row = build_row(_leg_args(self.load, 1, scene=scene),
                            scrape(lines, self.probes), self.probes)
            first = _capture(lambda: print_row(row)).splitlines()[0]
            for banner in (absent, none_drawn, invalid):
                if banner == want:
                    self.assertIn(banner, first, "case %r" % want)
                else:
                    self.assertNotIn(banner, first, "case %r co-fired %r" % (want, banner))
            self.assertEqual(first.count("**"), 2, first)

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
        for per_picture, line, per_loop, scene, want in (
            (ONE_PANE_PICTURE_BYTES, None, 10, "A", 0),
            (ONE_PANE_PICTURE_BYTES, OVERLAY_PICTURES_ONE, 10, "A", 0),
            (ONE_PANE_PICTURE_BYTES, OVERLAY_PICTURES_ONE, 0, "B", 0),
            (ONE_PANE_PICTURE_BYTES, OVERLAY_PICTURES_ONE, 0, "A", 1),
            (MODEL_ONE_PANE_BYTES, OVERLAY_PICTURES_ONE, 10, "A", 1),
        ):
            log = os.path.join(self._tmp.name, "leg.log")
            with open(log, "w", encoding="utf-8") as fh:
                fh.write("\n".join(_leg_log(per_picture, line, per_loop)) + "\n")
            rc = [None]
            _capture(lambda: rc.__setitem__(
                0, cmd_analyze(_leg_args(self.load, 1, log, scene=scene))))
            self.assertEqual(rc[0], want, "scene=%s line=%r per_picture=%d"
                             % (scene, line, per_picture))


class VblankTests(unittest.TestCase):
    """The dead-panel gate, against the NINE REAL LEGS that straddle it.

    Every row below is the field shape of a recorded artefact, not an invented
    one: the native legs are `~/.cache/rd-wo28-legs/*` and WO-47's two own legs
    (`hz` is the xrandr reading `run_measure_native.sh` passed through
    `--refresh`), the browser legs are `~/.cache/rd-0f-*` (`hz` is
    `1000 / p50(rAF)`; the panel was never recorded on any of them, because
    `run_measure.sh` did not read one until 2026-09-04, so `panel_hz` is None).
    The panel died on 2026-09-03 at 11:34:52 and came back on 2026-09-04; the
    09-02 legs are real measurements, everything between is void, and the two
    09-04 legs were taken on the live monitor to exercise this gate in the
    direction it had never been exercised in -- the direction where it must
    STAY QUIET.
    """

    # (name, arm, gpu_mode, ff_mode, hz, panel_hz, valid?)
    LEGS = (
        # --- native, `hz` IS the panel's advertised mode ---------------------
        ("fixed-c/C.main.r1  09-02 22:47", "native", "", "", "174.96", "174.96", True),
        ("fixed-c/C.main.r2  09-02 22:49", "native", "", "", "174.96", "174.96", True),
        ("fixed-c/C.main.r3  09-02 22:51", "native", "", "", "174.96", "174.96", True),
        ("fixed-b/B.main.r1  09-03 23:40", "native", "", "", "?", "", False),
        ("prefix-b/B.prefix.r1 09-03 23:45", "native", "", "", "?", "", False),
        # WO-47, 2026-09-04 11:11, THE PANEL ALIVE. The healthy leg this gate
        # had never once been shown: `hz~174.96`, `invalid: []`, no stamp.
        ("wo47-healthy/A.main.r1 09-04 11:11", "native", "", "", "174.96", "174.96", True),
        # WO-47, 2026-09-04 11:16, the same box with `xrandr` switched to the
        # monitor's own 59.96 Hz mode and switched back. THE NEAREST HEALTHY
        # NEIGHBOUR to the refusal below: a GENUINE 60 Hz leg, one hour from a
        # void leg reading 59. Nothing in the two cadences separates them; the
        # panel reading does, which is the whole argument for requiring one.
        ("wo47-neighbour-60/A.main.r1 09-04 11:16", "native", "", "", "59.96", "59.96", True),
        # --- browser, `hz` is the ACHIEVED rAF cadence -----------------------
        # WAS TRUE UNTIL 2026-09-04, and this flip is the cost of retiring the
        # cadence threshold, recorded rather than glossed. This leg almost
        # certainly ran against the live monitor -- 175 is this box's panel --
        # but it carries no reading to say so, and "the number looks like our
        # monitor" is the encoding-one-monitor-into-the-rig reasoning the
        # threshold was written to avoid in the first place.
        ("rd-0f-encodesplit-2155/d-cr  09-02",
         "hardware", "headed-host-display", "", "175", None, False),
        ("rd-0f-encodesplit-after-0125/d-ff  09-04",
         "hardware", "", "host", "59", None, False),
    )

    def test_every_recorded_leg_classifies_correctly(self):
        for name, arm, gpu, ff, hz, panel_hz, want in self.LEGS:
            v = refresh_verdict(panel_backed_leg(arm, gpu, ff), hz, panel_hz)
            self.assertEqual(v["ok"], want, "%s -> %s" % (name, v["why"]))

    def test_the_healthy_legs_are_quiet_for_the_RIGHT_reason(self):
        """An over-fire is the expensive direction and it had never been
        observed: this gate was verified only against the case it rejects.
        Both 09-04 legs are here green ON THE PANEL BRANCH -- not by some
        accident of the cadence -- and neither carries a reason at all."""
        for name, arm, gpu, ff, hz, panel_hz, want in self.LEGS:
            if not want:
                continue
            v = refresh_verdict(panel_backed_leg(arm, gpu, ff), hz, panel_hz)
            self.assertEqual(v["panel_state"], "live", name)
            self.assertEqual(v["reasons"], [], name)
            self.assertIsNone(v["why"], name)

    def test_the_leg_a_presence_check_would_have_passed(self):
        """THE CASE THIS GATE EXISTS FOR, spelled out.

        `rd-0f-encodesplit-after-2026-09-04-0125/d-ff` reads `hz~59`. Not `?`
        -- an actual number, because Firefox does not stop when it loses the
        refresh rate, it falls back to a ~60 Hz software timer. A check spelled
        "is hz present?" passes that leg, and it is exactly as void as the ones
        reading `?`.

        It is refused, and SINCE 2026-09-04 NOT BY ITS CADENCE. The reason is
        that it carries no panel reading; the 59 is not consulted, because the
        very next test shows a genuine panel that reads 59.96.
        """
        headed = panel_backed_leg("hardware", "", "host")
        self.assertTrue(headed)
        self.assertIsNotNone(parse_hz("59"), "a presence check passes it")
        v = refresh_verdict(headed, "59", None)
        self.assertFalse(v["ok"])
        self.assertEqual(v["panel_state"], "absent")
        self.assertIn("NO PANEL READING", v["why"])
        # The fast legs of the same browser on the same box are refused for the
        # SAME reason, and that is deliberate: the rig cannot tell 165 Hz on a
        # live monitor from 165 Hz on a monitor it never looked at. What it
        # costs is stated where the rule is; what it buys is that no leg is
        # ever passed on a number that resembles this box's panel.
        for live in ("165", "166"):
            u = refresh_verdict(headed, live, None)
            self.assertFalse(u["ok"], live)
            self.assertEqual(u["panel_state"], "absent", live)
        # Record the panel and the whole family becomes exact -- fast and slow.
        for hz, panel in (("59", "60"), ("165", "165.00"), ("59", "174.96")):
            self.assertTrue(refresh_verdict(headed, hz, panel)["ok"],
                            "%s/%s" % (hz, panel))

    def test_the_other_two_void_browser_legs_read_nothing_at_all(self):
        """`rd-0f-cr-retake-0132` and `rd-0f-2point-0143`: rAF timed out, so
        `run_measure.sh` prints `hz~?`. Absent is a hard INVALID, not an
        UNCHECKED: it is not a leg with a missing field, it is a leg that
        measured no cadence."""
        headed = panel_backed_leg("hardware", "headed-host-display", "")
        for spelling in ("?", "", "-", None):
            v = refresh_verdict(headed, spelling, None)
            self.assertFalse(v["ok"], repr(spelling))
            # TWO things are wrong with these legs and the row picks neither
            # for the reader: both are collected. `why` is the panel one
            # because it is the more general failure of the run.
            self.assertIn(_WHY_NO_CADENCE, v["reasons"], repr(spelling))
            self.assertIn("NO PANEL READING", v["why"], repr(spelling))

    # ------------------------------------------------------------ Tier-2 ----

    def test_the_tier2_software_arm_is_never_touched(self):
        """THE ARM THAT STILL WORKED THE NIGHT THE PANEL DIED. Tier-2 never
        opens X, has no display and no panel, and legitimately has no refresh
        rate. Nothing this gate can be handed may redden it."""
        headed = panel_backed_leg("software", "headless-swiftshader", "")
        self.assertFalse(headed)
        # EVERY spelling this file knows, including the two the WO-47 pass
        # added: an arm with no display cannot be reddened by a rule about
        # displays, whatever the panel field happens to say.
        for hz in (None, "", "?", "-", "59", "0", "175", PANEL_NO_READER):
            for panel_hz in (None, "", "?", "-", "60", PANEL_NO_READER):
                v = refresh_verdict(headed, hz, panel_hz)
                self.assertTrue(v["ok"], "hz=%r panel_hz=%r -> %s"
                                % (hz, panel_hz, v["why"]))
                self.assertEqual(v["reasons"], [])
                self.assertIn("no display", v["basis"])

    def test_the_headless_hardware_and_xvfb_arms_are_not_touched_either(self):
        """Both present to something that is not this box's monitor, so the
        monitor's absence cannot void them."""
        for gpu, ff in (("headless-angle-egl", ""), ("", "xvfb"),
                        ("", "headless")):
            self.assertFalse(panel_backed_leg("hardware", gpu, ff),
                             "%s/%s" % (gpu, ff))
            self.assertTrue(refresh_verdict(
                panel_backed_leg("hardware", gpu, ff), "?", "")["ok"])

    def test_the_headed_modes_are_recognised_by_their_recorded_spelling(self):
        """Read off the artefacts: chromium records `binary.gpu_mode`, firefox
        records `binary.ff_mode`, and a native row has neither."""
        self.assertTrue(panel_backed_leg("hardware", "headed-host-display", ""))
        self.assertTrue(panel_backed_leg("hardware", "macos-quartz-headed", ""))
        self.assertTrue(panel_backed_leg("hardware", "", "host"))
        self.assertTrue(panel_backed_leg("native", "", ""))

    # ------------------------------------------------------- the threshold ---

    def test_a_live_panel_does_not_excuse_a_leg_with_no_cadence(self):
        """`rd-0f-cr-retake-0132`: rAF timed out. Declaring the panel says the
        display was fine; it says nothing about a leg that sampled nothing."""
        headed = panel_backed_leg("hardware", "headed-host-display", "")
        for panel_hz in (None, "174.96", "60"):
            v = refresh_verdict(headed, "?", panel_hz)
            self.assertFalse(v["ok"], repr(panel_hz))
            self.assertIn(_WHY_NO_CADENCE, v["reasons"], repr(panel_hz))
        # Under a DECLARED panel it is the only reason, so it is also `why`.
        for panel_hz in ("174.96", "60"):
            self.assertIn("no measured cadence",
                          refresh_verdict(headed, "?", panel_hz)["why"])

    def test_a_declared_60hz_panel_is_valid_at_60hz(self):
        """The limitation, and its escape hatch, both pinned. On a genuine
        60 Hz arm the fallback timer and the panel are the same number and no
        threshold separates them -- so declaring the panel (RIG_PANEL_HZ) moves
        the leg onto the exact branch, where the achieved cadence is not used
        at all and a slow app is a slow app rather than a void leg."""
        headed = panel_backed_leg("hardware", "headed-host-display", "")
        self.assertFalse(refresh_verdict(headed, "59.9", None)["ok"])
        self.assertTrue(refresh_verdict(headed, "59.9", "60")["ok"])
        # And a heavy scene on a live fast panel is likewise not void.
        self.assertTrue(refresh_verdict(headed, "31", "174.96")["ok"])

    def test_an_empty_panel_reading_refuses_however_good_the_cadence_looks(self):
        """The exact branch does not consult the cadence. An X server that
        kept a stale framebuffer after `Setting mode \"NULL\"` still reported a
        size; what it stopped reporting was a mode."""
        headed = panel_backed_leg("native", "", "")
        # `-` is NOT in this list, and that is the WO-47 correction: it was,
        # and it meant this test was asserting that a reading which was never
        # taken proved the display was dead. Its own state now.
        for spelling in ("", "?", "0", "n/a"):
            v = refresh_verdict(headed, "175", spelling)
            self.assertFalse(v["ok"], repr(spelling))
            self.assertEqual(v["panel_state"], "dead", repr(spelling))
            self.assertIn("no vblank", "no vblank: " + v["why"])
            self.assertIn("advertises no active mode", v["why"])
        self.assertEqual(refresh_verdict(headed, "175", "-")["panel_state"],
                         "absent")

    def test_the_cadence_threshold_is_gone_and_stays_gone(self):
        """RETIRED 2026-09-04. The threshold could not do the one job it was
        put there for, and its own message text said so: on a genuine 60 Hz
        arm the fallback timer and the panel are the same number.

        The pair, both halves MEASURED on this box: a real `xrandr`-selected
        59.96 Hz mode, and the void Firefox leg that read 59 with no monitor
        attached at all. No function of the cadence alone separates those, so
        the cadence alone no longer decides anything -- the reading does.
        """
        self.assertNotIn("FALLBACK_TIMER_CEILING_HZ", globals(),
                         "the retired threshold is back")
        headed = panel_backed_leg("hardware", "", "host")
        genuine_60, fallback_60 = "59.96", "59"
        # Without a reading, indistinguishable -- so BOTH are refused, and
        # neither refusal mentions a rate.
        for cadence in (genuine_60, fallback_60, "62.0", "62.1", "175"):
            v = refresh_verdict(headed, cadence, None)
            self.assertFalse(v["ok"], cadence)
            self.assertEqual(v["panel_state"], "absent", cadence)
        # WITH a reading, separated exactly, and the 60 Hz machine -- the
        # commonest refresh rate there is -- becomes quotable rather than
        # unquotable.
        self.assertTrue(refresh_verdict(headed, genuine_60, "59.96")["ok"])
        self.assertFalse(refresh_verdict(headed, fallback_60, "")["ok"])

    def test_the_three_panel_states_are_three_different_reasons(self):
        """A rig that never looked must not be reported as a dead display.

        This is the class the macOS over-fire belonged to: `run_measure.sh`
        keyed the panel read off "needs an X display", the macOS browser arm is
        headed WITHOUT one, so the rig never asked -- and every healthy Mac leg
        was then refused with a message about a monitor that was fine. The
        instance is fixed; this is the class, stamped.
        """
        headed = panel_backed_leg("native", "", "")
        seen = {}
        for spelling, want_state in (("", "dead"), ("?", "dead"),
                                     ("0", "dead"),
                                     ("-", "absent"), (None, "absent"),
                                     (PANEL_NO_READER, "no-reader")):
            v = refresh_verdict(headed, "175", spelling)
            self.assertEqual(v["panel_state"], want_state, repr(spelling))
            self.assertFalse(v["ok"], repr(spelling))
            # The DEAD message quotes the spelling it read back, so the
            # reasons are compared by their opening clause rather than whole.
            head = v["why"].split(" (read back")[0]
            seen.setdefault(want_state, head)
            self.assertEqual(seen[want_state], head, repr(spelling))
        # Three states, three DISTINCT reasons -- the property, not three
        # strings restated here where they could drift into agreement.
        self.assertEqual(len(set(seen.values())), 3, seen)
        self.assertIn("advertises no active mode", seen["dead"])
        self.assertIn("NO PANEL READING", seen["absent"])
        self.assertIn("NEVER READ A PANEL", seen["no-reader"])
        # And the one that is NOT about the monitor says so, so a lane reading
        # it does not go looking at cables.
        self.assertIn("says nothing about the monitor", seen["no-reader"])
        self.assertIn("about the RIG", seen["no-reader"])
        # `panel_state` is not the same question as `panel_backed_leg`: Tier-2
        # is exempt at the arm, long before any of this is consulted.
        self.assertTrue(refresh_verdict(
            panel_backed_leg("software", "headless-swiftshader", ""),
            "?", PANEL_NO_READER)["ok"])

    def test_every_panel_literal_the_shells_emit_is_in_this_vocabulary(self):
        """The seam, checked as a SET rather than as a mention.

        `PANEL_NO_READER` is a contract between this file and two shell scripts
        that never import it, so it is checked by text -- the same drift guard
        `test_both_runners_read_the_panel_the_same_way` is. The first cut of
        this test asked only whether the string appeared SOMEWHERE in each
        shell, and a tamper proved it worthless: misspelling one of the three
        emission sites left the other two, the substring was still found, and
        the tamper stayed green. A near-miss sentinel does not fail loudly --
        `panel_state` classifies it as `dead`, so a box with no reader reports
        a monitor failure, which is the exact defect this state exists to end.

        So: every LITERAL either shell can emit as a panel reading must be a
        spelling this file knows. A typo is not in the set.
        """
        vocabulary = {"", "-", PANEL_NO_READER}

        def read(path):
            with open(os.path.join(RIG_DIR, path), encoding="utf-8") as fh:
                return fh.read()

        def panel_fn(text, name):
            """Just the reader function's body -- `plat_window_resolve` has a
            `*) echo "" ;;` of its own and it is not a panel reading."""
            at = text.index("%s() {" % name)
            return text[at:text.index("\n}", at)]

        # EVERY SITE, COUNTED. A set alone is not enough and a tamper showed
        # why twice: misspelling one of three sites made that site stop
        # MATCHING THE SCRAPE, and dropping the never-asks stamp left the
        # sentinel visible at the other sites. Both read green on a set check.
        # The count is what makes a lost site and a lost scrape the same red.
        measure = read("run_measure.sh")
        native = read("run_measure_native.sh")
        sites = {
            # (where, expected number of literal emission sites)
            "run_measure.sh panel_refresh": (re.findall(
                r'echo "([^"$]*)"; return; \}', panel_fn(measure, "panel_refresh")), 3),
            "run_measure.sh PANEL_HZ=": (re.findall(
                r'PANEL_HZ="([^"$]*)"', measure), 2),
            "run_measure_native.sh plat_refresh": (re.findall(
                r'\*\) echo "([^"$]*)" ;;', panel_fn(native, "plat_refresh")), 1),
        }
        for where, (found, want_n) in sites.items():
            self.assertEqual(len(found), want_n,
                             "%s: scraped %d literal panel emission site(s), "
                             "expected %d (%r). Either a site was added or "
                             "removed -- classify it here -- or the scrape "
                             "stopped matching, which is an instrument failure "
                             "and never a green" % (where, len(found), want_n, found))
            self.assertLessEqual(set(found), vocabulary,
                                 "%s emits %s, which this file cannot classify"
                                 % (where, sorted(set(found) - vocabulary)))
        # And each of the three states is actually reachable from the shells.
        all_emitted = [v for found, _ in sites.values() for v in found]
        self.assertEqual({panel_state(v) for v in all_emitted},
                         {"absent", "no-reader"},
                         "the shells no longer stamp both no-reading states")
        # Four: `run_measure.sh` has no xrandr, has no system_profiler, and
        # the branch that never asks; `run_measure_native.sh` has no reader in
        # its plan. Every one of them is an arm that DID NOT LOOK.
        self.assertEqual(all_emitted.count(PANEL_NO_READER), 4, all_emitted)
        self.assertEqual(panel_state(PANEL_NO_READER), "no-reader")
        # It must not be parseable as a rate, or it would read as a live panel.
        self.assertIsNone(parse_hz(PANEL_NO_READER))
        # And every spelling in the vocabulary lands somewhere real.
        self.assertEqual({panel_state(v) for v in vocabulary},
                         {"dead", "absent", "no-reader"})

    def test_a_native_leg_on_xvfb_is_refused_and_the_escape_is_named(self):
        """MEASURED 2026-09-04: `Xvfb :77` advertises its mode as `0.00`, which
        is what a virtual framebuffer with no vblank should say. A native leg
        run on one is panel-backed by `panel_backed_leg` (the native runner
        opens a real window on whatever DISPLAY it is handed) and is therefore
        refused -- deliberately: this repo already lost a pan-lag figure to an
        Xvfb `present()` artefact. Neither runner is in CI, so nothing green
        turns red on this; an arm that wants it declares RIG_PANEL_HZ.

        A BROWSER leg on Xvfb is a different case and is exempt earlier, at
        `panel_backed_leg`, because geckodriver records `ff_mode: xvfb` and the
        rig can see that it never asked for this box's monitor.
        """
        self.assertIsNone(parse_hz("0.00"))
        self.assertFalse(refresh_verdict(panel_backed_leg("native"),
                                         "0.00", "0.00")["ok"])
        # RIG_PANEL_HZ short-circuits `plat_refresh`, so BOTH readings become
        # the declared figure -- on a native row `hz` and `panel_hz` are one
        # reading, and a test that fed them different values would be testing a
        # combination the runner cannot produce.
        self.assertTrue(refresh_verdict(panel_backed_leg("native"),
                                        "60", "60")["ok"], "RIG_PANEL_HZ=60")
        self.assertTrue(refresh_verdict(
            panel_backed_leg("hardware", "", "xvfb"), "0.00", "0.00")["ok"])

    def test_parse_hz_rejects_every_spelling_of_nothing(self):
        for nothing in (None, "", "   ", "?", "-", "n/a", "unknown", "0",
                        "0.0", "None", "abc"):
            self.assertIsNone(parse_hz(nothing), repr(nothing))
        self.assertEqual(parse_hz("174.96"), 174.96)
        self.assertEqual(parse_hz(" 60 "), 60.0)
        self.assertEqual(parse_hz("165.00Hz"), 165.0)

    # -------------------------------------------------- the two runners ------

    def test_both_runners_read_the_panel_the_same_way(self):
        """`run_measure.sh` grew its own reader; it must be the native one.

        The same drift guard `shared_row_keys()` is: two files that must agree
        are compared by their text rather than trusted to.
        """
        def xrandr_awk(path):
            with open(os.path.join(RIG_DIR, path), encoding="utf-8") as fh:
                text = fh.read()
            at = text.index("xrandr --query")
            line = text[text.index("awk", at):]
            return line[:line.index("\n")].strip().rstrip("\\").strip()
        self.assertEqual(xrandr_awk("run_measure_native.sh"),
                         xrandr_awk("run_measure.sh"))

    # ------------------------------------------------------ the seed rule ----

    def test_the_scene_seeds_say_which_scenes_raster_pictures(self):
        """Read out of `run_measure.sh`'s own seeds, never restated here.

        B and E3 are one `render: Volume` pane -- the 3D view, which draws no
        whole-picture overlay raster, which is why `pictures=0` there is a fact
        about the scene. C is three Volume panes and three 2D ones, and drew
        1068-1080 pictures on every live leg.
        """
        self.assertEqual(flat_panes_of_seed(json.loads(scene_from_shell("B"))), 0)
        self.assertEqual(flat_panes_of_seed(json.loads(scene_from_shell("E3"))), 0)
        self.assertEqual(flat_panes_of_seed(json.loads(scene_from_shell("C"))), 3)
        for scene in ("A", "D", "E1", "E2"):
            self.assertEqual(
                flat_panes_of_seed(json.loads(scene_from_shell(scene))), 1, scene)
        self.assertIs(scene_draws_pictures("B"), False)
        self.assertIs(scene_draws_pictures("C"), True)
        self.assertIsNone(scene_draws_pictures("nosuchscene"))


class VblankRowTests(unittest.TestCase):
    """The same rule where it is PRINTED: a whole log through `build_row`."""

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

    def _row(self, scene="A", refresh="60", per_loop=10):
        lines = _leg_log(ONE_PANE_PICTURE_BYTES, OVERLAY_PICTURES_ONE,
                         per_loop=per_loop)
        row = build_row(_leg_args(self.load, 1, scene=scene, refresh=refresh),
                        scrape(lines, self.probes), self.probes)
        text = _capture(lambda: print_row(row))
        return row, text.splitlines()[0], text

    def test_a_live_panel_stamps_nothing(self):
        row, first, _ = self._row(refresh="174.96")
        self.assertEqual(row["invalid"], [], row["invalid"])
        self.assertTrue(row["refresh"]["ok"])
        self.assertEqual(row["refresh"]["panel_hz"], 174.96)
        self.assertNotIn("**", first, first)

    def test_a_dead_panel_stamps_a_hard_invalid_on_the_row(self):
        """The `hz~?` legs of 2026-09-03 23:40 onward, which carried
        `invalid: []` and were read as measurements for hours."""
        row, first, text = self._row(refresh="")
        self.assertTrue(row["panel_backed"])
        self.assertFalse(row["refresh"]["ok"])
        self.assertEqual([w for w in row["invalid"] if w.startswith("no vblank")],
                         row["invalid"], row["invalid"])
        self.assertEqual(row["unchecked"], [], "a hard INVALID, not an UNCHECKED")
        self.assertIn("hz~?", first)
        self.assertIn("** INVALID **", first)
        self.assertNotIn("UNCHECKED", first)
        self.assertIn("ROW   INVALID: no vblank: the display advertises no "
                      "active mode", text)

    def test_the_analyser_exits_one_on_a_dead_panel(self):
        """`run_measure_native.sh` takes the exit code as the leg's verdict, so
        a rule that does not reach the exit code does not reach the runner."""
        log = os.path.join(self._tmp.name, "leg.log")
        with open(log, "w", encoding="utf-8") as fh:
            fh.write("\n".join(_leg_log(ONE_PANE_PICTURE_BYTES,
                                        OVERLAY_PICTURES_ONE)) + "\n")
        for refresh, want in (("174.96", 0), ("", 1), ("?", 1)):
            rc = [None]
            args = _leg_args(self.load, 1, log, refresh=refresh)
            _capture(lambda: rc.__setitem__(0, cmd_analyze(args)))
            self.assertEqual(rc[0], want, "refresh=%r" % refresh)

    def test_a_2d_scene_that_drew_nothing_is_invalid_not_unchecked(self):
        """Scene C drew 1068-1080 pictures on every live leg and 0 on every
        leg after the panel died. Zero on a scene that rasters is a leg that
        drew nothing."""
        row, first, text = self._row(scene="A", refresh="174.96", per_loop=0)
        self.assertEqual(row["pictures"], 0)
        self.assertEqual(row["unchecked"], [], "the promotion REPLACES it")
        self.assertEqual(len(row["invalid"]), 1, row["invalid"])
        self.assertIn("no pictures drawn", row["invalid"][0])
        self.assertIn("** INVALID **", first)
        self.assertNotIn("UNCHECKED", first)
        self.assertEqual(first.count("**"), 2, "the stamps still never co-fire")

    def test_a_dead_panel_stands_beside_a_surface_unchecked(self):
        """The recorded `fixed-b/B.main.r*` shape exactly: scene B has nothing
        to raster (UNCHECKED, correctly) AND ran with no vblank (INVALID). Two
        answers to two questions; neither may swallow the other."""
        row, first, text = self._row(scene="B", refresh="", per_loop=0)
        self.assertEqual(len(row["unchecked"]), 1, row["unchecked"])
        self.assertEqual(len(row["invalid"]), 1, row["invalid"])
        self.assertIn("no vblank", row["invalid"][0])
        self.assertIn("** INVALID **", first)
        self.assertIn("** UNCHECKED: scene drew no overlay pictures **", first)

    def test_a_3d_scene_that_drew_nothing_stays_unchecked(self):
        """And the branch the promotion must not swallow: scene B is one
        Volume pane and has nothing to raster."""
        row, first, _ = self._row(scene="B", refresh="174.96", per_loop=0)
        self.assertEqual(row["pictures"], 0)
        self.assertEqual(row["invalid"], [], row["invalid"])
        self.assertEqual(len(row["unchecked"]), 1)
        self.assertIn("** UNCHECKED: scene drew no overlay pictures **", first)


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


FAMILY_TICKS = (
    # (prefix, name, n per loop, sum per loop): cumulative-from-boot readings
    # at loop k are k times these. Four families the app emits, one it does
    # not (`frame zorp`), so the table is what the LOG says, not a list.
    ("frame segment", "pre", 100, 1000),
    ("frame post", "dispatch", 100, 7000),
    ("frame dispatch", "hitmap", 10, 3000),
    ("tile take", "vector", 5, 50000),
    ("frame zorp", "thing", 1, 10),
)


def _family_line(t, prefix, name, n, total):
    return (t + "%s (%s): n=%d, sum=%d us, p50=63 us, p90=63 us, p99=63 us, "
            "hist=%s" % (prefix, name, n, total, _hist_first_bin(n)))


def _family_leg_log():
    """`_leg_log` with a per-family tick before every loop marker, plus a
    `tile take (put)` family first written at loop 3 -- INSIDE the bracket
    (loops 2..4), so its left reading is absent and its window is from
    boot."""
    t = "[2026-09-02T00:00:00Z INFO  squallar_app::app::render] "
    out = []
    k = 0
    for line in _leg_log(ONE_PANE_PICTURE_BYTES, OVERLAY_PICTURES_ONE):
        if "loop complete" in line:
            k += 1
            for prefix, name, n, total in FAMILY_TICKS:
                out.append(_family_line(t, prefix, name, n * k, total * k))
            if k >= 3:
                out.append(_family_line(t, "tile take", "put", 7 + 2 * (k - 3),
                                        70 + 20 * (k - 3)))
        out.append(line)
    return out


class FamilyWindowTests(unittest.TestCase):
    """Every per-family line the log carries is windowed on the bracket.

    Before these, `windows` held `interact`, `idle` and `cadence` and nothing
    else: no `segment:`/`prepare:`/`post:`/`dispatch:` family reached the
    JSON, and the only native reading for a cut family was the last tick's
    cumulative-from-boot total -- the contamination the bracket exists to
    remove."""

    def setUp(self):
        import tempfile
        self._tmp = tempfile.TemporaryDirectory()
        self.load = os.path.join(self._tmp.name, "load")
        with open(self.load, "w", encoding="utf-8") as fh:
            for i in range(6):
                fh.write("%d\t1.0\n" % (1_000_000 + 5 * i))
        self.probes = compile_probes()
        self.lines = _family_leg_log()
        self.row = build_row(_leg_args(self.load, 1),
                             scrape(self.lines, self.probes), self.probes)
        self.text = _capture(lambda: print_row(self.row))

    def tearDown(self):
        self._tmp.cleanup()

    def test_every_per_family_line_the_log_carries_is_windowed_on_the_bracket(self):
        self.assertEqual(self.row["window_basis"], "2 whole loops, 2 skipped")
        w = self.row["windows"]
        for prefix, name, n, total in FAMILY_TICKS:
            key = named_key(prefix, name)
            self.assertIn(key, w, "family `%s (%s)` reached no window" % (prefix, name))
            self.assertNotIn("error", w[key], w[key])
            # Loops 2..4: the difference of the loop-4 and loop-2 readings.
            self.assertEqual(w[key]["n"], 2 * n, key)
            self.assertEqual(w[key]["sum_us"], 2 * total, key)
            self.assertEqual(w[key]["mean_us"], (2 * total) // (2 * n), key)
            self.assertEqual(w[key]["line"], prefix)
            # NOT the last tick's cumulative-from-boot total.
            self.assertNotEqual(w[key]["n"], 4 * n, "cumulative, not windowed")
            self.assertNotEqual(w[key]["sum_us"], 4 * total, "cumulative, not windowed")
            self.assertIn("ROW   family %-18s n=%-6s sum=%s us mean=%s us"
                          % (key, 2 * n, 2 * total, (2 * total) // (2 * n)),
                          self.text)
        expected = sorted([named_key(p, nm) for p, nm, _n, _s in FAMILY_TICKS]
                          + ["take:put"])
        self.assertEqual(self.row["named_families"], expected)
        self.assertIn("ROW   families: 6 per-family lines windowed on the same "
                      "bracket", self.text)

    def test_a_family_the_rig_never_listed_is_a_printed_row_not_a_dropped_line(self):
        """`frame zorp (thing)` is a family no probe in drive.py spells. A
        fixed name table that must equal what the app emits is the failure
        the browser side just fixed; here the table is the log."""
        self.assertIn("zorp:thing", self.row["windows"])
        self.assertEqual(self.row["windows"]["zorp:thing"]["sum_us"], 20)
        self.assertIn("ROW   family zorp:thing", self.text)

    def test_a_family_first_written_inside_the_bracket_windows_from_zero(self):
        """`tile take (put)` first appears at loop 3. Its left reading is
        absent, and absence at the LEFT bracket is a genuine zero: the
        counters start at zero. Absence at the RIGHT stays an error."""
        w = self.row["windows"]["take:put"]
        self.assertNotIn("error", w, w)
        self.assertEqual(w["n"], 9)
        self.assertEqual(w["sum_us"], 90)
        self.assertIn("boot", w["basis"])
        self.assertIn("ROW   family take:put           n=9      sum=90 us "
                      "mean=10 us", self.text)
        self.assertIn("[boot: no reading at or before the bracket start", self.text)
        # The right edge: a family whose only reading is AFTER the bracket.
        late = [Reading(95, 3, "1", "1", "1", [0] * SLOTS, sum=30)]
        self.assertIn("error", diff_window(late, 10, 90, absent_left_is_zero=True))

    def test_the_interact_and_segments_lines_are_not_per_family_lines(self):
        """Non-vacuity for the derived pattern: the families it discovers are
        the `sum`-carrying lines and nothing else -- `frame service
        (interact)` and `frame segments (interact, p99 us)` do not match it,
        so interact is windowed once, under its own name."""
        pat = self.probes["named_hist"]
        hist = ",".join(["0"] * SLOTS)
        self.assertIsNone(pat.search(
            "frame service (interact): n=7, p50=100 us, p90=200 us, "
            "p99=300 us, hist=" + hist))
        self.assertIsNone(pat.search(
            "frame segments (interact, p99 us): pre=1, pump=1, ui=1, "
            "prepare=1, finish=1, post=1; acquire n=1, p50=1 us, p99=1 us"))
        # And a family drive.py spells no probe for, in the app's shape.
        m = pat.search("[2026-09-02T00:00:00Z INFO  squallar_app::app_render] "
                       "frame ui (chrome): n=12, sum=340 us, p50=63 us, "
                       "p90=63 us, p99=63 us, hist=" + hist)
        self.assertIsNotNone(m)
        self.assertEqual((m.group(1), m.group(2), m.group(3), m.group(4)),
                         ("frame ui", "chrome", "12", "340"))
        self.assertFalse(any(k.startswith("interact") or k.startswith("service")
                             for k in self.row["named_families"]))

    def test_the_per_family_shape_is_read_out_of_drive_py_and_refused_when_it_splits(self):
        """The one pattern is DERIVED: the tail after the `(<name>): ` group
        is read from drive.py's seed probe and every sibling carrying that
        tail must put it behind a name group. A drive.py whose seed lost its
        `sum=` or whose siblings disagree is refused, not read as empty."""
        real = _read(DRIVE_PY)
        self.assertIsNotNone(named_hist_pattern(real))
        seed_line = next(l for l in real.splitlines()
                         if l.startswith("var %s = /" % NAMED_HIST_SEED))
        no_sum = real.replace(seed_line, seed_line.replace(r"sum=(\d+) us, ", ""))
        self.assertNotEqual(no_sum, real)
        with self.assertRaises(SystemExit):
            named_hist_pattern(no_sum)
        post_line = next(l for l in real.splitlines()
                         if l.startswith("var frame_post_re = /"))
        split = real.replace(post_line, post_line.replace(
            r"frame post \(([a-z0-9-]+)\): ", "frame post [a-z]+: "))
        self.assertNotEqual(split, real)
        with self.assertRaises(SystemExit):
            named_hist_pattern(split)


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
        # The named probes, plus the one per-family pattern derived from them.
        self.assertEqual(len(probes), len(PROBE_NAMES) + 1)
        self.assertIn("named_hist", probes)
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
        """A word group first, fourteen ints after -- `budget state`'s shape.

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
            "resident, 93 parsed, snap 1"
        )
        s = scrape([line], probes)
        self.assertEqual(len(s["tile_cache"]), 1)
        idx, role, figures = s["tile_cache"][0]
        self.assertEqual(role, "base")
        self.assertEqual(
            figures,
            [1001, 12, 103, 904, 15, 26, 37, 48, 59, 6000060, 71, 8000082, 93, 1],
        )
        by_role = tile_cache_by_role(s["tile_cache"])
        self.assertEqual(set(by_role), {"base"})
        self.assertEqual(by_role["base"][1][2], 103)
        self.assertIsNone(tile_cache_by_role([]))

    def test_the_budget_state_line_scrapes_into_its_own_arm(self):
        """The bracket word first, fifteen ints after -- the app's exact
        sentence, so a drift in either file reddens this before a leg is
        spent. The last four are the capacity in force, its source, the
        WebGPU probe's state and the loops' balloon; a binary older than any
        of those groups matches nothing and reads as absent, never as `cap 0`
        or `balloon 0`."""
        probes = compile_probes()
        line = (
            "[2026-09-02T00:00:00Z INFO  squallar_app::app::render] budget state: "
            "bracket desktop, rung 1, steps 3, pool 3072 MiB, ceiling 3840 MiB, "
            "vram 24576 MiB, ram 65536 MiB, declared 8192 MiB, threads 32, form 2, "
            "linear 300/700 MiB, cap 5120 2, probe 5, balloon 7 MiB"
        )
        s = scrape([line], probes)
        self.assertEqual(len(s["budget_state"]), 1)
        _idx, bracket, figures = s["budget_state"][0]
        self.assertEqual(bracket, "desktop")
        self.assertEqual(
            figures,
            [1, 3, 3072, 3840, 24576, 65536, 8192, 32, 2, 300, 700, 5120, 2, 5, 7],
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
        before_balloon = line.rsplit(", balloon", 1)[0]
        self.assertEqual(
            scrape([before_balloon], probes)["budget_state"], [],
            "a line without the balloon group matched: every group is mandatory",
        )

    def test_a_log_without_the_budget_state_line_prints_n_a_not_zero(self):
        row = _fixture_row()
        self.assertIsNone(row["budget_state"])
        text = _capture(lambda: print_row(row))
        self.assertIn("budget: n/a", text)
        self.assertNotIn("cap=0", text)
        row["budget_state"] = (
            7, "desktop",
            [1, 3, 3072, 3840, 24576, 65536, 8192, 32, 2, 300, 700, 5120, 2, 5, 7],
        )
        text = _capture(lambda: print_row(row))
        self.assertIn("budget bracket=desktop rung=1 steps=3 pool=3072 MiB", text)
        self.assertIn(
            "linear=300/700 MiB cap=5120 MiB source=2 probe=5 balloon=7 MiB", text
        )

    def test_a_log_without_the_tile_cache_line_prints_n_a_not_zero(self):
        row = _fixture_row()
        self.assertIsNone(row["tile_cache"])
        text = _capture(lambda: print_row(row))
        self.assertIn("tile cache: n/a", text)
        self.assertNotIn("tile cache (base)", text)
        row["tile_cache"] = {"base": (7, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14])}
        text = _capture(lambda: print_row(row))
        self.assertIn("tile cache (base): asks=1 restyle_asks=2 refetch_after_eviction=3", text)
        self.assertIn("parsed=13 snap=14", text)

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


class SubjectTests(unittest.TestCase):
    """The subject pin: did these two legs measure the same build?

    The fixtures are the real incident. On 2026-09-04 the Mac's installed
    Firefox moved 155.0 -> 155.0.1 at 12:12, mid-campaign and unasked. A leg
    taken after it would have measured a different browser than its
    counterpart and said nothing. (The leg FAILURES that followed had a
    different cause and are not this gate's -- see the section header.) The
    commit fixtures are verbatim strings off rows still on disk.
    """

    @staticmethod
    def _browser(version, tag="A.firefox", browser="firefox",
                 driver="geckodriver 0.37.1"):
        return {"browser": browser, "tag": tag,
                "binary": {"browser_version": version, "driver_version": driver},
                "session": {"browserVersion": version}}

    @staticmethod
    def _native(commit, label="main", scene="C"):
        return {"browser": "native", "scene": scene, "commit": commit,
                "position": "p1(%s)" % label, "binary": None}

    # ---- the defect direction ------------------------------------------

    def test_a_browser_that_updated_itself_mid_run_is_invalid(self):
        v = subject_pin(self._browser("Mozilla Firefox 155.0"),
                        self._browser("Mozilla Firefox 155.0.1"))
        self.assertEqual(v["state"], "moved")
        self.assertTrue(v["invalid"])
        self.assertIn("155.0.1", v["invalid"][0])
        self.assertFalse(v["unchecked"], "INVALID and UNCHECKED never co-fire")

    def test_two_runs_of_one_arm_on_two_commits_are_invalid(self):
        v = subject_pin(self._native("17b9c917"), self._native("5a275382"))
        self.assertEqual(v["state"], "moved")
        self.assertIn("commit moved", v["invalid"][0])

    def test_the_invalid_verdict_is_a_nonzero_return(self):
        """The banner is not the gate; the return code is.

        A refusal that only prints is a caption. `run_measure_native.sh` runs
        `diverge` from a shell loop, and a shell reads the status.
        """
        v = subject_pin(self._browser("Mozilla Firefox 155.0"),
                        self._browser("Mozilla Firefox 155.0.1"))
        text = _capture(lambda: self.assertEqual(print_subject(v), 1))
        self.assertIn("** INVALID **", text)

    # ---- the green direction: over-firing is the worse failure ----------

    def test_a_pair_on_one_browser_build_passes(self):
        v = subject_pin(self._browser("Mozilla Firefox 155.0"),
                        self._browser("Mozilla Firefox 155.0"))
        self.assertEqual(v["state"], "pinned")
        self.assertEqual(v["pinned_by"], ["browser_version"])
        self.assertEqual(_capture(lambda: print_subject(v)).count("INVALID"), 0)

    def test_a_cross_browser_comparison_is_not_a_moved_subject(self):
        """firefox against chromium is the comparison, not a defect.

        This is the over-fire that would have refused every two-browser row
        the campaign holds -- and `run_tier2.sh` runs per browser by design.
        """
        v = subject_pin(self._browser("Mozilla Firefox 154.0", browser="firefox"),
                        self._browser("Chromium 151.0.7922.173 Arch Linux",
                                      tag="A.chromium", browser="chromium"))
        self.assertEqual(v["state"], "declared")
        self.assertFalse(v["invalid"])

    def test_an_ab_pair_on_two_commits_is_the_axis_not_a_defect(self):
        """`before` vs `after` differ in commit ON PURPOSE.

        Gating commit equality unconditionally would refuse every A/B the
        native runner exists to run -- `--arm-commit LABEL=SHA` is the flag
        that makes them differ.
        """
        v = subject_pin(self._native("6e936c6a", label="before"),
                        self._native("9fcccaae", label="after"))
        self.assertEqual(v["state"], "declared")
        self.assertEqual(v["declared_by"], ["commit"])
        self.assertFalse(v["invalid"])

    def test_a_firefox_pair_is_never_refused_for_its_geckodriver(self):
        """The naming rule this table exists instead of.

        `binary.version_match` is None on every Firefox leg because
        geckodriver 0.37.1 is not Firefox 155; a rule over fields named
        `*_version` would call that a moved subject on every honest pair.
        """
        v = subject_pin(
            self._browser("Mozilla Firefox 155.0", driver="geckodriver 0.37.1"),
            self._browser("Mozilla Firefox 155.0", driver="geckodriver 0.36.0"))
        self.assertEqual(v["state"], "pinned")
        self.assertFalse(v["invalid"])

    # ---- differs is not the same finding as not recorded ---------------

    def test_two_unrecorded_fields_do_not_satisfy_a_match(self):
        """The sibling-positive property, stated as a test.

        Both rows carry nothing. Equality holds trivially and must NOT read as
        agreement: the answer is that the rig cannot vouch, which is a
        different finding with a different remedy from a mismatch.
        """
        v = subject_pin({"browser": "native"}, {"browser": "native"})
        self.assertEqual(v["state"], "unrecorded")
        self.assertFalse(v["invalid"], "an unvouchable pair is not an invalid one")
        self.assertTrue(v["unchecked"])
        self.assertEqual(v["pinned_by"], [])
        text = _capture(lambda: self.assertEqual(print_subject(v), 0))
        self.assertIn("** UNCHECKED: subject not recorded **", text)
        self.assertNotIn("INVALID", text)

    def test_the_absent_spellings_all_read_as_absent(self):
        """`unknown` is `commit_for_arm`'s honest fallback for a multi-arm run.

        Two of them are two admissions of ignorance, not a matched pair.
        """
        for spelling in ("unknown", "", "  ", "?", "-", "n/a"):
            v = subject_pin(self._native(spelling), self._native(spelling))
            self.assertEqual(v["state"], "unrecorded", spelling)

    def test_one_side_recorded_and_one_side_not_cannot_pin(self):
        v = subject_pin(self._native("17b9c917"), self._native("unknown"))
        self.assertEqual(v["state"], "unrecorded")
        self.assertFalse(v["invalid"])

    def test_a_commit_naming_two_builds_is_unresolvable_not_matched(self):
        """The false green this check would otherwise have printed.

        `gl-native-legs/A.before.r1.json` and `A.after.r1.json` both carry the
        literal string `6e936c6a-vs-9fcccaae` -- one field made to hold a
        two-arm run, which `commit_for_arm` exists to replace. Plain equality
        calls that pair MATCHED while it is the one pair on disk that most
        needs saying it cannot be vouched for.
        """
        for composite in ("6e936c6a-vs-9fcccaae", "d1ab16d9+016054bf"):
            v = subject_pin(self._native(composite, label="before"),
                            self._native(composite, label="after"))
            self.assertEqual(v["state"], "unresolvable", composite)
            self.assertFalse(v["invalid"], "unvouchable is UNCHECKED, not INVALID")
            text = _capture(lambda: print_subject(v))
            self.assertIn("** UNCHECKED: subject not resolvable **", text)

    def test_a_lane_suffix_on_one_sha_is_not_a_composite(self):
        """`59f08766-p1abl` names ONE build with a lane's tag on it.

        Read as a composite it would turn an honest pinned pair into an
        UNCHECKED one -- the over-fire direction, inside the composite rule.
        """
        self.assertFalse(subject_composite("59f08766-p1abl"))
        self.assertFalse(subject_composite("17b9c917"))
        self.assertTrue(subject_composite("6e936c6a-vs-9fcccaae"))
        v = subject_pin(self._native("59f08766-p1abl"),
                        self._native("59f08766-p1abl"))
        self.assertEqual(v["state"], "pinned")

    def test_the_verdicts_never_co_fire(self):
        """One question, one answer -- the shape `surface` already uses."""
        cases = (
            subject_pin(self._browser("Mozilla Firefox 155.0"),
                        self._browser("Mozilla Firefox 155.0.1")),
            subject_pin({"browser": "native"}, {"browser": "native"}),
            subject_pin(self._native("a1b2c3d-vs-e4f5a6b", label="x"),
                        self._native("a1b2c3d-vs-e4f5a6b", label="y")),
            subject_pin(self._native("17b9c917"), self._native("17b9c917")),
            subject_pin(self._browser("nightly"), self._browser("nightly")),
        )
        states = [c["state"] for c in cases]
        self.assertEqual(
            states,
            ["moved", "unrecorded", "unresolvable", "pinned", "misshapen"])
        for c in cases:
            self.assertFalse(c["invalid"] and c["unchecked"])

    # ---- the shape pin: equality alone is not enough --------------------

    def test_every_spelling_on_disk_today_passes_its_shape(self):
        """The shape pin, from the over-fire side FIRST.

        A shape tight enough to reject a real recorded value would turn every
        honest pair into an UNCHECKED one. These are verbatim from artefacts on
        disk: `binary.browser_version` for both desktop browsers, the bare
        `session.browserVersion` the w3c session reports, geckodriver's and
        chromedriver's `--version` first lines, and four real commits.
        """
        by_name = {f.name: f for f in SUBJECT_FIELDS}
        for value in ("Mozilla Firefox 154.0", "Mozilla Firefox 155.0.1",
                      "Chromium 151.0.7922.173 Arch Linux", "155.0"):
            self.assertFalse(by_name["browser_version"].misshapen(value), value)
        for value in ("geckodriver 0.37.1 (300705c65d1b 2026-07-17 09:25 +0000)",
                      "ChromeDriver 151.0.7922.173 (a96602f30358e9b5d)"):
            self.assertFalse(by_name["driver_version"].misshapen(value), value)
        for value in ("17b9c917", "ed569ae7", "6e936c6a-vs-9fcccaae",
                      "59f08766-p1abl"):
            self.assertFalse(by_name["commit"].misshapen(value), value)

    def test_a_field_that_changed_shape_stops_carrying_a_verdict(self):
        """Equality alone cannot see a reshaping; the shape pin can.

        Two legs recording `{'name': 'Firefox', 'version': '155.0'}` compare
        EQUAL as strings and would print a clean pin, while nothing downstream
        that reads the old spelling is reading a version at all. The rig has
        had to hand-work this one level down already: an app line keeps
        `since_boot=N us` as a literal token so prefix matches survive a change
        to the rest of the sentence.
        """
        reshaped = "{'name': 'Firefox'}"
        v = subject_pin(self._browser(reshaped), self._browser(reshaped))
        self.assertEqual(v["state"], "misshapen")
        self.assertEqual(v["pinned_by"], [])
        self.assertFalse(v["invalid"], "an unfamiliar spelling is not a defect")
        text = _capture(lambda: self.assertEqual(print_subject(v), 0))
        self.assertIn("** UNCHECKED: subject field changed shape **", text)
        self.assertIn("dotted version number", text)

    def test_the_shape_pin_says_what_the_shape_is(self):
        """A caption nobody can act on is a defect; every row names its shape."""
        for f in SUBJECT_FIELDS:
            self.assertTrue(f.shape_is, "%s pins a shape it cannot name" % f.name)
            self.assertTrue(hasattr(f.shape, "search"))

    # ---- the table itself ----------------------------------------------

    def test_the_subject_table_is_populous_and_says_where_each_is_checked(self):
        """Non-vacuity for the table, the way `shared_row_keys` has it.

        An empty table would make every pair `unrecorded` and every gate
        silent, which reads as a clean board.
        """
        names = [f.name for f in SUBJECT_FIELDS]
        self.assertIn("browser_version", names)
        self.assertIn("commit", names)
        self.assertGreaterEqual(len(names), 3)
        self.assertTrue(any(f.may_pin for f in SUBJECT_FIELDS))
        for f in SUBJECT_FIELDS:
            self.assertTrue(f.why, "%s says nothing about why" % f.name)
            self.assertIn(f.gated_when, (None, "same-browser", "same-arm"))
            rel = subject_relation({}, {})
            if f.gated_when:
                self.assertIn(f.gated_when, rel)

    def test_an_unknown_relation_can_only_relax_a_gate(self):
        """A missing field must never manufacture a refusal."""
        rel = subject_relation({}, {})
        self.assertFalse(rel["same-browser"])
        self.assertFalse(rel["same-arm"])

    def test_the_arm_label_comes_off_the_row_not_the_filename(self):
        self.assertEqual(subject_arm_label({"position": "p2(ringoff)"}), "ringoff")
        self.assertEqual(subject_arm_label({"tag": "A.firefox"}), "A.firefox")
        self.assertIsNone(subject_arm_label({"position": "-"}))

    def test_the_session_version_backstops_a_missing_binary_block(self):
        row = {"browser": "firefox", "tag": "t", "binary": None,
               "session": {"browserVersion": "155.0"}}
        v = subject_pin(row, dict(row))
        self.assertEqual(v["state"], "pinned")

    def test_geckodrivers_licence_paragraph_does_not_reach_the_row(self):
        long_driver = "geckodriver 0.37.1 (abc 2026-07-17)\n\nThe source code"
        self.assertEqual(_read_driver_version({"binary": {"driver_version": long_driver}}),
                         "geckodriver 0.37.1 (abc 2026-07-17)")

    # ---- the run-level census ------------------------------------------

    def test_a_run_whose_browser_updated_between_legs_is_caught(self):
        """The incident happened INSIDE one run, where nobody assembles a pair."""
        census = subject_census([
            ("A.firefox", self._browser("Mozilla Firefox 155.0")),
            ("B.firefox", self._browser("Mozilla Firefox 155.0")),
            ("C.firefox", self._browser("Mozilla Firefox 155.0.1")),
        ])
        self.assertEqual([c["state"] for c in census], ["moved"])

    def test_a_run_on_one_build_per_browser_is_pinned(self):
        census = subject_census([
            ("A.firefox", self._browser("Mozilla Firefox 155.0")),
            ("A.chromium", self._browser("Chromium 151.0", tag="A.chromium",
                                         browser="chromium")),
        ])
        self.assertEqual(sorted(c["browser"] for c in census),
                         ["chromium", "firefox"])
        self.assertEqual([c["state"] for c in census], ["pinned", "pinned"])

    def test_a_census_over_legs_that_recorded_nothing_is_not_a_pass(self):
        census = subject_census([("a", {"browser": "native"}),
                                 ("b", {"browser": "native"})])
        self.assertEqual([c["state"] for c in census], ["unrecorded"])

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


class WindowScaleTests(unittest.TestCase):
    """The unit a row's pixel figures are in, read from winit's own line.

    The three fixtures are the three spellings real legs of 2026-09-03/04
    wrote on this box, same display and same binary family: `1` on the WO-23c
    pair and on the `fixed-b`/`prefix-b` legs, `1.0833333333333333` on the
    six `abc-native` legs and on `fixed-c`, and nothing at all from a backend
    that does not log it.
    """

    LOG = "[2026-09-04T05:16:13Z INFO  winit::platform_impl::linux::x11::window] "

    def test_a_scale_one_leg_reads_back_one(self):
        r = window_scale([self.LOG + "Guessed window scale factor: 1"])
        self.assertEqual(r["scale"], 1.0)
        self.assertFalse(r["differ"])

    def test_a_thirteen_twelfths_leg_reads_back_its_own_fraction(self):
        """The value that made a one-pane leg draw 2880x1555 where a scale-1
        leg drew 2880x1560, and the reason a row cannot be read without it."""
        r = window_scale([self.LOG + "Guessed window scale factor: 1.0833333333333333"])
        self.assertEqual(r["scale"], 13.0 / 12.0)

    def test_a_log_without_the_line_has_no_scale(self):
        """None, not 1. A Wayland or macOS leg never says, and a default of
        one is a measurement nobody took."""
        self.assertIsNone(window_scale([self.LOG + "Guessed nothing at all"]))

    def test_two_guesses_report_the_last_and_say_they_differed(self):
        r = window_scale([
            self.LOG + "Guessed window scale factor: 1.0833333333333333",
            self.LOG + "Guessed window scale factor: 1",
        ])
        self.assertEqual(r["scale"], 1.0)
        self.assertEqual(r["seen"], [13.0 / 12.0, 1.0])
        self.assertTrue(r["differ"])


class RowScaleTests(unittest.TestCase):
    """Where the scale lands once a whole leg goes through the row."""

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

    def _row(self, scale_lines):
        lines = scale_lines + _leg_log(ONE_PANE_PICTURE_BYTES, OVERLAY_PICTURES_ONE)
        row = build_row(_leg_args(self.load, 1), scrape(lines, self.probes), self.probes)
        return row, _capture(lambda: print_row(row))

    def _guess(self, v):
        return ["[..] INFO  winit::platform_impl::linux::x11::window] "
                "Guessed window scale factor: %s" % v]

    def test_the_row_carries_the_scale_beside_the_geometry(self):
        row, text = self._row(self._guess("1.0833333333333333"))
        self.assertEqual(row["surface"]["scale"], 13.0 / 12.0)
        self.assertIn("achieved=1920x1080 (from the app) scale=1.0833333333333333",
                      text)

    def test_a_leg_that_never_said_prints_absent_rather_than_one(self):
        row, text = self._row([])
        self.assertIsNone(row["surface"]["scale"])
        self.assertIsNone(row["surface"]["scale_seen"])
        self.assertIn("(from the app) scale=absent", text)

    def test_dpr_is_the_measured_scale_not_the_flags_default(self):
        """`--dpr` defaults to 1 and the native runner never passes it, so
        every native row printed `dpr=1` -- including six legs of 2026-09-02
        that ran at 13/12 and drew 2880x1555 pictures."""
        row, text = self._row(self._guess("1.0833333333333333"))
        self.assertEqual(row["dpr"], "1.0833333333333333")
        self.assertIn("dpr=1.0833333333333333", text.splitlines()[0])
        # And the fallback survives for a leg whose log cannot say.
        row, text = self._row([])
        self.assertEqual(row["dpr"], "1")

    def test_two_guesses_in_one_leg_are_a_note_on_the_row(self):
        row, _text = self._row(self._guess("1.0833333333333333") + self._guess("1"))
        self.assertEqual(row["surface"]["scale"], 1.0)
        self.assertTrue(
            any("more than one window scale factor" in n for n in row["notes"]),
            row["notes"])


class TileBodiesTests(unittest.TestCase):
    """The count that tells a silent phase family from a phase that never ran.

    `tile phase (parse)` and `(style)` are take families, and a take family
    with no samples is not printed. Once the pump offloads they fall to n=0
    and then go quiet -- the exact moment a reader needs to know whether the
    work left the frame thread or never happened. Nothing scraped this line
    on either half of the rig.
    """

    LINE = "[..] INFO tile bodies: %d offloaded, %d decoded on the frame thread"

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

    def test_the_probe_is_drive_pys_own(self):
        """Read out of drive.py at run time, never restated here, so the two
        halves of the rig cannot come to read different lines."""
        self.assertEqual(
            drive_pattern("tile_bodies_re"),
            r"tile bodies: (\d+) offloaded, (\d+) decoded on the frame thread")

    def test_the_line_scrapes_with_every_group_mandatory(self):
        m = self.probes["tile_bodies_re"].search(self.LINE % (41, 7))
        self.assertIsNotNone(m)
        self.assertEqual([int(g) for g in m.groups()], [41, 7])
        self.assertIsNone(
            self.probes["tile_bodies_re"].search("[..] INFO tile bodies: 41 offloaded"))

    def _row(self, lines):
        row = build_row(_leg_args(self.load, 1), scrape(lines, self.probes), self.probes)
        return row, _capture(lambda: print_row(row))

    def test_the_row_windows_the_running_total_over_the_bracket(self):
        """A running total, differenced across the bracket like every other
        one here -- not the last level, which would carry the whole process."""
        lines = _leg_log(ONE_PANE_PICTURE_BYTES, OVERLAY_PICTURES_ONE)
        out = []
        seen = 0
        for line in lines:
            out.append(line)
            if "gesture script pan-zoom-2d loop complete" in line:
                seen += 1
                out.append(self.LINE % (10 * seen, seen))
        row, text = self._row(out)
        self.assertIsNotNone(row["tile_bodies"])
        self.assertGreater(row["tile_bodies"]["offloaded"], 0)
        self.assertIn("ROW   tile bodies: ", text)
        self.assertIn("offloaded", text)

    def test_a_binary_older_than_the_line_says_so_rather_than_zero(self):
        """None, and the row says which. The app emits this line
        unconditionally, so an absent line is never a leg that decoded
        nothing -- and printing `0 offloaded` for it would be the exact
        reading the line was added to make impossible."""
        row, text = self._row(_leg_log(ONE_PANE_PICTURE_BYTES, OVERLAY_PICTURES_ONE))
        self.assertIsNone(row["tile_bodies"])
        self.assertIn("ROW   tile bodies: n/a", text)
        self.assertIn("binary older than the line", text)
        self.assertNotIn("ROW   tile bodies: 0 offloaded", text)


class OffFrameEvictionTests(unittest.TestCase):
    """`rasterization_ms.by_kind == {}` must never read as zero.

    It is the only family the web half reads from PER-EVENT lines, and
    `__rig_console` is a 1200-entry ring the app's per-frame telemetry evicts.
    The eviction rate follows the LOG rate, which is why an empty family looks
    browser-correlated when it is an instrument artifact.
    """

    def test_drive_py_cross_checks_an_empty_family_against_the_running_total(self):
        text = _read(DRIVE_PY)
        self.assertIn('"why_empty": _why_empty,', text,
                      "drive.py no longer records WHY the off-frame family was "
                      "empty, so `{}` reads as zero again")
        self.assertIn('_replies = ((_sig.get("transport") or {}).get("replies"))', text,
                      "drive.py no longer cross-checks the evictable per-event "
                      "family against `transport:`, the page's own running "
                      "total -- which is the only reading that can tell "
                      "eviction from absence")
        self.assertIn("not seen, NOT zero", text)
        self.assertIn('"evictable": True,', text)


class ConsoleExportTests(unittest.TestCase):
    """An absence in the exported console must not read as an absence in the log.

    The rig used to ship `console_tail`, the last SIXTY entries, out of a ring
    holding up to 1200 -- and on three of four `huge` passes measured
    2026-09-04 the ring had never even filled, so nothing was evicted and the
    export threw away 87-95% of what the ring held for nothing. A reader then
    took that tail, found no line from a page-pressure arm, and reported the
    arm never fired; the window was 4.8 s of a 50 s leg. Two things stop that
    recurring and both are pinned here: the export is the WHOLE ring where it
    fits, and every windowed export says what fraction it is.
    """

    def test_the_probe_exports_the_ring_not_a_sixty_entry_tail(self):
        text = _read(DRIVE_PY)
        # The newline terminators are load-bearing: `"console: keep"` alone
        # is satisfied by `console: keep.slice(-60)`, which is the regression.
        self.assertIn("\n  console: keep,\n", text,
                      "RIG_ERRORS_PROBE no longer exports the fuller console "
                      "record, so the artifact is back to a 60-entry tail")
        self.assertIn(
            "for (var i = C.length - 1; i >= 0 && keep.length < CAP; i--) {",
            text,
            "the export loop no longer walks the whole ring up to CAP")
        self.assertIn("var CAP = %d, BUDGET = %d;", text,
                      "the probe's bounds are no longer built from "
                      "CONSOLE_RING_ENTRIES / CONSOLE_EXPORT_BYTE_BUDGET")
        self.assertIn("CONSOLE_RING_ENTRIES = 1200", text)
        self.assertIn("CONSOLE_EXPORT_BYTE_BUDGET = 2_000_000", text)

    def test_the_old_keys_keep_their_old_meanings(self):
        """`console_tail` is 60 and `console_total` is the ring length.

        Readers depend on both. The fuller record was added BESIDE them; if a
        later change repoints either, every existing reading silently changes
        denominator."""
        text = _read(DRIVE_PY)
        self.assertIn("\n  console_tail: C.slice(-60),\n", text)
        self.assertIn("\n  console_total: C.length,\n", text)

    def test_every_windowed_export_carries_a_window_record(self):
        text = _read(DRIVE_PY)
        for key in ('result["rig_signal"]["console_window"] = _cw',
                    'result["rig_signal"]["errors_window"] = _ew',
                    '"window": export_window('):
            self.assertIn(key, text,
                          "a windowed export lost its legibility record: %s"
                          % key)

    def test_the_window_record_says_what_fraction_of_the_leg_it_covers(self):
        """`covers` and `complete`, computable from the JSON alone.

        The reading this exists for is "this window is 4.8 s of a 50 s leg" --
        the artifact must carry it, not leave it to be re-derived from
        timestamps a reader may not have."""
        text = _read(DRIVE_PY)
        self.assertIn('rec["covers"] = "; ".join(parts)', text)
        self.assertIn('"complete": bool(source_len is not None', text)
        self.assertIn('covering %.1f s of a %.1f s leg (%.0f%%)', text)

    def test_a_full_ring_is_not_a_complete_log(self):
        """The ring's length SATURATES at its cap, so the eviction count is
        what makes it readable. Without it, 1200-of-1200 reads whole."""
        self.assertIn("arr.evicted = (arr.evicted || 0) + drop;",
                      _read(SERVE_PY),
                      "serve.py's prelude no longer counts what the rings "
                      "evict, so `console_total` saturates with nothing beside "
                      "it to say how many lines existed")
        text = _read(DRIVE_PY)
        self.assertIn("console_ring_evicted: C.evicted || 0,", text)
        self.assertIn('lost_before_export=sig.get("console_ring_evicted") or 0',
                      text)

    def test_the_ring_stays_a_page_memory_bound(self):
        """1200 is a bound on the page under measurement, not a display
        window. Growing it spends the heap of the leg being measured -- and
        the scene this rig runs hardest is the one that traps at 1 GiB."""
        self.assertIn("1200 entries, and it STAYS 1200", _read(SERVE_PY))


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
        "quiet": "yes",
        "loadavg_start": 1.0, "loadavg_end": 1.2, "loadavg_max": 1.4,
        "quiet_ceiling": 8.0,
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
