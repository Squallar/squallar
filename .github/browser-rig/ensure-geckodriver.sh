#!/usr/bin/env bash
#
# ensure-geckodriver.sh -- durable geckodriver provisioning for the browser
# gate (Tier 1 `wasm-pack test --firefox`, Tier 2 `run_tier2.sh`).
#
# Prints exactly one line on stdout -- the path to a geckodriver binary at the
# pinned version -- so callers can `GECKODRIVER=$(ensure-geckodriver.sh)`.
# Diagnostics go to stderr. Idempotent: a cached binary already reporting the
# pinned version is reused; anything else (absent, wrong version, not
# executable) is re-provisioned from the mozilla/geckodriver GitHub release.
#
# The version and tarball sha256 below are the ONLY constants in this file.
# The sha256 was computed from the tarball verified working with this rig on
# 2026-08-18; a mismatch is a HARD FAILURE (a changed release asset is a
# supply-chain event), never a warning.

set -u -o pipefail

GECKODRIVER_VERSION="0.37.1"
GECKODRIVER_SHA256="e815130ea95983e162ae91843b48d3a3ce991735635fce83a647afde21e09f7e"

BIN="${GECKODRIVER_BIN:-$HOME/.cache/squallar-ci/geckodriver-$GECKODRIVER_VERSION/geckodriver}"

version_ok() {
  [ -x "$1" ] &&
    "$1" --version 2>/dev/null | head -n 1 |
    grep -q "geckodriver $GECKODRIVER_VERSION"
}

if version_ok "$BIN"; then
  echo "ensure-geckodriver: reusing cached $BIN" >&2
  printf '%s\n' "$BIN"
  exit 0
fi

DIR="$(dirname -- "$BIN")"
mkdir -p "$DIR" || { echo "FATAL: cannot create $DIR" >&2; exit 1; }

URL="https://github.com/mozilla/geckodriver/releases/download/v$GECKODRIVER_VERSION/geckodriver-v$GECKODRIVER_VERSION-linux64.tar.gz"
TARBALL="$DIR/geckodriver-v$GECKODRIVER_VERSION-linux64.tar.gz"

echo "ensure-geckodriver: downloading $URL" >&2
if command -v curl >/dev/null 2>&1; then
  curl -fsSL --retry 3 -o "$TARBALL" "$URL" ||
    { echo "FATAL: download failed: $URL" >&2; exit 1; }
elif command -v wget >/dev/null 2>&1; then
  wget -q -O "$TARBALL" "$URL" ||
    { echo "FATAL: download failed: $URL" >&2; exit 1; }
else
  echo "FATAL: neither curl nor wget on PATH" >&2
  exit 1
fi

GOT="$(sha256sum "$TARBALL" | cut -d' ' -f1)"
if [ "$GOT" != "$GECKODRIVER_SHA256" ]; then
  echo "FATAL: geckodriver tarball sha256 mismatch (hard failure):" >&2
  echo "  expected $GECKODRIVER_SHA256" >&2
  echo "  got      $GOT" >&2
  rm -f "$TARBALL"
  exit 1
fi

tar -xzf "$TARBALL" -C "$DIR" geckodriver ||
  { echo "FATAL: could not extract geckodriver from $TARBALL" >&2; exit 1; }
rm -f "$TARBALL"

# A caller-overridden $GECKODRIVER_BIN may name the binary something else;
# the tarball member is always `geckodriver`.
if [ "$(basename -- "$BIN")" != "geckodriver" ]; then
  mv "$DIR/geckodriver" "$BIN"
fi
chmod +x "$BIN"

version_ok "$BIN" ||
  { echo "FATAL: extracted binary does not report geckodriver $GECKODRIVER_VERSION" >&2; exit 1; }

echo "ensure-geckodriver: provisioned $BIN" >&2
printf '%s\n' "$BIN"
