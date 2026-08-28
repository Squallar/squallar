#!/usr/bin/env bash
# Toolchain for the terrain build on Amazon Linux 2023, and the EC2 user-data.
#
# This is the only shell left in the tool, and it is shell because it runs
# before the binary exists. Its SIZE is load-bearing: user-data is capped at
# 16384 bytes after base64, and the shell build this replaced was 23964.
#
# Facts, verified against AL2023 repo metadata rather than recalled:
#
#  1. The package is `gdal310`, NOT `gdal`. No gdal310 package Provides a bare
#     `gdal`, so `dnf install gdal` fails outright. It ships GDAL 3.10.3 and
#     lives in the AL2023 CORE repo -- EPEL is not binary compatible with AL2023
#     and is not needed. Nothing here needs a newer GDAL: the Terrain-RGB
#     encoding is done in the binary, so no VRT expression pixel function and no
#     muparser is involved.
#  2. gdal310 only appeared around AL2023 release 2023.8. An AMI older than that
#     sees zero gdal packages because AL2023 pins repos per release, so the
#     guard below is not paranoia.
#  3. tippecanoe is not packaged anywhere and must be built.
#  4. go-pmtiles is the ONLY step GDAL cannot do: GDAL's PMTiles driver is
#     registered `-vector-` in every released version, so raster PMTiles has to
#     come from there.
#
# The binaries themselves are unversioned: /usr/bin/gdal_contour, not
# /usr/bin/gdal310_contour. Only the package name carries the suffix.
#
# No Python and no Rust toolchain: the build uses GDAL's C binaries, coreutils,
# tippecanoe, go-pmtiles and one statically linked binary fetched below.

set -euo pipefail

PMTILES_VERSION="${PMTILES_VERSION:-1.31.2}"
TIPPECANOE_REF="${TIPPECANOE_REF:-2.79.0}"
WORK="${WORK:-/mnt/terrain-work}"

log() { printf '[bootstrap] %s\n' "$*" >&2; }

[ -n "${TERRAIN_BIN_URL:-}" ] || {
  echo "[FATAL] set TERRAIN_BIN_URL to a squallar-terrain binary built for musl" >&2
  echo "        (see README 'Deploying'); a glibc build from a modern distro" >&2
  echo "        will not run against AL2023's glibc 2.34." >&2
  exit 1
}

log "AL2023 release: $(cat /etc/system-release 2>/dev/null || echo unknown)"

dnf -q info gdal310 >/dev/null 2>&1 || {
  cat >&2 <<'EOF'
[FATAL] gdal310 is not visible to dnf.
  AL2023 locks its repositories to the release the AMI was built from, and
  gdal310 only entered the core repo around release 2023.8. Either
  `sudo dnf upgrade --releasever=latest` first, or start from a newer AMI.
  Do NOT reach for EPEL: it is not binary compatible with AL2023.
EOF
  exit 1
}

log "installing GDAL and build tools"
# NO `curl` IN THIS LIST. AL2023 ships `curl-minimal`, which provides
# /usr/bin/curl and speaks HTTPS -- the three fetches below are proof. Asking
# for the full `curl` package makes dnf refuse the whole transaction with a
# conflict against every curl-minimal build in the repo, and the error runs to
# hundreds of lines that name curl-minimal rather than the request that caused
# it. Use `--allowerasing` only if something here ever genuinely needs a
# protocol curl-minimal lacks.
sudo dnf install -y \
  gdal310 sqlite \
  gcc-c++ make git zlib-devel sqlite-devel libzstd-devel \
  tar gzip
log "GDAL: $(gdalinfo --version)"

# tippecanoe: built from a tag, not from HEAD, so a rebuild in two years
# produces the same tiler.
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

# Asset naming differs per platform -- Linux uses an underscore after
# "go-pmtiles", macOS uses a hyphen. Hardcode the Linux form.
if ! command -v pmtiles >/dev/null 2>&1; then
  log "installing go-pmtiles $PMTILES_VERSION"
  case "$(uname -m)" in
    x86_64)  asset="Linux_x86_64" ;;
    aarch64) asset="Linux_arm64"  ;;
    *) echo "[FATAL] unsupported arch $(uname -m)" >&2; exit 1 ;;
  esac
  tmp="$(mktemp -d)"
  curl -fsSL -o "$tmp/p.tar.gz" \
    "https://github.com/protomaps/go-pmtiles/releases/download/v${PMTILES_VERSION}/go-pmtiles_${PMTILES_VERSION}_${asset}.tar.gz"
  tar xzf "$tmp/p.tar.gz" -C "$tmp" pmtiles
  sudo install -m755 "$tmp/pmtiles" /usr/local/bin/pmtiles
  rm -rf "$tmp"
fi
log "pmtiles: $(pmtiles version 2>&1 | head -1)"

log "fetching squallar-terrain"
curl -fsSL -o /tmp/squallar-terrain "$TERRAIN_BIN_URL"
sudo install -m755 /tmp/squallar-terrain /usr/local/bin/squallar-terrain

# The instance-store stripe. The launch template already assembles it; this only
# checks that the build has somewhere to put ~1 TB of intermediates.
[ -d "$WORK" ] || {
  echo "[FATAL] $WORK does not exist. Create it on the instance-store stripe," >&2
  echo "        or set WORK to a large volume." >&2
  exit 1
}
log "work volume: $(df -h "$WORK" | tail -1)"

[ -z "${TERRAIN_NO_RUN:-}" ] || { log "ready; run: WORK=$WORK squallar-terrain build"; exit 0; }
log "starting build"
exec env WORK="$WORK" squallar-terrain build "${TERRAIN_TARGET:-all}"
