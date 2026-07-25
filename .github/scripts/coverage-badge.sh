#!/usr/bin/env bash
# Renders a flat coverage badge SVG from an LCOV report.
#
# Line coverage is summed across every file in the report: LF records count
# instrumented lines, LH records the ones that were hit.
#
# Usage: coverage-badge.sh <lcov.info> <output.svg>

set -euo pipefail

lcov=${1:?usage: coverage-badge.sh <lcov.info> <output.svg>}
out=${2:?usage: coverage-badge.sh <lcov.info> <output.svg>}

pct=$(awk -F: '
  /^LF:/ { found += $2 }
  /^LH:/ { hit   += $2 }
  END    { printf "%.1f", found ? hit * 100 / found : 0 }
' "$lcov")

color=$(awk -v p="$pct" 'BEGIN {
  print (p >= 80) ? "#4c1" : (p >= 60) ? "#dfb317" : "#e05d44"
}')

label="coverage"
message="${pct}%"

# 7px per character at font-size 11 approximates DejaVu Sans closely enough.
label_w=$(( ${#label} * 7 + 10 ))
msg_w=$(( ${#message} * 7 + 10 ))
total_w=$(( label_w + msg_w ))
label_x=$(( label_w / 2 ))
msg_x=$(( label_w + msg_w / 2 ))

mkdir -p "$(dirname "$out")"
cat > "$out" <<SVG
<svg xmlns="http://www.w3.org/2000/svg" width="${total_w}" height="20" role="img" aria-label="${label}: ${message}">
  <title>${label}: ${message}</title>
  <linearGradient id="s" x2="0" y2="100%">
    <stop offset="0" stop-color="#bbb" stop-opacity=".1"/>
    <stop offset="1" stop-opacity=".1"/>
  </linearGradient>
  <clipPath id="r">
    <rect width="${total_w}" height="20" rx="3" fill="#fff"/>
  </clipPath>
  <g clip-path="url(#r)">
    <rect width="${label_w}" height="20" fill="#555"/>
    <rect x="${label_w}" width="${msg_w}" height="20" fill="${color}"/>
    <rect width="${total_w}" height="20" fill="url(#s)"/>
  </g>
  <g fill="#fff" text-anchor="middle" font-family="Verdana,DejaVu Sans,Geneva,sans-serif" font-size="11">
    <text x="${label_x}" y="15" fill="#010101" fill-opacity=".3">${label}</text>
    <text x="${label_x}" y="14">${label}</text>
    <text x="${msg_x}" y="15" fill="#010101" fill-opacity=".3">${message}</text>
    <text x="${msg_x}" y="14">${message}</text>
  </g>
</svg>
SVG

echo "Line coverage: ${message}"
