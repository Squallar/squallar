#!/usr/bin/env bash
# Build the RASTER archive: PMTiles v3, hillshade by default.
#
# A separate archive from the contours because PMTiles v3 carries ONE
# `tile_type` byte for the whole file. MVT and PNG cannot share a container;
# this is a format constraint, not a packaging preference.
#
# Every zoom is generated from ELEVATION resampled to that zoom's resolution and
# then encoded. Zooms are never derived from each other's pixels. See "THE
# OVERVIEW TRAP" below -- it is the single most important correctness property
# of this script.
#
# Usage:  build-raster.sh [tileList.txt]
# Env:    WORK OUT TMP JOBS RASTER_ENCODING RASTER_MAXZOOM RASTER_GLOBAL_MAXZOOM

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/common.sh
. "$HERE/lib/common.sh"

need curl gdalwarp gdalbuildvrt gdal_translate gdaldem gdalinfo sqlite3 awk flock
command -v pmtiles >/dev/null 2>&1 \
  || die "missing 'pmtiles' (go-pmtiles); run bootstrap-al2023.sh"

verify_raster_settings

TILELIST="${1:-$WORK/tileList.txt}"
STAGE="$TMP/raster-stage"
ACC="$WORK/raster-acc"
mkdir -p "$OUT" "$STAGE" "$ACC"

[ -f "$TILELIST" ] || fetch_tilelist "$TILELIST"
verify_tilelist "$TILELIST"
TILELIST="$(tilelist_lf "$TILELIST")"

# Tiles per side of a super-cell. 64 -> 16384x16384 px -> 1.07 GB as Float32,
# and identically so everywhere on the globe, which is the whole reason raster
# chunking counts tiles rather than degrees.
SUPERCELL="${SUPERCELL:-64}"

GLOBAL_ELEV="$WORK/global_z${RASTER_GLOBAL_MAXZOOM}_elev.tif"

# GDAL only grew MBTiles ELEVATION_TYPE in 3.13.0, and even there it is only a
# metadata label (lib/encode.sh explains). Pass it when it exists so the archive
# is self-describing; skip it otherwise rather than emit a warning per cell.
ELEV_TYPE_CO=()
if [ "$RASTER_ENCODING" = terrain-rgb ] \
   && gdalinfo --format MBTiles 2>/dev/null | grep -q ELEVATION_TYPE; then
  ELEV_TYPE_CO=(-co "ELEVATION_TYPE=terrain-rgb")
fi

# ---------------------------------------------------------------------------
# THE OVERVIEW TRAP
#
# `gdaladdo -r average` over a terrain-RGB image averages R, G and B
# INDEPENDENTLY. The encoding is a base-256 positional number, so averaging the
# digits ignores every carry between them. Measured on the N39 W106 probe at a
# single 2x reduction: max error 3289.7 m, mean 14.6 m, 14.5% of pixels wrong by
# more than 10 m. It looks plausible and is garbage.
#
# `-r nearest` is exactly correct -- it copies one source triple verbatim -- but
# aliases badly on shaded relief.
#
# The same argument applies to hillshade, whose 3x3 slope window is just as
# non-linear: a hillshade of a downsampled DEM is not a downsampled hillshade.
#
# So: resample the ELEVATION, then encode, at every zoom independently. The cost
# is a 1/(1-1/4) = 1.33x multiplier over doing the deepest zoom alone.
# ---------------------------------------------------------------------------

# encode_and_tile <elev.tif> <zoom> <centre-lat> <out.mbtiles> <te+ts...>
encode_and_tile() {
  local elev="$1" z="$2" clat="$3" out="$4"
  local xmin="$5" ymin="$6" xmax="$7" ymax="$8" nx="$9" ny="${10}"
  local enc="${elev%.tif}.enc.tif"

  case "$RASTER_ENCODING" in
    terrain-rgb)
      make_terrain_rgb_vrt "$elev" "${elev%.tif}.vrt" \
        "$xmin" "$ymin" "$xmax" "$ymax" "$nx" "$ny"
      gdal_translate -q -co COMPRESS=DEFLATE -co PREDICTOR=2 -co TILED=YES \
        "${elev%.tif}.vrt" "$enc"
      rm -f "${elev%.tif}.vrt"
      ;;
    hillshade)
      # -s corrects for the fact that EPSG:3857 "metres" are not ground metres:
      # they are inflated by 1/cos(lat), so a slope computed against raw
      # Mercator pixel spacing is too shallow by cos(lat) -- a factor of 2 at
      # 60N, which is the difference between the Alps looking like the Alps and
      # looking like a rumpled sheet. gdaldem takes one scalar, so this uses the
      # super-cell's centre latitude; a super-cell spans little latitude at the
      # zooms where relief is legible, so the residual is small.
      #
      # -compute_edges stops a one-pixel dark frame appearing at every
      # super-cell edge, which would otherwise draw a grid over the planet.
      local s
      s="$(awk -v d="$clat" 'BEGIN{c=cos(d*3.14159265358979/180); print (c<1e-6?1e-6:c)}')"
      gdaldem hillshade -q -alg Horn -z 1 -s "$s" -az 315 -alt 45 \
        -compute_edges -co COMPRESS=DEFLATE "$elev" "$enc"
      ;;
  esac

  # No -co ZOOM_LEVEL: the MBTiles driver advertises it but its CreateCopy path
  # rejects it ("driver MBTiles does not support creation option ZOOM_LEVEL").
  # It is unnecessary anyway -- the source was warped onto zoom $z's exact grid,
  # so the driver derives $z from the resolution. That inference is asserted
  # rather than assumed, because a silently-misplaced zoom would put real tiles
  # at the wrong address and still look like a successful build.
  gdal_translate -q -of MBTILES \
    -co "TILE_FORMAT=$RASTER_TILE_FORMAT" \
    "${ELEV_TYPE_CO[@]}" \
    "$enc" "$out"

  local got
  got="$(sqlite3 "$out" "SELECT DISTINCT zoom_level FROM tiles;" | paste -sd,)"
  if [ -z "$got" ]; then
    # A super-cell that is entirely ocean or entirely outside the DEM's coverage
    # produces no tiles. That is expected -- the land mask is per degree cell, so
    # a cell can qualify on one corner and still be empty at tile resolution.
    rm -f "$enc" "$elev" "$out"
    return 1
  fi
  [ "$got" = "$z" ] || die "zoom mismatch: warped to z$z but MBTiles holds z$got"
  rm -f "$enc" "$elev"
}

# merge_mbtiles <src.mbtiles> <dst.mbtiles>
# The raster analogue of tile-join, which is vector-only. MBTiles is SQLite, so
# appending one archive to another is an ATTACH and an INSERT. Super-cells abut
# rather than overlap; OR REPLACE just makes a resumed cell idempotent.
merge_mbtiles() {
  local src="$1" dst="$2"
  if [ ! -f "$dst" ]; then cp "$src" "$dst"; return; fi
  sqlite3 "$dst" \
    "ATTACH DATABASE '$src' AS s;
     INSERT OR REPLACE INTO tiles (zoom_level, tile_column, tile_row, tile_data)
       SELECT zoom_level, tile_column, tile_row, tile_data FROM s.tiles;
     DETACH DATABASE s;"
}

# ---------------------------------------------------------------------------
# One global elevation raster, built ONCE from the COGs. Zooms at or below
# RASTER_GLOBAL_MAXZOOM resample from it instead of going back to S3; GDAL
# serves those low-resolution reads out of each COG's own internal overviews,
# so this touches a small fraction of the 1.5 TB.
# ---------------------------------------------------------------------------
build_global_elev() {
  [ -f "$GLOBAL_ELEV" ] && { log "global elevation raster present"; return; }
  log "building global VRT over $DEM_TILELIST_COUNT COGs (/vsis3/)"
  awk -v b="$DEM_BUCKET" '{printf "/vsis3/%s/%s/%s.tif\n", b, $1, $1}' \
    "$TILELIST" >"$WORK/vsis3-tiles.txt"
  gdalbuildvrt -q -input_file_list "$WORK/vsis3-tiles.txt" "$WORK/global.vrt"

  read -r xmin ymin xmax ymax nx ny < <(
    awk -v CMD=extent -v Z="$RASTER_GLOBAL_MAXZOOM" \
        -v W=-180 -v S=-85.0511287798066 -v E=180 -v N=85.0511287798066 \
        -f "$MERC_AWK" </dev/null)
  log "global z$RASTER_GLOBAL_MAXZOOM grid: ${nx}x${ny} px"
  AWS_NO_SIGN_REQUEST=YES gdalwarp -q -overwrite \
    -t_srs EPSG:3857 -te "$xmin" "$ymin" "$xmax" "$ymax" -ts "$nx" "$ny" \
    -r average -ot Float32 -dstnodata 0 -multi -wo NUM_THREADS="$JOBS" \
    -co COMPRESS=DEFLATE -co PREDICTOR=3 -co TILED=YES -co BIGTIFF=YES \
    "$WORK/global.vrt" "$GLOBAL_ELEV"
}

# ---------------------------------------------------------------------------
# One super-cell at one zoom. Above RASTER_GLOBAL_MAXZOOM the source is a VRT
# over just the COGs it overlaps, with a one-degree margin so the resampling
# kernel and gdaldem's 3x3 window see real neighbours rather than an edge --
# that margin is what stops a grid of seams appearing at super-cell borders.
# ---------------------------------------------------------------------------
supercell_one() {
  local z="$1" tx0="$2" ty0="$3" tx1="$4" ty1="$5" name="$6"
  local acc="$ACC/z$z.mbtiles" part="$STAGE/$name.mbtiles"
  [ -f "$STAGE/$name.done" ] && return 0

  local xmin ymin xmax ymax nx ny w s e n clat src
  read -r xmin ymin xmax ymax nx ny < <(
    awk -v CMD=tile-extent -v Z="$z" -v TX0="$tx0" -v TY0="$ty0" \
        -v TX1="$tx1" -v TY1="$ty1" -f "$MERC_AWK" </dev/null)
  read -r w s e n clat < <(
    awk -v CMD=bbox -v Z="$z" -v TX0="$tx0" -v TY0="$ty0" \
        -v TX1="$tx1" -v TY1="$ty1" -f "$MERC_AWK" </dev/null)

  if [ "$z" -le "$RASTER_GLOBAL_MAXZOOM" ]; then
    src="$GLOBAL_ELEV"
  else
    local srcs=() lat lon nm
    for (( lat = s; lat < n; lat++ )); do
      (( lat < -90 || lat > 89 )) && continue
      for (( lon = w; lon < e; lon++ )); do
        (( lon < -180 || lon > 179 )) && continue
        nm="$(printf 'Copernicus_DSM_COG_10_%s%02d_00_%s%03d_00_DEM' \
                "$([ "$lat" -ge 0 ] && echo N || echo S)" "${lat#-}" \
                "$([ "$lon" -ge 0 ] && echo E || echo W)" "${lon#-}")"
        grep -qxF "$nm" "$TILELIST" && srcs+=("$(tile_vsis3 "$nm")")
      done
    done
    [ "${#srcs[@]}" -eq 0 ] && { touch "$STAGE/$name.done"; return 0; }
    printf '%s\n' "${srcs[@]}" >"$STAGE/$name.txt"
    gdalbuildvrt -q -input_file_list "$STAGE/$name.txt" "$STAGE/$name.vrt"
    src="$STAGE/$name.vrt"
  fi

  local elev="$STAGE/$name.elev.tif"
  AWS_NO_SIGN_REQUEST=YES gdalwarp -q -overwrite \
    -t_srs EPSG:3857 -te "$xmin" "$ymin" "$xmax" "$ymax" -ts "$nx" "$ny" \
    -r average -ot Float32 -dstnodata 0 \
    -co COMPRESS=DEFLATE -co PREDICTOR=3 -co TILED=YES \
    "$src" "$elev"

  if encode_and_tile "$elev" "$z" "$clat" "$part" \
       "$xmin" "$ymin" "$xmax" "$ymax" "$nx" "$ny"; then
    flock "$acc.lock" -c \
      "$(declare -f merge_mbtiles); merge_mbtiles '$part' '$acc'"
  fi
  rm -f "$part" "$STAGE/$name.vrt" "$STAGE/$name.txt"
  touch "$STAGE/$name.done"
}
export -f supercell_one encode_and_tile merge_mbtiles

# Only needed for the zooms that resample from it. Building high zooms alone
# (RASTER_MINZOOM > RASTER_GLOBAL_MAXZOOM) reads the COGs directly and does not
# need the global raster at all, which is also what makes a one-cell smoke test
# cheap enough to run.
if [ "$RASTER_MINZOOM" -le "$RASTER_GLOBAL_MAXZOOM" ]; then
  build_global_elev
else
  log "min zoom $RASTER_MINZOOM > $RASTER_GLOBAL_MAXZOOM: skipping global raster"
fi

# Fewer jobs than cores: each warp is already multi-threaded internally and each
# holds ~1 GB of Float32 plus its encoded copy.
SC_JOBS=$(( JOBS / 4 > 0 ? JOBS / 4 : 1 ))
export HERE STAGE ACC TILELIST MERC_AWK GLOBAL_ELEV SUPERCELL \
       RASTER_ENCODING RASTER_TILE_FORMAT RASTER_GLOBAL_MAXZOOM

for (( z = RASTER_MINZOOM; z <= RASTER_MAXZOOM; z++ )); do
  log "z$z: enumerating ${SUPERCELL}x${SUPERCELL}-tile super-cells over land"
  awk -v CMD=supercells -v Z="$z" -v SIDE="$SUPERCELL" -f "$MERC_AWK" \
    "$TILELIST" | sort -n >"$WORK/sc_z$z.txt"
  # See ONLY_CHUNK in build-contours.sh; same purpose, matched on the cell name.
  if [ -n "${ONLY_SUPERCELL-}" ]; then
    grep -E "$ONLY_SUPERCELL" "$WORK/sc_z$z.txt" >"$WORK/sc_sel" || true
    mv "$WORK/sc_sel" "$WORK/sc_z$z.txt"
  fi
  log "z$z: $(wc -l <"$WORK/sc_z$z.txt") super-cells, $SC_JOBS jobs"
  awk -v z="$z" '{print z, $0}' "$WORK/sc_z$z.txt" \
    | xargs -P "$SC_JOBS" -L1 \
        bash -c '. "$HERE/lib/common.sh"; supercell_one "$@"' _
done

# ---------------------------------------------------------------------------
# One MBTiles, then one PMTiles. `pmtiles convert` is the only step here GDAL
# cannot do: GDAL's PMTiles driver is registered `-vector-` in every released
# version, 3.13.3 included, so there is no raster PMTiles writer in GDAL at all.
#
# go-pmtiles decides the archive's tile_type from the MBTiles `metadata` row
# named `format`, so that row must read png/jpg/webp or the conversion produces
# an archive a viewer cannot interpret.
# ---------------------------------------------------------------------------
log "merging per-zoom archives"
FINAL="$WORK/raster-all.mbtiles"
rm -f "$FINAL"
for f in "$ACC"/z*.mbtiles; do
  log "  + $(basename "$f") ($(stat -c%s "$f") bytes)"
  merge_mbtiles "$f" "$FINAL"
done

sqlite3 "$FINAL" \
  "INSERT OR REPLACE INTO metadata (name, value) VALUES
     ('name', 'squallar-terrain-$RASTER_ENCODING'),
     ('format', '$(echo "$RASTER_TILE_FORMAT" | tr 'A-Z' 'a-z')'),
     ('minzoom', '$RASTER_MINZOOM'), ('maxzoom', '$RASTER_MAXZOOM'),
     ('attribution', '$DEM_ATTRIBUTION'),
     ('description', 'Copernicus GLO-30 $RASTER_ENCODING, $DEM_RELEASE');"

log "converting to PMTiles v3"
pmtiles convert "$FINAL" "$RASTER_PMTILES"
assert_pmtiles "$RASTER_PMTILES"
log "raster: $RASTER_PMTILES ($(stat -c%s "$RASTER_PMTILES") bytes)"
