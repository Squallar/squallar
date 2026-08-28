#!/usr/bin/env bash
# One-shot builder for both squallar terrain archives.
#
# ONE job, because both artifacts are derived from the same 1.5 TB of Copernicus
# GLO-30 COGs and the download is the slow part. TWO archives, because PMTiles
# v3 has a single `tile_type` byte per file and MVT tiles cannot live in the
# same container as PNG ones.
#
# This is not part of the 35-day OSM basemap cycle. The DEM does not change; see
# README "Pinning" for what "does not change" is actually load-bearing on.
#
#   ./build.sh              both archives
#   ./build.sh contours     just the vector one
#   ./build.sh raster       just the raster one

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/common.sh
. "$HERE/lib/common.sh"

what="${1:-all}"
mkdir -p "$WORK" "$OUT" "$TMP"

log "squallar terrain build"
log "  DEM      $DEM_RELEASE  (s3://$DEM_BUCKET, $DEM_BUCKET_REGION)"
log "  work     $WORK"
log "  out      $OUT"
log "  jobs     $JOBS"
log "  contours ${CONTOUR_BANDS[*]}"
log "  raster   $RASTER_ENCODING z$RASTER_MINZOOM-$RASTER_MAXZOOM $RASTER_TILE_FORMAT"

# Fetched once and shared, so the pin is checked once and both passes agree on
# which tiles exist.
fetch_tilelist "$WORK/tileList.txt"

case "$what" in
  all)      "$HERE/build-contours.sh" "$WORK/tileList.txt"
            "$HERE/build-raster.sh"   "$WORK/tileList.txt" ;;
  contours) "$HERE/build-contours.sh" "$WORK/tileList.txt" ;;
  raster)   "$HERE/build-raster.sh"   "$WORK/tileList.txt" ;;
  *) die "usage: build.sh [all|contours|raster]" ;;
esac

log "done"
ls -la "$OUT"
