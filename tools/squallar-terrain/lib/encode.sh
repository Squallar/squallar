# Elevation -> displayable raster, with no Python and no third-party encoder.
#
# ---------------------------------------------------------------------------
# WHY NOT THE OBVIOUS OPTIONS
#
# `gdal_translate -of MBTILES -co ELEVATION_TYPE=terrain-rgb` does NOT encode.
# It writes `elevation_type=terrain-rgb` into the MBTiles metadata table and
# nothing else. Measured on GDAL 3.13.3 with the N39 W106 probe, from both a
# native EPSG:4326 Float32 DEM and a reprojected EPSG:3857 one: the tiles come
# back 2-band grey+alpha Byte, 700 bytes each, values 0..255 -- gdal_translate
# rescaled Float32 into Byte on the way in and the option never got a chance to
# act. Exit status 0. Feed the same driver an ALREADY-encoded 3-band RGB and it
# passes it through correctly (97 KB, 3 bands), which is what this file
# produces, so the option is a LABEL for pre-encoded input, not an encoder.
# The Create() path is at least honest: `gdalwarp -of MBTILES` on Float32 fails
# outright with "ERROR 6: Only Byte supported".
#
# `rio rgbify` is the usual answer and is not taken: PyPI 0.4.0 dates from
# April 2022 and the repository's recent traffic is dependency bots. That is not
# a dependency to put under a shipped artifact.
#
# So the encoding is done with a VRT expression pixel function, which is pure
# GDAL C++. muparser has no floor(), but floor is exactly recoverable as
#
#     fl(x) = rint(x) - (rint(x) > x)
#
# since the comparison yields 1.0 or 0.0. Verified against a numpy reference
# over all 12,960,000 pixels of the probe tile: identical except on 2,893
# pixels (0.022%) whose exact packed value is a half-integer, where numpy's
# banker's rounding and muparser's rint break the tie in opposite directions.
# Both sit exactly half a quantum (0.05 m) from the true value, so the two
# encoders are equally correct and neither is preferable.
#
# VERSION GATE: expression pixel functions landed in GDAL 3.11.0. Amazon Linux
# 2023 ships gdal310 = GDAL 3.10.3, where this is unavailable at ANY spelling.
# `require_terrain_rgb_support` turns that into a clear refusal rather than a
# silent fallback.
# ---------------------------------------------------------------------------

gdal_version_atleast() {
  local want="$1" have
  have="$(gdalinfo --version | sed -n 's/^GDAL \([0-9]*\.[0-9]*\.[0-9]*\).*/\1/p')"
  [ -n "$have" ] || return 1
  printf '%s\n%s\n' "$want" "$have" | sort -V -C
}

require_terrain_rgb_support() {
  gdal_version_atleast 3.11.0 && return 0
  die "RASTER_ENCODING=terrain-rgb needs GDAL >= 3.11 for VRT expression pixel
     functions; this host has $(gdalinfo --version | cut -d, -f1).
     Amazon Linux 2023's gdal310 package is GDAL 3.10.3 and cannot do it.
     Either run RASTER_ENCODING=hillshade, which needs only gdaldem and is the
     default, or run the build inside ghcr.io/osgeo/gdal:ubuntu-small-3.13.2."
}

# make_terrain_rgb_vrt <src> <out.vrt> <xmin> <ymin> <xmax> <ymax> <nx> <ny>
#
# Mapbox Terrain-RGB v1:  height = -10000 + (R*65536 + G*256 + B) * 0.1
# so, inverting, with v the packed 24-bit integer:
#     v = round((height + 10000) / 0.1)   clamped to 0 .. 16777215
#     R = fl(v/65536)   G = fl(v/256) - 256*R   B = v - 256*fl(v/256)
#
# The geotransform is written from the numbers this build passed to gdalwarp
# rather than parsed back out of the source, so the grid stays exact.
make_terrain_rgb_vrt() {
  local src="$1" out="$2" xmin="$3" ymin="$4" xmax="$5" ymax="$6" nx="$7" ny="$8"
  local v='min(max(rint((B1+10000)*10),0),16777215)'
  local fv="(rint($v) - (rint($v) > ($v)))"
  local v256="(rint($v/256) - (rint($v/256) > ($v/256)))"
  local v65k="(rint($v/65536) - (rint($v/65536) > ($v/65536)))"
  local resx resy
  resx="$(awk -v a="$xmax" -v b="$xmin" -v n="$nx" 'BEGIN{printf "%.12f",(a-b)/n}')"
  resy="$(awk -v a="$ymax" -v b="$ymin" -v n="$ny" 'BEGIN{printf "%.12f",(a-b)/n}')"

  {
    printf '<VRTDataset rasterXSize="%s" rasterYSize="%s">\n' "$nx" "$ny"
    printf '  <SRS>EPSG:3857</SRS>\n'
    printf '  <GeoTransform>%s, %s, 0, %s, 0, -%s</GeoTransform>\n' \
      "$xmin" "$resx" "$ymax" "$resy"
    _trgb_band 1 Red   "$v65k"                "$src" "$nx" "$ny"
    _trgb_band 2 Green "$v256 - 256*$v65k"    "$src" "$nx" "$ny"
    _trgb_band 3 Blue  "$v - 256*$v256"       "$src" "$nx" "$ny"
    printf '</VRTDataset>\n'
  } >"$out"
}

_trgb_band() {
  local band="$1" ci="$2" expr="$3" src="$4" nx="$5" ny="$6"
  # Only & and < are special inside an XML attribute value. `>` is NOT, and
  # escaping it is actively wrong here: GDAL's CPL XML reader hands the
  # attribute to muparser without expanding entities, so a `&gt;` arrives as the
  # literal text and muparser fails with `Unexpected token "gt"`. Every floor in
  # the encoding contains a `>`, so this is not a corner case.
  expr="${expr//&/&amp;}"; expr="${expr//</&lt;}"
  cat <<EOF
  <VRTRasterBand dataType="Byte" band="$band" subClass="VRTDerivedRasterBand">
    <ColorInterp>$ci</ColorInterp>
    <PixelFunctionType>expression</PixelFunctionType>
    <PixelFunctionArguments expression="$expr" dialect="muparser"/>
    <SourceTransferType>Float64</SourceTransferType>
    <SimpleSource>
      <SourceFilename relativeToVRT="0">$src</SourceFilename>
      <SourceBand>1</SourceBand>
      <SrcRect xOff="0" yOff="0" xSize="$nx" ySize="$ny"/>
      <DstRect xOff="0" yOff="0" xSize="$nx" ySize="$ny"/>
    </SimpleSource>
  </VRTRasterBand>
EOF
}
