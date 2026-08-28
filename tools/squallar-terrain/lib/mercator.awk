# WebMercatorQuad grid arithmetic for the terrain build.
#
# awk rather than Python so the whole pipeline needs nothing beyond GDAL's C
# binaries, coreutils and tippecanoe. On Amazon Linux 2023 that is the
# difference between `dnf install gdal310` and additionally needing
# `gdal310-python-tools` + `python3-gdal310` + numpy for arithmetic this simple.
#
# Every raster chunk has to be warped onto the exact pixel grid a
# WebMercatorQuad pyramid uses, or the tiles one chunk contributes will not line
# up with its neighbour's and the seams show.
#
# Degree cells are the right unit for the contour pass, which works in the DEM's
# own EPSG:4326 grid. They are the WRONG unit for the raster pass: Mercator
# stretches vertically by sec(lat), so a 5x5 degree cell at 80N is ~5.7x taller
# in pixels than the same cell at the equator -- 14564 x 83000 px at z12, 4.8 TB
# as Float32 for ONE chunk. Raster chunks therefore count TILES, which is
# uniform everywhere on the globe by construction.
#
# Usage (CMD is set with -v):
#   extent       Z W S E N            -> "xmin ymin xmax ymax nx ny"
#   tile-extent  Z TX0 TY0 TX1 TY1    -> "xmin ymin xmax ymax nx ny"
#   bbox         Z TX0 TY0 TX1 TY1    -> "w s e n clat"  (lon/lat of a tile range)
#   chunks       DEG   < tileList     -> "W S E N name" per populated cell
#   supercells   Z SIDE < tileList    -> "TX0 TY0 TX1 TY1 name" per land block

function abs(x)   { return x < 0 ? -x : x }
function fl(x)    { return x >= 0 ? int(x) : (x == int(x) ? x : int(x) - 1) }
function cl(x)    { return -fl(-x) }
function tan(x)   { return sin(x) / cos(x) }
function atan(x)  { return atan2(x, 1) }
function pow2(z,  i, r) { r = 1; for (i = 0; i < z; i++) r *= 2; return r }

function lon_to_x(lon) { return lon * PI / 180 * R }
function lat_to_y(lat) {
  if (lat >  LATLIM) lat =  LATLIM
  if (lat < -LATLIM) lat = -LATLIM
  return log(tan(PI / 4 + lat * PI / 360)) * R
}
function x_to_lon(x) { return x / R * 180 / PI }
function y_to_lat(y) { return (2 * atan(exp(y / R)) - PI / 2) * 180 / PI }

function clampt(v, hi) { return v < 0 ? 0 : (v > hi ? hi : v) }

# Fills TX0/TY0/TX1/TY1 with the inclusive tile range covering a lon/lat box.
function tile_range(z, w, s, e, n,   span, hi) {
  span = WORLD / pow2(z)
  hi = pow2(z) - 1
  TX0 = clampt(fl((lon_to_x(w) + ORIGIN) / span), hi)
  TX1 = clampt(cl((lon_to_x(e) + ORIGIN) / span) - 1, hi)
  TY0 = clampt(fl((ORIGIN - lat_to_y(n)) / span), hi)
  TY1 = clampt(cl((ORIGIN - lat_to_y(s)) / span) - 1, hi)
  if (TX1 < TX0) TX1 = TX0
  if (TY1 < TY0) TY1 = TY0
}

function emit_extent(z, tx0, ty0, tx1, ty1,   span) {
  span = WORLD / pow2(z)
  printf "%.10f %.10f %.10f %.10f %d %d\n",
    -ORIGIN + tx0 * span, ORIGIN - (ty1 + 1) * span,
    -ORIGIN + (tx1 + 1) * span, ORIGIN - ty0 * span,
    (tx1 - tx0 + 1) * 256, (ty1 - ty0 + 1) * 256
}

function emit_bbox(z, tx0, ty0, tx1, ty1,   span, s, n) {
  span = WORLD / pow2(z)
  s = y_to_lat(ORIGIN - (ty1 + 1) * span)
  n = y_to_lat(ORIGIN - ty0 * span)
  printf "%d %d %d %d %.6f\n",
    fl(x_to_lon(-ORIGIN + tx0 * span)) - 1, fl(s) - 1,
    cl(x_to_lon(-ORIGIN + (tx1 + 1) * span)) + 1, cl(n) + 1, (s + n) / 2
}

# Copernicus tile name -> LAT/LON of the 1x1 degree cell's SW corner.
#
#   Copernicus_DSM_COG_10_N39_00_W106_00_DEM
#                         ^^^^^^ ^^^^^^^
# Latitude is always 2 digits and longitude always 3, so the fields sit at fixed
# offsets from the end: hemisphere/digits at -17/-16 and -10/-9 respectively.
function parse_tile(name,   L) {
  if (name !~ /_[NS][0-9][0-9]_00_[EW][0-9][0-9][0-9]_00_DEM$/) return 0
  L = length(name)
  LAT = substr(name, L - 16, 2) + 0
  if (substr(name, L - 17, 1) == "S") LAT = -LAT
  LON = substr(name, L - 9, 3) + 0
  if (substr(name, L - 10, 1) == "W") LON = -LON
  return 1
}

BEGIN {
  PI = 3.14159265358979323846
  R = 6378137.0
  ORIGIN = PI * R
  WORLD = 2 * ORIGIN
  LATLIM = 85.0511287798066

  if (CMD == "extent") {
    tile_range(Z + 0, W + 0, S + 0, E + 0, N + 0)
    emit_extent(Z + 0, TX0, TY0, TX1, TY1); exit
  }
  if (CMD == "tile-extent") { emit_extent(Z+0, TX0+0, TY0+0, TX1+0, TY1+0); exit }
  if (CMD == "bbox")        { emit_bbox(Z+0, TX0+0, TY0+0, TX1+0, TY1+0); exit }
}

# --- streaming commands read tileList on stdin --------------------------------
CMD == "chunks" {
  if (!parse_tile($1)) next
  seen[fl(LON / DEG) * DEG " " fl(LAT / DEG) * DEG] = 1
}

CMD == "supercells" {
  if (!parse_tile($1)) next
  tile_range(Z + 0, LON, LAT, LON + 1, LAT + 1)
  for (tx = fl(TX0 / SIDE); tx <= fl(TX1 / SIDE); tx++)
    for (ty = fl(TY0 / SIDE); ty <= fl(TY1 / SIDE); ty++)
      block[tx " " ty] = 1
}

END {
  if (CMD == "chunks") {
    for (k in seen) {
      split(k, a, " ")
      printf "%d %d %d %d chunk_%s%03d_%s%02d\n", a[1], a[2],
        a[1] + DEG, a[2] + DEG,
        (a[1] >= 0 ? "E" : "W"), abs(a[1]), (a[2] >= 0 ? "N" : "S"), abs(a[2])
    }
  }
  if (CMD == "supercells") {
    hi = pow2(Z + 0) - 1
    for (k in block) {
      split(k, a, " ")
      tx0 = a[1] * SIDE; ty0 = a[2] * SIDE
      tx1 = tx0 + SIDE - 1; ty1 = ty0 + SIDE - 1
      if (tx1 > hi) tx1 = hi
      if (ty1 > hi) ty1 = hi
      printf "%d %d %d %d sc_z%d_%06d_%06d\n", tx0, ty0, tx1, ty1, Z + 0, tx0, ty0
    }
  }
}
