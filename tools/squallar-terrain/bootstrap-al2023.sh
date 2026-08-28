#!/usr/bin/env bash
# Toolchain for the terrain build on Amazon Linux 2023.
#
# Three facts drive everything here, all verified against AL2023 repo metadata
# rather than recalled:
#
#  1. The package is `gdal310`, NOT `gdal`. No gdal310 package Provides a bare
#     `gdal`, so `dnf install gdal` fails outright. It ships GDAL 3.10.3 and
#     lives in the AL2023 CORE repo -- EPEL is not binary compatible with AL2023
#     and is not needed, and AWS's SPAL repo does NOT carry GDAL despite its own
#     documentation naming GDAL as an example.
#
#  2. gdal310 only appeared around AL2023 release 2023.8. An AMI older than that
#     sees zero gdal packages because AL2023 pins repos per release, so the
#     guard below is not paranoia.
#
#  3. tippecanoe is not packaged anywhere and must be built.
#
# The binaries themselves are unversioned: /usr/bin/gdal_contour, not
# /usr/bin/gdal310_contour. Only the package name carries the suffix.
#
# NOTE: nothing here installs Python. The build uses GDAL's C binaries, awk,
# coreutils and tippecanoe. `gdal310-python-tools` and `python3-gdal310` are
# deliberately NOT dependencies.

set -euo pipefail

PMTILES_VERSION="${PMTILES_VERSION:-1.31.2}"
TIPPECANOE_REF="${TIPPECANOE_REF:-2.79.0}"

log() { printf '[bootstrap] %s\n' "$*" >&2; }

log "AL2023 release: $(cat /etc/system-release 2>/dev/null || echo unknown)"

if ! dnf -q info gdal310 >/dev/null 2>&1; then
  cat >&2 <<'EOF'
[FATAL] gdal310 is not visible to dnf.

  AL2023 locks its repositories to the release the AMI was built from, and
  gdal310 only entered the core repo around release 2023.8. Either
  `sudo dnf upgrade --releasever=latest` first, or start from a newer AMI.
  Do NOT reach for EPEL: it is not binary compatible with AL2023.
EOF
  exit 1
fi

log "installing GDAL and build tools"
sudo dnf install -y \
  gdal310 \
  sqlite \
  gcc-c++ make git zlib-devel sqlite-devel libzstd-devel \
  tar gzip curl

log "GDAL: $(gdalinfo --version)"

# tippecanoe: not packaged on any RHEL-family distro. Built from a tag, not from
# HEAD, so a rebuild in two years produces the same tiler.
if ! command -v tippecanoe >/dev/null 2>&1; then
  log "building tippecanoe $TIPPECANOE_REF"
  tmp="$(mktemp -d)"
  git clone --depth 1 --branch "$TIPPECANOE_REF" \
    https://github.com/felt/tippecanoe "$tmp/tippecanoe"
  make -C "$tmp/tippecanoe" -j"$(nproc)"
  sudo make -C "$tmp/tippecanoe" install
  rm -rf "$tmp"
fi
log "tippecanoe: $(tippecanoe --version 2>&1 | head -1)"

# go-pmtiles: the ONLY step GDAL cannot do. GDAL's PMTiles driver is vector-only
# in every released version, so raster PMTiles has to come from here.
#
# Asset naming differs per platform -- Linux uses an underscore after
# "go-pmtiles", macOS uses a hyphen. Hardcode the Linux form.
if ! command -v pmtiles >/dev/null 2>&1; then
  log "installing go-pmtiles $PMTILES_VERSION"
  arch="$(uname -m)"
  case "$arch" in
    x86_64)  asset="Linux_x86_64" ;;
    aarch64) asset="Linux_arm64"  ;;
    *) echo "[FATAL] unsupported arch $arch" >&2; exit 1 ;;
  esac
  tmp="$(mktemp -d)"
  curl -fsSL -o "$tmp/p.tar.gz" \
    "https://github.com/protomaps/go-pmtiles/releases/download/v${PMTILES_VERSION}/go-pmtiles_${PMTILES_VERSION}_${asset}.tar.gz"
  tar xzf "$tmp/p.tar.gz" -C "$tmp" pmtiles
  sudo install -m755 "$tmp/pmtiles" /usr/local/bin/pmtiles
  rm -rf "$tmp"
fi
log "pmtiles: $(pmtiles version 2>&1 | head -1)"

# The instance-store stripe. The launch template already assembles it; this only
# checks that the build has somewhere to put ~1 TB of intermediates.
WORK="${WORK:-/mnt/terrain-work}"
if [ ! -d "$WORK" ]; then
  log "WARNING: $WORK does not exist. Create it on the instance-store stripe"
  log "         before running build.sh, or set WORK to a large volume."
else
  log "work volume: $(df -h "$WORK" | tail -1)"
fi

cat >&2 <<EOF

[bootstrap] ready.

  RASTER_ENCODING=hillshade   (default) works on this GDAL 3.10.3.
  RASTER_ENCODING=terrain-rgb NEEDS GDAL >= 3.11 for VRT expression pixel
                              functions and will refuse to run here. Use
                              ghcr.io/osgeo/gdal:ubuntu-small-3.13.2 for that.

  Next:  WORK=$WORK ./build.sh
EOF
