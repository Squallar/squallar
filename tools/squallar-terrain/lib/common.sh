# Shared configuration and helpers for the squallar terrain build.
#
# Sourced by build.sh, build-contours.sh, build-raster.sh and selftest.sh.
# Nothing here executes work; it only defines the pins, the schedule and a few
# helpers.

set -euo pipefail

# ---------------------------------------------------------------------------
# The DEM pin.
#
# `copernicus-dem-30m` is NOT a versioned bucket: every object answers
# `x-amz-version-id: null`, so there is no S3 version id to pin to and no
# per-release prefix to select. The AWS Open Data registry entry
# (awslabs/open-data-registry, datasets/copernicus-dem.yaml) states the bucket
# "comes from Copernicus DEM 2021 release", and its UpdateFrequency reads
# "None, except GLO-30 Public can be updated if the public tile list changes."
#
# So the elevation values are fixed and the only thing that can move underneath
# a rerun is the SET OF TILES, as countries release previously withheld ones.
# That makes `tileList.txt` the pin, and we pin it by content.
# ---------------------------------------------------------------------------
DEM_BUCKET="${DEM_BUCKET:-copernicus-dem-30m}"
DEM_BUCKET_REGION="eu-central-1"
DEM_RELEASE="COP-DEM_GLO-30 Public, 2021 release"

# Observed 2026-08-27. `verify_tilelist` fails the build if either moves; that
# is the signal that the public tile set changed and the archives are stale.
DEM_TILELIST_MD5="637fe75ddf7615ba853dd83caf05cd82"
DEM_TILELIST_COUNT="26450"
DEM_TILELIST_BYTES="1110900"

# The MODIFIED-work form of the notice, which is the one that applies: tiling
# and Terrain-RGB encoding make these archives a derivative. The unmodified-
# redistribution wording is a different string and would be the wrong claim.
DEM_ATTRIBUTION="produced using Copernicus WorldDEM-30 © DLR e.V. 2010-2014 and © Airbus Defence and Space GmbH 2014-2018 provided under COPERNICUS by the European Union and ESA; all rights reserved"

# ---------------------------------------------------------------------------
# Contour interval schedule -- "MINZOOM:MAXZOOM:INTERVAL_METRES".
#
# HARMONIC CONSTRAINT: each interval must divide the next coarser one exactly.
# 1000 / 200 = 5, 200 / 100 = 2. `verify_schedule` enforces this.
#
# If it is violated, contour lines VANISH as you zoom in -- a 500 m line has no
# counterpart in a 200 m band, so crossing that zoom makes an existing line
# disappear, which reads as a rendering bug rather than a data choice.
# ---------------------------------------------------------------------------
CONTOUR_BANDS=("10:10:1000" "11:12:200" "13:14:100")

CONTOUR_LAYER="contour"
CONTOUR_ATTR="elev"

# ---------------------------------------------------------------------------
# Raster archive.
#
# RASTER_ENCODING=hillshade     1-band grey, `gdaldem hillshade`.  DEFAULT.
# RASTER_ENCODING=terrain-rgb   3-band PNG, Mapbox Terrain-RGB v1.
#                               height = -10000 + (R*65536 + G*256 + B) * 0.1
#
# hillshade is the default for v1 because it makes the whole encoding problem
# disappear rather than solving it: `gdaldem` is GDAL's own C, present in every
# GDAL including the 3.10.3 that Amazon Linux 2023 packages, and there is no
# encode step to get wrong. If the requirement is "the map shows terrain", this
# is the entire job.
#
# terrain-rgb is what you want when the CLIENT needs the elevation: relighting
# at an arbitrary azimuth, exaggeration, elevation readout, or draping the 3D
# volumetric view. That work is long-tail and unscheduled, so it does not get to
# set the v1 dependency budget -- but the mode is implemented and verified, so
# switching costs a rebuild rather than a redesign.
#
# terrain-rgb MUST be stored losslessly. A single-count error in R is 6553.6 m,
# so lossy WebP/JPEG on a terrain-rgb archive is not "slightly soft", it is
# destroyed. `verify_raster_settings` enforces that. A hillshade is an ordinary
# grey image with no such constraint, which is most of why it is smaller.
# ---------------------------------------------------------------------------
RASTER_ENCODING="${RASTER_ENCODING:-hillshade}"
RASTER_MINZOOM="${RASTER_MINZOOM:-0}"
RASTER_MAXZOOM="${RASTER_MAXZOOM:-12}"
# PNG unless overridden; hillshade may safely use lossy WEBP, terrain-rgb may not.
RASTER_TILE_FORMAT="${RASTER_TILE_FORMAT:-PNG}"

# Zooms at or below this are built from ONE global mercator DEM; zooms above it
# are built per chunk. At z8 the global grid is 65536x65536, which is 8.6 GB as
# Float32 and the largest single raster this build ever materialises.
RASTER_GLOBAL_MAXZOOM="${RASTER_GLOBAL_MAXZOOM:-8}"

# Chunk edge in whole degrees for both the contour and the high-zoom raster
# passes. Peak disk scales with the square of this; see README "Peak disk".
CHUNK_DEG="${CHUNK_DEG:-5}"

# ---------------------------------------------------------------------------
# Layout
# ---------------------------------------------------------------------------
TERRAIN_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MERC_AWK="$TERRAIN_ROOT/lib/mercator.awk"
# shellcheck source=encode.sh
. "$TERRAIN_ROOT/lib/encode.sh"
WORK="${WORK:-/mnt/terrain-work}"
OUT="${OUT:-$WORK/out}"
TMP="${TMP:-$WORK/tmp}"
JOBS="${JOBS:-$(nproc)}"

CONTOURS_PMTILES="$OUT/squallar-contours.pmtiles"
RASTER_PMTILES="$OUT/squallar-terrain-$RASTER_ENCODING.pmtiles"

log() { printf '[%(%H:%M:%S)T] %s\n' -1 "$*" >&2; }
die() { printf '[FATAL] %s\n' "$*" >&2; exit 1; }

need() {
  for c in "$@"; do
    command -v "$c" >/dev/null 2>&1 || die "missing required command: $c"
  done
}

# ---------------------------------------------------------------------------
# The harmonic check. Runs on every build, not just when the schedule is edited,
# because the point is that an edit cannot land without tripping it.
# ---------------------------------------------------------------------------
verify_schedule() {
  local prev="" band lo hi iv
  for band in "${CONTOUR_BANDS[@]}"; do
    IFS=: read -r lo hi iv <<<"$band"
    [ "$lo" -le "$hi" ] || die "band $band: minzoom > maxzoom"
    if [ -n "$prev" ]; then
      [ "$iv" -lt "$prev" ] || die "band $band: interval must decrease as zoom rises"
      if [ $(( prev % iv )) -ne 0 ]; then
        die "NON-HARMONIC SCHEDULE: $iv m does not divide $prev m exactly.
     Contours would VANISH when crossing into the $iv m band, because lines at
     multiples of $prev m that are not multiples of $iv m have no counterpart
     there. Pick an interval that divides $prev m."
      fi
    fi
    prev="$iv"
  done
  log "contour schedule harmonic: ${CONTOUR_BANDS[*]}"
}

verify_raster_settings() {
  case "$RASTER_ENCODING" in
    terrain-rgb)
      require_terrain_rgb_support
      if [ "$RASTER_TILE_FORMAT" != "PNG" ]; then
        die "terrain-rgb must be stored losslessly; RASTER_TILE_FORMAT=$RASTER_TILE_FORMAT
     would quantise the packed elevation bytes. One count of error in the R
     channel is 6553.6 m."
      fi
      ;;
    hillshade) ;;
    *) die "unknown RASTER_ENCODING=$RASTER_ENCODING (terrain-rgb|hillshade)" ;;
  esac
}

# Fetch tileList.txt and refuse to proceed if the public tile set moved.
fetch_tilelist() {
  local dest="$1"
  log "fetching s3://$DEM_BUCKET/tileList.txt"
  curl -fsS -o "$dest" "https://$DEM_BUCKET.s3.amazonaws.com/tileList.txt"
  verify_tilelist "$dest"
}

# Verify the pin against the RAW bytes, then leave a LF-normalised copy beside
# it for everything else to read.
#
# tileList.txt is CRLF-terminated -- all 26450 lines, verified 2026-08-27. Every
# key built by pasting a raw line into a URL carries a trailing \r and 404s, and
# `grep -qxF "$name" tileList.txt` matches NOTHING. The md5 pin has to hash the
# CRLF bytes, because that is what the bucket serves.
verify_tilelist() {
  local f="$1"
  local got_md5 got_n got_b
  got_md5="$(md5sum "$f" | cut -d' ' -f1)"
  got_n="$(wc -l <"$f")"
  got_b="$(stat -c%s "$f")"
  if [ "$got_md5" != "$DEM_TILELIST_MD5" ]; then
    die "tileList.txt moved.
     pinned md5 $DEM_TILELIST_MD5 ($DEM_TILELIST_COUNT tiles, $DEM_TILELIST_BYTES bytes)
     actual md5 $got_md5 ($got_n tiles, $got_b bytes)
     GLO-30 Public gained or lost tiles. The elevation values themselves do not
     change; re-pin DEM_TILELIST_* in lib/common.sh, note the date, and rebuild."
  fi
  if [ "$got_n" -ne "$DEM_TILELIST_COUNT" ]; then
    die "tileList.txt line count $got_n != pinned $DEM_TILELIST_COUNT"
  fi
  tr -d '\r' <"$f" >"$f.lf"
  log "tileList.txt matches pin: $got_n tiles, $DEM_RELEASE (LF copy at $f.lf)"
}

# Every consumer reads the normalised copy, never the raw one.
tilelist_lf() { printf '%s.lf\n' "$1"; }

# A PMTiles v3 archive begins with the 7-byte magic "PMTiles" then a version
# byte. Asserted because tippecanoe and tile-join choose their output container
# from the FILE EXTENSION: `-o foo.pmtiles.part` writes SQLite, and a later
# rename to .pmtiles leaves a file that is the wrong format, the right name, a
# plausible size, and a zero exit status.
assert_pmtiles() {
  local f="$1" magic
  [ -s "$f" ] || die "$f is missing or empty"
  magic="$(head -c 7 "$f")"
  [ "$magic" = "PMTiles" ] || die "$f is not a PMTiles archive (magic '$magic').
     tippecanoe/tile-join infer the container from the output file extension --
     check that every -o argument ends in .pmtiles."
}

# Absolute S3 (or /vsis3/) path of one tile's DEM GeoTIFF.
tile_key() { printf '%s/%s.tif\n' "$1" "$1"; }
tile_vsis3() { printf '/vsis3/%s/%s\n' "$DEM_BUCKET" "$(tile_key "$1")"; }
tile_url() { printf 'https://%s.s3.amazonaws.com/%s\n' "$DEM_BUCKET" "$(tile_key "$1")"; }

# Copernicus tile name -> integer SW corner, e.g.
#   Copernicus_DSM_COG_10_N39_00_W106_00_DEM -> "39 -106"
tile_corner() {
  # Pure bash: no python3, for two reasons. The stated one is that this pipeline
  # takes no Python dependency at all. The one that costs money is that this is
  # called ONCE PER TILE across 26,450 tiles -- forking an interpreter that many
  # times is minutes of pure process-spawn overhead for a regex and two sign
  # flips.
  #
  # `10#` on both numbers is load-bearing: the names carry zero-padded fields
  # (N08, W006) and bash reads a leading zero as octal, so $((08)) is a syntax
  # error rather than 8. Python's int() hid that; bash does not.
  local n="$1" ns lat ew lon
  [[ "$n" =~ _([NS])([0-9]+)_00_([EW])([0-9]+)_00_DEM$ ]] || die "unparseable tile name: $n"
  ns="${BASH_REMATCH[1]}"; lat="${BASH_REMATCH[2]}"
  ew="${BASH_REMATCH[3]}"; lon="${BASH_REMATCH[4]}"
  lat=$((10#$lat)); lon=$((10#$lon))
  [ "$ns" = "S" ] && lat=$(( -lat ))
  [ "$ew" = "W" ] && lon=$(( -lon ))
  printf '%d %d\n' "$lat" "$lon"
}
