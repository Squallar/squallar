# squallar-elevation test fixtures

## `terrain-rgb-z10-210-391.png` — **do not re-encode or optimise**

96,049 bytes, 256×256, PNG colour type 2 (8-bit RGB), no interlace.
`md5 228fc2cb6d8ab1c459a57503c3f28984`.

A real Terrain-RGB tile: WebMercatorQuad z10, x 210, y 391 (XYZ), covering
−106.171875°..−105.8203125° longitude and 38.822591°..39.095963° latitude —
the Colorado Rockies west of Denver. Its heights run 2396.2 m to 4053.7 m.

**Provenance.** Produced 2026-08-30 by the shipped builder, unmodified, from the
real Copernicus GLO-30 bucket:

```sh
cd tools/squallar-terrain
WORK=… RASTER_ENCODING=terrain-rgb RASTER_MINZOOM=10 RASTER_MAXZOOM=10 \
SUPERCELL=1 ONLY_SUPERCELL=sc_z10_000210_000391 JOBS=4 \
cargo run --release -- build raster
# then: sqlite3 raster-all.mbtiles \
#   "select writefile('…/terrain-rgb-z10-210-391.png', tile_data) from tiles;"
```

So the bytes are `gdalwarp` → `trgb::pack_stream` → `gdal_translate -of MBTILES
-co TILE_FORMAT=PNG` output, not a synthesis. That is the whole point of the
fixture: it is what the archive will actually contain.

**Why "do not re-encode or optimise".** `squallar-egui/testdata/README.md`
carries the same instruction for the opposite reason — its hillshade tile is
*load-bearingly lossy*, and re-encoding would change what a lossy decode is
being pinned against. This one is **load-bearingly lossless**. Terrain-RGB is a
base-256 positional number: one count of R is 6553.6 m, and any re-encoding that
touches a pixel value — a palette pass, a bit-depth reduction, a quantiser —
turns metres into kilometres. `oxipng`, `pngcrush`, `zopflipng` and friends are
lossless on pixel values, but they change the byte length, and
`the_committed_real_tile_decodes_to_its_recorded_heights` asserts that length
first precisely so a tree-wide "optimise the PNGs" pass reddens rather than
quietly replacing the builder's output with something else's.

**What it pins.** That this crate's decode path — 8-bit RGB only, `unpack` on
whole triples, never on averaged digits — reproduces heights an independent
decoder (PIL + numpy, recorded in the test) read from the same bytes.
