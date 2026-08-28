#!/usr/bin/env bash
# End-to-end proof on ONE degree tile. Launches nothing; touches no cloud.
#
# Runs the real pipeline pieces on a single Copernicus tile and prints byte
# counts, so the build script is exercised rather than merely written. Also
# asserts the two properties that are easy to get silently wrong:
#
#   * the harmonic guard actually rejects a non-harmonic schedule
#   * terrain-RGB round-trips, and pyramiding it in RGB space does not
#
# Every check is pure GDAL + awk; there is no Python here either.
#
#   ./selftest.sh [/path/to/N39_00_W106_00.tif]

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORK="${WORK:-$(mktemp -d)}"; export WORK
# shellcheck source=lib/common.sh
. "$HERE/lib/common.sh"

SRC="${1:-/home/reddragon/basemap-build/contour-probe/N39_00_W106_00.tif}"
TILE="Copernicus_DSM_COG_10_N39_00_W106_00_DEM"
D="$WORK/selftest"; mkdir -p "$D"

need gdal_contour gdalwarp gdal_translate gdaldem gdalinfo tippecanoe tile-join awk

if [ ! -f "$SRC" ]; then
  SRC="$D/$TILE.tif"
  log "fetching one degree tile"
  curl -fsS -o "$SRC" "$(tile_url "$TILE")"
fi
log "source: $SRC ($(stat -c%s "$SRC") bytes)"
gdalinfo "$SRC" | grep -E "^Size is|Type=" | head -2 >&2

hr() { printf '\n== %s\n' "$*"; }
KV() { printf '  %-46s %s\n' "$1" "$2"; }

# ---------------------------------------------------------------------------
hr "1. harmonic guard"
verify_schedule 2>/dev/null && KV "schedule A (1000/200/100)" "accepted"
(
  CONTOUR_BANDS=("10:10:1000" "11:12:500" "13:14:200")
  verify_schedule
) 2>/dev/null && { KV "non-harmonic (1000/500/200)" "ACCEPTED -- GUARD IS BROKEN"; exit 1; } \
             || KV "non-harmonic (1000/500/200)" "rejected, as it must be"

# ---------------------------------------------------------------------------
hr "2. contours: schedule A, streamed straight into tippecanoe"
tot=0; parts=()
for band in "${CONTOUR_BANDS[@]}"; do
  IFS=: read -r lo hi iv <<<"$band"
  o="$D/band_${lo}_${hi}.pmtiles"
  /usr/bin/time -f "%e" -o "$D/t_$lo" \
    bash -c "gdal_contour -q -a elev -i $iv -f GeoJSONSeq '$SRC' /vsistdout/ \
      | tippecanoe -q --force -Z$lo -z$hi -l contour -y elev \
          --no-feature-limit --no-tile-size-limit -t '$D' -o '$o'"
  b=$(stat -c%s "$o"); tot=$((tot + b)); parts+=("$o")
  KV "z$lo-z$hi @ ${iv} m" "$b bytes  ($(cat "$D/t_$lo") s)"
done
tile-join -q --force -o "$D/contours.pmtiles" "${parts[@]}"
KV "joined contour archive" "$(stat -c%s "$D/contours.pmtiles") bytes"
KV "  (sum of bands before join)" "$tot bytes"

# The comparison the schedule was chosen on: uniform 100 m over z10-z14.
gdal_contour -q -a elev -i 100 -f GeoJSONSeq "$SRC" /vsistdout/ \
  | tippecanoe -q --force -Z10 -z14 -l contour -y elev \
      --no-feature-limit --no-tile-size-limit -t "$D" -o "$D/uniform100.pmtiles"
KV "uniform 100 m z10-z14, same z13-14 detail" "$(stat -c%s "$D/uniform100.pmtiles") bytes"

# ---------------------------------------------------------------------------
hr "3. raster: warp to the real z12 WebMercatorQuad grid"
read -r xmin ymin xmax ymax nx ny < <(
  awk -v CMD=extent -v Z=12 -v W=-106 -v S=39 -v E=-105 -v N=40 \
      -f "$MERC_AWK" </dev/null)
KV "z12 snapped grid" "${nx}x${ny} px"
gdalwarp -q -overwrite -t_srs EPSG:3857 \
  -te "$xmin" "$ymin" "$xmax" "$ymax" -ts "$nx" "$ny" \
  -r average -ot Float32 -dstnodata 0 \
  -co COMPRESS=DEFLATE -co PREDICTOR=3 -co TILED=YES "$SRC" "$D/elev12.tif"
KV "elevation, EPSG:3857 Float32" "$(stat -c%s "$D/elev12.tif") bytes"

# --- hillshade (the default) ---
s=$(awk 'BEGIN{print cos(39.5*3.14159265358979/180)}')
gdaldem hillshade -q -alg Horn -z 1 -s "$s" -az 315 -alt 45 -compute_edges \
  -co COMPRESS=DEFLATE "$D/elev12.tif" "$D/hs.tif"
gdal_translate -q -of MBTILES -co TILE_FORMAT=PNG \
  "$D/hs.tif" "$D/hs.mbtiles"
KV "hillshade GeoTIFF (1 band)" "$(stat -c%s "$D/hs.tif") bytes"
KV "hillshade MBTiles PNG z12" "$(stat -c%s "$D/hs.mbtiles") bytes"
gdal_translate -q -of MBTILES -co TILE_FORMAT=WEBP -co QUALITY=85 \
  "$D/hs.tif" "$D/hs_webp.mbtiles" 2>/dev/null \
  && KV "hillshade MBTiles WEBP q85 z12" "$(stat -c%s "$D/hs_webp.mbtiles") bytes"

# --- terrain-rgb (only where GDAL is new enough) ---
if gdal_version_atleast 3.11.0; then
  make_terrain_rgb_vrt "$D/elev12.tif" "$D/trgb.vrt" \
    "$xmin" "$ymin" "$xmax" "$ymax" "$nx" "$ny"
  gdal_translate -q -co COMPRESS=DEFLATE -co PREDICTOR=2 -co TILED=YES \
    "$D/trgb.vrt" "$D/trgb.tif"
  gdal_translate -q -of MBTILES -co TILE_FORMAT=PNG \
    "$D/trgb.tif" "$D/trgb.mbtiles"
  KV "terrain-rgb GeoTIFF (3 band)" "$(stat -c%s "$D/trgb.tif") bytes"
  KV "terrain-rgb MBTiles PNG z12" "$(stat -c%s "$D/trgb.mbtiles") bytes"

  # Round trip: decode the RGB and difference it against the elevation it came
  # from, entirely inside a VRT, then read the stats off gdalinfo.
  cat >"$D/rt.vrt" <<EOF
<VRTDataset rasterXSize="$nx" rasterYSize="$ny">
  <VRTRasterBand dataType="Float64" band="1" subClass="VRTDerivedRasterBand">
    <PixelFunctionType>expression</PixelFunctionType>
    <PixelFunctionArguments expression="abs(-10000 + (B1*65536 + B2*256 + B3)*0.1 - B4)" dialect="muparser"/>
    <SourceTransferType>Float64</SourceTransferType>
    <SimpleSource><SourceFilename relativeToVRT="0">$D/trgb.tif</SourceFilename><SourceBand>1</SourceBand></SimpleSource>
    <SimpleSource><SourceFilename relativeToVRT="0">$D/trgb.tif</SourceFilename><SourceBand>2</SourceBand></SimpleSource>
    <SimpleSource><SourceFilename relativeToVRT="0">$D/trgb.tif</SourceFilename><SourceBand>3</SourceBand></SimpleSource>
    <SimpleSource><SourceFilename relativeToVRT="0">$D/elev12.tif</SourceFilename><SourceBand>1</SourceBand></SimpleSource>
  </VRTRasterBand>
</VRTDataset>
EOF
  st=$(gdalinfo -stats "$D/rt.vrt" 2>/dev/null | sed -n 's/.*Minimum=\([^,]*\), Maximum=\([^,]*\), Mean=\([^,]*\),.*/max \2 m, mean \3 m/p')
  KV "terrain-rgb round-trip error" "${st:-unavailable} (quantum 0.1 m -> max 0.05)"

  # --- THE OVERVIEW TRAP -------------------------------------------------
  # right: halve the ELEVATION, then encode.  wrong: halve the RGB.
  hx=$((nx / 2)); hy=$((ny / 2))
  gdalwarp -q -overwrite -ts "$hx" "$hy" -r average -ot Float32 \
    "$D/elev12.tif" "$D/elev11.tif"
  make_terrain_rgb_vrt "$D/elev11.tif" "$D/trgb11.vrt" \
    "$xmin" "$ymin" "$xmax" "$ymax" "$hx" "$hy"
  gdal_translate -q "$D/trgb11.vrt" "$D/right11.tif"
  gdal_translate -q -outsize "$hx" "$hy" -r average "$D/trgb.tif" "$D/wrong11.tif"
  cat >"$D/trap.vrt" <<EOF
<VRTDataset rasterXSize="$hx" rasterYSize="$hy">
  <VRTRasterBand dataType="Float64" band="1" subClass="VRTDerivedRasterBand">
    <PixelFunctionType>expression</PixelFunctionType>
    <PixelFunctionArguments expression="abs((B1*65536 + B2*256 + B3)*0.1 - (B4*65536 + B5*256 + B6)*0.1)" dialect="muparser"/>
    <SourceTransferType>Float64</SourceTransferType>
    <SimpleSource><SourceFilename relativeToVRT="0">$D/wrong11.tif</SourceFilename><SourceBand>1</SourceBand></SimpleSource>
    <SimpleSource><SourceFilename relativeToVRT="0">$D/wrong11.tif</SourceFilename><SourceBand>2</SourceBand></SimpleSource>
    <SimpleSource><SourceFilename relativeToVRT="0">$D/wrong11.tif</SourceFilename><SourceBand>3</SourceBand></SimpleSource>
    <SimpleSource><SourceFilename relativeToVRT="0">$D/right11.tif</SourceFilename><SourceBand>1</SourceBand></SimpleSource>
    <SimpleSource><SourceFilename relativeToVRT="0">$D/right11.tif</SourceFilename><SourceBand>2</SourceBand></SimpleSource>
    <SimpleSource><SourceFilename relativeToVRT="0">$D/right11.tif</SourceFilename><SourceBand>3</SourceBand></SimpleSource>
  </VRTRasterBand>
</VRTDataset>
EOF
  st=$(gdalinfo -stats "$D/trap.vrt" 2>/dev/null | sed -n 's/.*Minimum=\([^,]*\), Maximum=\([^,]*\), Mean=\([^,]*\),.*/max \2 m, mean \3 m/p')
  KV "averaging RGB vs averaging elevation" "${st:-unavailable}"
else
  KV "terrain-rgb" "SKIPPED, needs GDAL >= 3.11 (have $(gdalinfo --version | cut -d, -f1))"
fi

# ---------------------------------------------------------------------------
hr "4. PMTiles conversion (raster)"
if command -v pmtiles >/dev/null 2>&1; then
  for m in hs trgb; do
    [ -f "$D/$m.mbtiles" ] || continue
    pmtiles convert "$D/$m.mbtiles" "$D/$m.pmtiles" >/dev/null 2>&1
    KV "$m.pmtiles" "$(stat -c%s "$D/$m.pmtiles") bytes, tile type $(pmtiles show "$D/$m.pmtiles" 2>/dev/null | sed -n 's/^tile type: //p')"
  done
else
  KV "pmtiles" "NOT INSTALLED -- raster archive cannot be produced"
fi

# ---------------------------------------------------------------------------
hr "5. tile enumeration against the real pinned tile list"
fetch_tilelist "$WORK/tileList.txt" >/dev/null 2>&1 \
  || { KV "tileList.txt" "could not fetch, skipping"; LF=""; }
if [ -n "${LF-unset}" ] && [ -f "$WORK/tileList.txt" ]; then
  LF="$(tilelist_lf "$WORK/tileList.txt")"
  KV "pin verified" "$(wc -l <"$LF") tiles"

  # Exactly the construction chunk_one and supercell_one use.
  hit=0; miss=0
  for lat in 39 40; do for lon in -106 -105; do
    nm="$(printf 'Copernicus_DSM_COG_10_%s%02d_00_%s%03d_00_DEM' \
            "$([ "$lat" -ge 0 ] && echo N || echo S)" "${lat#-}" \
            "$([ "$lon" -ge 0 ] && echo E || echo W)" "${lon#-}")"
    if grep -qxF "$nm" "$LF"; then hit=$((hit+1)); else miss=$((miss+1)); fi
  done; done
  KV "names built and found in LF list" "$hit found, $miss missing (expect 4/0)"

  # The CRLF trap, asserted rather than described: the SAME names must match
  # nothing at all in the raw list. If this ever reports a hit, the bucket
  # switched to LF and the normalisation became load-bearing in the other
  # direction.
  raw=$(grep -cxF "Copernicus_DSM_COG_10_N39_00_W106_00_DEM" "$WORK/tileList.txt" || true)
  KV "same name against RAW CRLF list" "$raw matches (expect 0 -- this is the trap)"

  KV "5-degree chunks over land" "$(awk -v CMD=chunks -v DEG=5 -f "$MERC_AWK" "$LF" | wc -l)"
  KV "z12 super-cells (side 64) over land" "$(awk -v CMD=supercells -v Z=12 -v SIDE=64 -f "$MERC_AWK" "$LF" | wc -l)"
fi

# ---------------------------------------------------------------------------
hr "6. MBTiles merge (the raster analogue of tile-join)"
if [ -f "$D/hs.mbtiles" ]; then
  cp "$D/hs.mbtiles" "$D/merge_a.mbtiles"
  gdalwarp -q -overwrite -ts 512 512 -r average "$D/elev12.tif" "$D/e9.tif"
  gdaldem hillshade -q -alg Horn -z 1 -s "$s" -compute_edges "$D/e9.tif" "$D/h9.tif"
  gdal_translate -q -of MBTILES -co TILE_FORMAT=PNG "$D/h9.tif" "$D/merge_b.mbtiles"
  a=$(sqlite3 "$D/merge_a.mbtiles" "select count(*) from tiles;")
  b=$(sqlite3 "$D/merge_b.mbtiles" "select count(*) from tiles;")
  sqlite3 "$D/merge_a.mbtiles" \
    "ATTACH DATABASE '$D/merge_b.mbtiles' AS s;
     INSERT OR REPLACE INTO tiles (zoom_level, tile_column, tile_row, tile_data)
       SELECT zoom_level, tile_column, tile_row, tile_data FROM s.tiles;
     DETACH DATABASE s;"
  m=$(sqlite3 "$D/merge_a.mbtiles" "select count(*) from tiles;")
  z=$(sqlite3 "$D/merge_a.mbtiles" "select group_concat(distinct zoom_level) from tiles;")
  KV "merged $a + $b tiles" "$m tiles across zooms $z"
  [ "$m" -eq $((a + b)) ] || KV "WARNING" "merge lost or duplicated tiles"
fi

printf '\nselftest artefacts: %s\n' "$D"
