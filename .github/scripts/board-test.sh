#!/usr/bin/env bash
# The workspace test board, spelled so it cannot report a total it did not
# measure.
#
# It exists because the spelling it replaces could not. That one was
#
#   cargo test --workspace -j 4 2>&1 | grep -E "^test result" \
#     | awk '{s+=$4; f+=$6} END {print "SUITES:",NR," passed:",s," failed:",f}'
#
# and it has two defects that compound into a green board over a run that
# stopped two thirds of the way through.
#
#   1. The pipeline throws cargo's exit code away. A shell reports a
#      pipeline's status from its LAST command, `pipefail` is off in both
#      bash and zsh by default, and `awk` always succeeds -- so `cargo`
#      failing, being killed, or timing out all read as 0. Measured on this
#      tree: a suite that aborts mid-run gives PIPELINE_EXIT=0.
#
#   2. It sums `passed` but never reports a denominator. `cargo test`
#      fail-fasts by default: the first test binary that fails or dies stops
#      the run, and every later binary is never executed. The awk then sums
#      what it saw and prints a smaller number with no indication that
#      anything is missing.
#
# Measured 2026-09-07 on a4fca342, one deliberately broken suite planted in
# `squallar-overlays/tests/` (removed again), old spelling, same tree:
#
#     clean          SUITES: 161  passed: 6618  failed: 0   PIPELINE_EXIT=0
#     one failure    SUITES:  97  passed: 4870  failed: 1   PIPELINE_EXIT=0
#     one abort      SUITES:  96  passed: 4870  failed: 0   PIPELINE_EXIT=0
#
# The last row is the shape that was read as a flaky total: 1,748 tests
# absent, nothing failed, exit 0. It is not a flake and the count does not
# vary -- three consecutive untouched runs of this workspace produce
# byte-identical per-suite counts.
#
# What this script does differently:
#
#   * cargo's status is read from cargo, never through a pipe.
#   * `--no-fail-fast`, so one dead suite cannot take the other 60 with it.
#   * every figure is printed with its denominator, and the run is refused
#     unless `running N tests` headers and `test result:` lines pair up 1:1.
#     That pairing is the pin-free check: libtest prints exactly one of each
#     per harness invocation, so a binary that dies without printing its
#     summary shows up as a header with no result. It needs no expected
#     total, so it does not rot as tests land.
#
# The pairing covers three distinct hazards. Only the first two were in mind
# when it was written; anyone simplifying it away deletes all three.
#
#   1. fail-fast -- the first binary to fail or die stops the run, and every
#      later binary never executes.
#   2. a swallowed exit status -- `| grep | awk` replaces cargo's code with
#      awk's zero. That is the defect this script replaced.
#   3. mid-run truncation -- the run is killed from OUTSIDE cargo partway
#      through: a CI step timeout, an OOM kill, or a wall-clock cap on the
#      command. Measured 2026-09-07: a 3600 s cap reaped a board mid-run.
#
# All three surface identically -- a `running N tests` header with no matching
# `test result:` line -- because the check is derived from the SHAPE of
# libtest's output (exactly one header and one result per harness invocation)
# and not from an expected total. That is why it caught the third without
# anyone knowing the hazard existed. A check pinned to a total catches a
# truncation only if someone remembers to lower the expectation.
#
# Usage: .github/scripts/board-test.sh [extra cargo args...]
#        LOG=path .github/scripts/board-test.sh   # keep the full log

set -uo pipefail

log=${LOG:-$(mktemp -t squallar-board-XXXXXX.log)}
keep=${LOG:+1}

CARGO_TERM_COLOR=never cargo test --workspace --no-fail-fast "$@" >"$log" 2>&1
cargo_status=$?

awk -v status="$cargo_status" -v logfile="$log" '
  /^running [0-9]+ tests?$/ { headers++; selected += $2; next }
  /^test result:/ {
    results++
    for (i = 1; i <= NF; i++) {
      if ($i == "passed;")   passed  += $(i-1)
      if ($i == "failed;")   failed  += $(i-1)
      if ($i == "ignored;")  ignored  += $(i-1)
      if ($i == "measured;") measured += $(i-1)
      if ($i == "out;")      filtered += $(i-2)
    }
    next
  }
  END {
    printf "cargo exit      %d\n", status
    printf "suites          %d started, %d finished\n", headers, results
    printf "tests selected  %d\n", selected
    printf "passed          %d\n", passed
    printf "failed          %d\n", failed
    printf "ignored         %d\n", ignored
    printf "measured        %d\n", measured
    printf "filtered out    %d\n", filtered
    bad = 0
    if (headers != results) {
      printf "REFUSED: %d suite(s) started and never printed a result -- a test binary died without a summary. Log: %s\n", headers - results, logfile
      bad = 1
    }
    # `running N tests` counts the tests left AFTER filtering, so `filtered
    # out` is NOT one of its terms -- adding it refused a healthy filtered run.
    # `measured` is the opposite sign: bench mode counts it INSIDE that N.
    if (selected != passed + failed + ignored + measured) {
      printf "REFUSED: selected %d != passed+failed+ignored+measured %d. Log: %s\n", selected, passed + failed + ignored + measured, logfile
      bad = 1
    }
    if (failed > 0) {
      printf "REFUSED: %d test(s) failed. Log: %s\n", failed, logfile
      bad = 1
    }
    if (status != 0) {
      printf "REFUSED: cargo exited %d. Log: %s\n", status, logfile
      bad = 1
    }
    if (bad) exit 1
    print "BOARD GREEN"
  }
' "$log"
verdict=$?

[ -n "$keep" ] || { [ "$verdict" -eq 0 ] && rm -f "$log"; }
exit "$verdict"
