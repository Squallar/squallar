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
# JS and Python flavours agree on all of `\d`, `\(`, `[0-9,]+`, `[a-z0-9-]+`.
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
NATIVE_ONLY_ROW_KEYS = (
    "loadavg_start",
    "loadavg_end",
    "loadavg_max",
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
        "segments": [],
        "begins": [],
        "loops": [],
        "backend": None,
        "adapter": None,
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


# ------------------------------------------------------------------ surface --


def picture_bytes_for(w, h):
    """The whole-picture overlay raster's size at a WxH surface.

    `(W * 1.5) * ((H - 40) * 1.5) * 4` -- the 1.5 is OVERDRAW_FRACTION 0.25
    spent on both sides, the 40 is the top bar in points, the 4 is RGBA.
    Verified exact at three surfaces on 2026-08-31.
    """
    return int(w * 1.5) * int((h - 40) * 1.5) * 4


def surface_check(asked, achieved, pictures, picture_bytes):
    """Does the picture the app actually drew match the window it was given?

    Geometry is READ BACK, never requested and trusted, and then checked
    against the bytes -- because a window manager that silently ran a leg at
    3440x1440 instead of 1920x1080 was caught by exact factorization of the
    picture totals and by nothing else. A leg whose surface cannot be
    confirmed is refused rather than reported.
    """
    out = {
        "asked": "%dx%d" % asked if asked else None,
        "achieved": "%dx%d" % achieved if achieved else None,
        "geometry_met": bool(asked and achieved and asked == achieved),
    }
    if not achieved:
        out["met"] = False
        out["why"] = "no window geometry was read back"
        return out
    expected = picture_bytes_for(*achieved)
    out["expected_picture_bytes"] = expected
    if not pictures:
        out["met"] = False
        out["why"] = (
            "no pictures were drawn in the window, so the surface cannot be "
            "confirmed from the bytes"
        )
        return out
    observed = picture_bytes / float(pictures)
    out["observed_picture_bytes"] = observed
    # Exact is the expectation; the tolerance only absorbs a ratio taken over
    # pictures of one size counted in whole bytes.
    out["bytes_met"] = abs(observed - expected) <= 1.0
    if not out["bytes_met"]:
        implied = implied_surface(observed)
        out["why"] = (
            "picture bytes say the surface was %s, not the %s the window "
            "reported: %d B/picture observed against %d B expected"
            % (implied or "some other size", out["achieved"], observed, expected)
        )
    out["met"] = bool(out["geometry_met"] and out["bytes_met"])
    return out


def implied_surface(observed_bytes):
    """A WxH whose picture bytes are `observed_bytes`, if a common one fits.

    Names the size the leg REALLY ran at instead of only saying the check
    failed -- that is the difference between "something is wrong" and "the
    window manager gave you 3440x1440".
    """
    common = [
        (1920, 1080), (3440, 1440), (2560, 1440), (1280, 900), (1280, 800),
        (1680, 1050), (1600, 900), (3840, 2160), (2560, 1600), (1440, 900),
    ]
    for w, h in common:
        if abs(picture_bytes_for(w, h) - observed_bytes) <= 1.0:
            return "%dx%d" % (w, h)
    return None


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
    br, why = bracket(scraped["loops"], skip, win)
    if br is None:
        # A gestureless leg (scene E1) has no markers by design and takes the
        # whole log as its bracket, saying so on the row.
        if args.script == "none":
            first = scraped["interact"][0].idx if scraped["interact"] else 0
            last = scraped["interact"][-1].idx if scraped["interact"] else 0
            br = (first, last)
            basis = "unbracketed (E-scene, no gesture): whole log"
        else:
            invalid.append("no whole-loop bracket: %s" % why)
            br = (0, len(scraped["interact"]) and scraped["interact"][-1].idx or 0)
            basis = "UNBRACKETED FALLBACK -- not a window figure"
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

    surf = surface_check(args.asked_geom, args.achieved_geom, pictures, picture_bytes)
    if not surf["met"]:
        invalid.append(
            "surface not confirmed: %s" % surf.get("why", "geometry did not match")
        )

    load = load_samples(args.load_file)
    quiet_max = args.quiet_max
    if load is None:
        quiet = "unknown"
        invalid.append(
            "no load samples: a leg with no during-leg load record cannot be "
            "stamped quiet, and a start-of-leg gate would not have seen a "
            "compile that began after it passed"
        )
    else:
        quiet = "yes" if load["max"] < quiet_max else "no"

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

    aw, ah = args.achieved_geom if args.achieved_geom else (0, 0)
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
        "cross": "yes" if surf["met"] else "no",
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
        "quiet_max": quiet_max,
        "windows": windows,
        "window_basis": basis,
        "bracket": {"start_line": start_idx, "end_line": end_idx},
        "liveness": live,
        "surface": surf,
        "loop_state": (scraped["loop_state"][-1][1] if scraped["loop_state"] else None),
        "gpu_unavailable": scraped["gpu_unavailable"],
        "throughput_interact_frames": throughput,
        "percentiles_clamped": clamped,
        "invalid": invalid,
        "notes": notes,
    }


def print_row(row):
    """The ROW line. Shared columns first, in `run_measure.sh`'s order and
    spelling, then the native-only ones. A native row and a web row are meant
    to be read in one table; a column that drifted would silently become two
    tables."""
    load = row["load"] or {}
    print(
        "ROW scene=%s browser=%s arm=%s adapter=%s backend=%s "
        "viewport=%s px=%s dpr=%s cross=%s hz~%s coi=%s panel=%s "
        "script=%s basemap=%s pictures=%s MB/picture=%s commit=%s "
        "loadavg_start=%s loadavg_end=%s loadavg_max=%s quiet=%s position=%s%s"
        % (
            row["scene"], row["browser"], row["arm"], row["adapter"],
            row["backend"], row["viewport"], row["px"], row["dpr"],
            row["cross"], row["hz"], row["coi"], row["panel"], row["script"],
            row["basemap"], row["pictures"], row["mb_per_picture"],
            row["commit"],
            load.get("start", "?"), load.get("end", "?"), load.get("max", "?"),
            row["quiet"], row["position"],
            "" if not row["invalid"] else "  ** INVALID **",
        )
    )
    s = row["surface"]
    print(
        "ROW   surface asked=%s achieved=%s expected=%s B/picture "
        "observed=%s B/picture -> %s"
        % (
            s.get("asked"), s.get("achieved"), s.get("expected_picture_bytes"),
            ("%.0f" % s["observed_picture_bytes"])
            if s.get("observed_picture_bytes") is not None else "-",
            "CONFIRMED" if s.get("met") else "REFUSED",
        )
    )
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
    if row["gpu_unavailable"]:
        print("ROW   gpu passes: unavailable (adapter lacks TIMESTAMP_QUERY)")
    for n in row["notes"]:
        print("ROW   NOTE: %s" % n)
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
    a.add_argument("--json", default="")
    a.set_defaults(func=cmd_analyze)

    d = sub.add_parser("diverge", help="adjudicate a run pair")
    d.add_argument("rows", nargs=2)
    d.set_defaults(func=cmd_diverge)

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


class SurfaceTests(unittest.TestCase):
    def test_the_three_verified_surfaces(self):
        """Verified exact on 2026-08-31; see run_measure.sh's header."""
        self.assertEqual(picture_bytes_for(1920, 1080), 17_971_200)
        self.assertEqual(picture_bytes_for(1280, 779), 8_509_440)
        self.assertEqual(picture_bytes_for(1248, 714), 7_570_368)

    def test_a_wrong_window_is_refused_and_named(self):
        """The failure that actually happened: a title-substring `wmctrl -r`
        ran legs at 3440x1440 while the leg believed it asked for 1920x1080."""
        real = picture_bytes_for(3440, 1440)
        s = surface_check((1920, 1080), (1920, 1080), 10, real * 10)
        self.assertFalse(s["met"])
        self.assertIn("3440x1440", s["why"])

    def test_a_matching_surface_is_confirmed(self):
        b = picture_bytes_for(1920, 1080)
        s = surface_check((1920, 1080), (1920, 1080), 7, b * 7)
        self.assertTrue(s["met"])

    def test_geometry_alone_does_not_confirm(self):
        s = surface_check((1920, 1080), (1920, 1080), 0, 0)
        self.assertFalse(s["met"])


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


def _fixture_row(clamp=False):
    b = picture_bytes_for(1920, 1080)
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
        "cross": "yes", "hz": "60", "coi": "n/a", "panel": "off",
        "script": "pan-zoom-2d", "basemap": "some-decoded/some-placed",
        "pictures": 10, "mb_per_picture": "17.97", "commit": "deadbeef",
        "position": "A1",
        "load": {"start": 1.0, "end": 1.2, "max": 1.4, "samples": 9},
        "quiet": "yes", "quiet_max": 8.0,
        "windows": {"interact": w, "idle": dict(w), "cadence": dict(w)},
        "window_basis": "2 whole loops, 2 skipped",
        "bracket": {"start_line": 10, "end_line": 90},
        "liveness": {"ok": True, "grew_by": 40, "verdict": "interact frames "
                     "still rising (+40 after the window)"},
        "surface": surface_check((1920, 1080), (1920, 1080), 10, b * 10),
        "loop_state": None, "gpu_unavailable": False,
        "throughput_interact_frames": 1234,
        "percentiles_clamped": clamp,
        "invalid": [],
        "notes": (["p99 CLAMPED at the over-64ms bin: quote `interact n=1234` "
                   "as this leg's throughput figure, not the percentiles"]
                  if clamp else []),
    }


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
