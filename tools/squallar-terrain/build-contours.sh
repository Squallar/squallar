#!/usr/bin/env bash
# Build the CONTOUR archive: MVT vector tiles, PMTiles v3.
#
# Chunked so that peak disk is bounded by one chunk in flight per job rather
# than by the whole planet. Intermediates are deleted as soon as the chunk they
# belong to has been tiled.
#
# Usage:  build-contours.sh [tileList.txt]
# Env:    WORK OUT TMP JOBS CHUNK_DEG   (see lib/common.sh)

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/common.sh
. "$HERE/lib/common.sh"

need curl gdal_contour tippecanoe tile-join awk

verify_schedule

TILELIST="${1:-$WORK/tileList.txt}"
PARTS="$WORK/contour-parts"
STAGE="$TMP/contour-stage"
mkdir -p "$OUT" "$PARTS" "$STAGE"

[ -f "$TILELIST" ] || fetch_tilelist "$TILELIST"
verify_tilelist "$TILELIST"
TILELIST="$(tilelist_lf "$TILELIST")"

# ---------------------------------------------------------------------------
# One chunk. Stages its degree tiles, streams contours straight into tippecanoe
# without ever writing GeoJSON to disk, joins the three zoom bands, and drops
# everything it staged.
# ---------------------------------------------------------------------------
chunk_one() {
  local w="$1" s="$2" e="$3" n="$4" name="$5"
  local out="$PARTS/$name.pmtiles"
  [ -f "$out" ] && { log "$name: already built, skipping"; return 0; }

  local dir="$STAGE/$name"
  rm -rf "$dir"; mkdir -p "$dir"

  # Which of this chunk's degree cells actually have a tile.
  local tiles=() lat lon nm
  for (( lat = s; lat < n; lat++ )); do
    for (( lon = w; lon < e; lon++ )); do
      nm="$(printf 'Copernicus_DSM_COG_10_%s%02d_00_%s%03d_00_DEM' \
              "$([ "$lat" -ge 0 ] && echo N || echo S)" "${lat#-}" \
              "$([ "$lon" -ge 0 ] && echo E || echo W)" "${lon#-}")"
      grep -qxF "$nm" "$TILELIST" && tiles+=("$nm")
    done
  done
  if [ "${#tiles[@]}" -eq 0 ]; then
    log "$name: no land tiles"; rm -rf "$dir"; return 0
  fi

  log "$name: staging ${#tiles[@]} DEM tiles"
  for nm in "${tiles[@]}"; do
    curl -fsS --retry 5 --retry-delay 2 -o "$dir/$nm.tif" "$(tile_url "$nm")" \
      || { log "$name: FAILED to fetch $nm"; rm -rf "$dir"; return 1; }
  done

  local band lo hi iv parts=()
  for band in "${CONTOUR_BANDS[@]}"; do
    IFS=: read -r lo hi iv <<<"$band"
    local bout="$dir/band_${lo}_${hi}.pmtiles"
    log "$name: contouring at ${iv} m for z${lo}-z${hi}"
    # gdal_contour writes GeoJSONSeq to stdout; the whole chunk is one stream,
    # so no GeoJSON ever lands on disk. Measured on N39 W106: the two-step
    # GPKG + GeoJSON route costs 66.7 MB per degree tile at 100 m, this costs 0.
    # A coarse band over low relief legitimately produces NOTHING: a chunk whose
    # highest ground is 300 m has no 1000 m contour, and tippecanoe exits
    # non-zero with "Did not read any valid geometries". That is data, not
    # failure -- roughly flat land is most of the planet's land. Only that exact
    # message is tolerated, so a genuine tippecanoe error still stops the chunk.
    if ! {
      for nm in "${tiles[@]}"; do
        gdal_contour -q -a "$CONTOUR_ATTR" -i "$iv" -f GeoJSONSeq \
          "$dir/$nm.tif" /vsistdout/
      done
    } | tippecanoe -q --force \
          -Z"$lo" -z"$hi" -l "$CONTOUR_LAYER" -y "$CONTOUR_ATTR" \
          --no-feature-limit --no-tile-size-limit \
          --attribution "$DEM_ATTRIBUTION" \
          -t "$dir" -o "$bout" 2>"$dir/tip.err"
    then
      if grep -q "Did not read any valid geometries" "$dir/tip.err"; then
        log "$name: no features at ${iv} m, band z${lo}-z${hi} omitted"
        rm -f "$bout"
        continue
      fi
      log "$name: tippecanoe FAILED at ${iv} m"; cat "$dir/tip.err" >&2
      rm -rf "$dir"; return 1
    fi
    parts+=("$bout")
  done

  # Every band empty means this chunk contributes no contours at all.
  if [ "${#parts[@]}" -eq 0 ]; then
    log "$name: no contours at any interval"
    rm -rf "$dir"; return 0
  fi

  # The temp name MUST keep the .pmtiles suffix. tippecanoe and tile-join pick
  # the output container from the FILE EXTENSION, so `-o foo.pmtiles.part`
  # silently writes an MBTiles (SQLite) file, which a following `mv` then
  # renames to .pmtiles. Nothing fails; the archive is simply the wrong format,
  # and only `pmtiles show` returning nothing gives it away.
  tile-join -q --force -o "$out.tmp.pmtiles" "${parts[@]}"
  mv "$out.tmp.pmtiles" "$out"
  assert_pmtiles "$out"
  rm -rf "$dir"
  log "$name: $(stat -c%s "$out") bytes"
}
export -f chunk_one

log "enumerating ${CHUNK_DEG}-degree chunks"
awk -v CMD=chunks -v DEG="$CHUNK_DEG" -f "$MERC_AWK" "$TILELIST" \
  | sort -n >"$WORK/chunks.txt"

# ONLY_CHUNK is an ERE matched against the chunk name. It exists so this exact
# script can be smoke-tested on one cell, and so a region that failed can be
# re-run without walking the other 1486.
if [ -n "${ONLY_CHUNK-}" ]; then
  grep -E "$ONLY_CHUNK" "$WORK/chunks.txt" >"$WORK/chunks.sel" || true
  mv "$WORK/chunks.sel" "$WORK/chunks.txt"
  log "ONLY_CHUNK=$ONLY_CHUNK -> $(wc -l <"$WORK/chunks.txt") chunks"
fi
log "$(wc -l <"$WORK/chunks.txt") populated chunks, $JOBS jobs"

# `bash -c` per chunk so a single failure does not abandon the run; the skip on
# an existing part file makes a rerun resume.
export PARTS STAGE TILELIST HERE MERC_AWK
xargs -a "$WORK/chunks.txt" -P "$JOBS" -L1 \
  bash -c '. "$HERE/lib/common.sh"; chunk_one "$@"' _

# ---------------------------------------------------------------------------
# Join. tile-join in generations, because thousands of paths on one command
# line is both an ARG_MAX hazard and a memory one.
# ---------------------------------------------------------------------------
log "joining $(ls "$PARTS" | wc -l) chunk archives"
gen="$PARTS"
round=0
while [ "$(find "$gen" -maxdepth 1 -name '*.pmtiles' | wc -l)" -gt 1 ]; do
  round=$(( round + 1 ))
  next="$WORK/join-$round"
  rm -rf "$next"; mkdir -p "$next"
  find "$gen" -maxdepth 1 -name '*.pmtiles' -print0 \
    | xargs -0 -n 16 -P "$JOBS" bash -c '
        out="$1/$(printf "%s" "$2" | md5sum | cut -c1-16).pmtiles"; shift
        tile-join -q --force -o "$out" "$@"' _ "$next"
  [ "$gen" != "$PARTS" ] && rm -rf "$gen"
  gen="$next"
  log "join round $round: $(find "$gen" -maxdepth 1 -name '*.pmtiles' | wc -l) archives left"
done

mv "$(find "$gen" -maxdepth 1 -name '*.pmtiles')" "$CONTOURS_PMTILES"
rm -rf "$gen"
assert_pmtiles "$CONTOURS_PMTILES"
log "contours: $CONTOURS_PMTILES ($(stat -c%s "$CONTOURS_PMTILES") bytes)"
