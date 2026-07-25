#!/usr/bin/env bash
# Reduces an LCOV report to one `path<TAB>lines_found<TAB>lines_hit` row per
# file. llvm-cov emits absolute SF paths, so they are made relative to the
# repository root to stay comparable across machines and runs.
#
# Usage: coverage-summary.sh <lcov.info> [repo-root]

set -euo pipefail

lcov=${1:?usage: coverage-summary.sh <lcov.info> [repo-root]}
root=${2:-$PWD}

awk -v root="${root%/}/" '
  /^SF:/ {
    file = substr($0, 4)
    # Plain prefix strip rather than sub(), so regex metacharacters in the
    # workspace path cannot corrupt the result.
    if (index(file, root) == 1) file = substr(file, length(root) + 1)
    next
  }
  /^LF:/           { lf = substr($0, 4); next }
  /^LH:/           { lh = substr($0, 4); next }
  /^end_of_record/ {
    if (file != "") print file "\t" lf "\t" lh
    file = ""; lf = 0; lh = 0
  }
' "$lcov" | LC_ALL=C sort
