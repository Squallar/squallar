# squallar-terrain

A one-shot builder that turns the Copernicus GLO-30 DEM into **two** PMTiles v3
archives:

| archive | tiles | default | what it is |
|---|---|---|---|
| `squallar-contours.pmtiles` | MVT (vector) | always | elevation contour lines, layer `contour`, attribute `elev` |
| `squallar-terrain-hillshade.pmtiles` | PNG (raster) | `RASTER_ENCODING=hillshade` | shaded relief |
| `squallar-terrain-terrain-rgb.pmtiles` | PNG (raster) | `RASTER_ENCODING=terrain-rgb` | Mapbox Terrain-RGB v1 encoded elevation |

**One job**, because both artifacts derive from the same ~1.5 TB of COGs and the
download is the slow part. **Two archives**, because PMTiles v3 stores a single
`tile_type` byte for the whole file — MVT and PNG cannot share a container. That
is a format constraint, not a packaging preference.

**Global, not CONUS.** The basemap is global; terrain that vanishes when you pan
to the Alps is the basemap contradicting itself.

**One-shot.** The DEM does not change. This runs once, is pinned, and never
joins the 35-day OSM cycle.

## What this is

One Rust binary plus one shell script. The binary orchestrates; GDAL's C
binaries, tippecanoe and go-pmtiles do the heavy lifting and are shelled out to.
There is **no Python**, and the only Rust dependency is `squallar-geo` — the
app's own Web Mercator, so a tile this build writes carries the address the app
asks for.

```
Cargo.toml               its own workspace; NOT a member of the app's
src/main.rs              CLI: `build`, plus the grid arithmetic for inspection
src/config.rs            pins, contour schedule, layout, the harmonic guard
src/grid.rs              WebMercatorQuad arithmetic over squallar-geo
src/tiles.rs             Copernicus tile names, the pinned list, chunking
src/trgb.rs              Mapbox Terrain-RGB v1 packing
src/raster.rs            the raster archive
src/contours.rs          the vector archive
src/floor.rs             the 1x1-degree minimum-elevation grid the app compiles in
src/run.rs               subprocess pipelines with per-member exit status
src/mbtiles.rs           MBTiles merge and metadata, via sqlite3
src/pmtiles.rs           the magic-bytes assertion
src/md5.rs               RFC 1321, for the tileList pin
tests/pipeline.rs        end-to-end against GDAL and a real tile (#[ignore]d)
bootstrap-al2023.sh      toolchain + EC2 user-data; the only shell left
```

An earlier design was all shell, awk and a GDAL VRT expression. Three things
moved it:

* **The tile-name parser was written twice and wrong twice.** In bash, `N08` and
  `W006` are invalid octal without `10#`. In awk, the fields were read at fixed
  character offsets counted from the end of the string, so any name of the right
  length parsed to *something* rather than being rejected.
* **The Terrain-RGB encoder needed GDAL ≥ 3.11 for VRT expression pixel
  functions, and the version assertion could not see the real requirement**:
  `vrtexpression_muparser.cpp` compiles only under `GDAL_USE_MUPARSER`, so a
  GDAL reporting 3.13.3 passes the check and the expression still fails at
  runtime. Packing in-process removes the gate rather than tightening it, and
  the build runs on AL2023's stock GDAL 3.10.3 again.
* **A shell pipeline reports one status for the whole pipe.** A flat chunk has
  no 1000 m contour and tippecanoe exits non-zero saying so — that is data, not
  failure. But if `gdal_contour` dies instead, tippecanoe reads nothing and
  prints the *same* message, and the shell recorded the chunk as flat.
  `src/run.rs` waits on each producer separately, so that branch is only taken
  once every producer is known to have exited 0.

What did **not** move: `gdal_contour … /vsistdout/ | tippecanoe` is still one
stream with nothing on disk. `Pipeline::feed` hands the producer's stdout the
consumer's stdin file descriptor, so it is the same zero-copy pipe a shell
makes.

The one thing the awk got right and this does not do differently: its Web
Mercator was `ln(tan(π/4 + φ/2))`, which is the well-conditioned form, not the
`ln(tan φ + sec φ)` that cancels south of the equator. Over this build's domain
the two forms agree to 3.7 nanometres, and the `chunks` and `supercells`
enumerations were byte-identical when compared before the awk was deleted.
`bbox` was not: where a tile edge falls on a whole degree, a float round trip
through metres made the awk's margin one degree wider than intended — 15 of 140
integer fields over 35 real z12 super-cells, always in the conservative
direction, so it cost work rather than data.

---

## Pinning the DEM---

## Pinning the DEM

Source is `s3://copernicus-dem-30m/` (region `eu-central-1`, anonymously
readable). Key layout, confirmed live:

```
Copernicus_DSM_COG_10_N39_00_W106_00_DEM/Copernicus_DSM_COG_10_N39_00_W106_00_DEM.tif
```

The doubled directory/file name is real, and the `_10_` is **resolution in arc
seconds, not metres** — `10` is GLO-30, `30` is GLO-90.

**The bucket cannot be pinned the way `planetiler_version` is pinned, and
pretending otherwise would be the lie.** Specifically:

* There is no release identifier anywhere in the bucket — not in the name, not
  in any key, not in `tileList.txt`, not in the per-tile ISO 19115 XML (which
  carries only acquisition dates). The AWS Open Data registry entry is the only
  place the release appears at all, and it says the data "comes from Copernicus
  DEM 2021 release".
* **S3 versioning is off.** Every object answers `x-amz-version-id: null`, so
  there is no version id to pin to.
* The registry's `UpdateFrequency` reads *"None, except GLO-30 Public can be
  updated if the public tile list changes"* — the tile list is explicitly
  declared mutable, as countries release previously withheld tiles.

So the elevation values are fixed and the only thing that can move under a
rerun is the **set of tiles**. That makes `tileList.txt` the pin, and it is
pinned by content in `src/config.rs`:

```sh
DEM_RELEASE="COP-DEM_GLO-30 Public, 2021 release"
DEM_TILELIST_MD5="637fe75ddf7615ba853dd83caf05cd82"   # observed 2026-08-27
DEM_TILELIST_COUNT="26450"
DEM_TILELIST_BYTES="1110900"
```

`verify_tilelist` fails the build if any of those move. That detects drift; it
cannot prevent it. **If byte-identical reproducibility ever actually matters,
the only real fix is to copy the tiles into our own versioned bucket** — the
alternative sources (ESA PRISM, which does have explicit `2019_1`…`2024_1`
release directories, and Google Earth Engine) are slower, need registration,
and are a different product: AWS re-processed the ESA originals, stripping the
shared edge row/column and re-encoding as COG, so a byte comparison against ESA
fails by construction.

Note also that the AWS bucket is the **2021** release while ESA is at 2023_1 /
2024_1. "Pin the AWS bucket" and "pin a current release" are different goals.

### tileList.txt is CRLF

All 26,450 lines end `\r\n`. Verified, not assumed. Every key built by pasting a
raw line into a URL carries a trailing `\r` and 404s, and
`grep -qxF "$name" tileList.txt` matches **nothing**. `verify_tilelist` hashes
the raw CRLF bytes (that is what the bucket serves) and then writes a
LF-normalised `tileList.txt.lf` that everything else reads.

### Attribution

The **modified-work** form applies, because tiling and encoding make these
archives a derivative:

> produced using Copernicus WorldDEM-30 © DLR e.V. 2010-2014 and © Airbus
> Defence and Space GmbH 2014-2018 provided under COPERNICUS by the European
> Union and ESA; all rights reserved

It is written into both archives' metadata so it travels with the artifact.

---

## Contour interval schedule

Schedule **A**, and it is **harmonic**:

| zooms | interval |
|---|---|
| z10 | 1000 m |
| z11–z12 | 200 m |
| z13–z14 | 100 m |

**Each interval must divide the coarser one exactly** (1000/200 = 5,
200/100 = 2). This is not stylistic.

If the schedule is not harmonic, contours **vanish as you zoom in**. A 500 m
schedule stepping to 200 m loses the 500 m line at z11, because 500 is not a
multiple of 200 — the line has no counterpart in the finer band, so crossing
that zoom makes an existing line disappear. That reads as a rendering bug.

`verify_schedule` enforces divisibility on **every build**, not just when the
schedule is edited, so an edit cannot land without tripping it.
`config::tests` asserts both that schedule A is accepted and that
1000/500/200 is rejected — a guard nobody has watched fail is not a guard.

Schedule A is also *smaller* than uniform 100 m at identical z13–z14 detail,
because the low zooms stop carrying unreadable lines. Measured, not assumed —
see below.

---

## Measured, on one real degree tile

Run: `cargo test -- --ignored`. Everything here is from `N39_00_W106_00`
(Colorado, 2,794 m relief) unless noted, on GDAL 3.13.3 / tippecanoe 2.79.0.

### Contours

| band | interval | bytes | time |
|---|---|---|---|
| z10 | 1000 m | 42,028 | 0.50 s |
| z11–z12 | 200 m | 907,750 | 1.47 s |
| z13–z14 | 100 m | 3,890,114 | 2.78 s |
| **joined archive** | | **4,838,653  (4.84 MB)** | |
| uniform 100 m z10–z14, same z13–14 detail | | 6,140,629  (6.14 MB) | |

Schedule A costs **79%** of uniform 100 m at identical fine detail.

### Raster, z12, same tile

| artifact | bytes |
|---|---|
| hillshade PMTiles (PNG) | 6,104,843 |
| hillshade MBTiles (WebP q75) | 1,728,512 |
| terrain-RGB PMTiles (PNG) | 14,190,386 |
| terrain-RGB round-trip error | max 0.050 m, mean 0.020 m |

Round-trip error is exactly half the 0.1 m encoding quantum, which is the best
achievable.

### The flat/steep bracket

Two tiles, because one tile is not a range:

| tile | relief | contours A | hillshade PNG | hillshade WebP q75 | terrain-RGB PNG |
|---|---|---|---|---|---|
| N39 W106 (Colorado) | 2,794 m | 4,838,650 | 6,258,688 | 1,728,512 | 14,385,152 |
| N35 W098 (Oklahoma) | 201 m | 1,277,821 | 4,636,672 | 946,176 | 9,785,344 |

The Oklahoma tile has **no 1000 m contour at all** — its highest ground is
201 m. An empty band is treated as data rather than failure; this is common,
since most of the planet's land is roughly flat.

### Extrapolating to the globe

**Denominator, stated:** 9,117,681 z12 tiles cover land (counted from the pinned
tile list, deduplicated), and a z0–z12 pyramid is 4/3 of its base, so
≈ 12.16 M raster tiles. These are **extrapolations from two US tiles**, not
measurements of the globe:

| archive | per-tile | global estimate |
|---|---|---|
| contours (26,450 degree tiles) | 1.28–4.84 MB | **34–128 GB** |
| hillshade PNG | 23.8–30.1 kB | **289–366 GB** |
| hillshade WebP q75 | 4.85–8.31 kB | **59–101 GB** |
| terrain-RGB PNG | 50.2–69.2 kB | **610–841 GB** |

> **The WebP figures on this page are q75, and were labelled q85 until
> 2026-08-29.** The code asked for quality 85 with `-co WEBP_LEVEL=85`, but
> `WEBP_LEVEL` is a GTiff creation option — the MBTiles driver's is `QUALITY` —
> so GDAL dropped it and wrote every tile at its default of 75. It said so, once
> per super-cell: `Warning 6: driver MBTiles does not support creation option
> WEBP_LEVEL`. The option is now `QUALITY=85`, so **a future build will produce
> larger tiles than every WebP number here.** They are kept as measured rather
> than re-labelled with a setting that never applied. The q75 output was
> inspected and does not visibly band: 185 distinct grey levels in a 256×256 z12
> tile over Colorado.

The low end is the more representative one: much of the world's land is
flat, desert or ice-sheet, all of which compress far better than either US
sample. Treat the high end as a headroom figure.

**The contour archive is big** — plausibly 50–70 GB — and the z13–z14 100 m band
is 80% of it. If that turns out to be too large to serve, the lever is dropping
z14 or coarsening the finest band, not changing the harmonic structure.

---

## Peak disk

The launch template stripes all instance-store volumes: 2,850 GB on
m6id.12xlarge, 5,700 GB on c6id.24xlarge. It does not all fit at once, so both
passes chunk, and delete intermediates as soon as the chunk that owns them is
tiled.

**The DEM is never held whole.** Contours fetch a degree tile, use it, and drop
it. The raster pass never downloads at all — it reads COGs through `/vsis3/`.

### Contours, per chunk (`CHUNK_DEG=5`, so ≤ 25 degree tiles)

| item | worst case |
|---|---|
| staged DEM GeoTIFFs | 25 × 39 MB = 975 MB |
| tippecanoe temp (`-t`) | ~1 GB |
| chunk output archive | ~121 MB |
| **per chunk in flight** | **~2.1 GB** |

× `JOBS` (48 on m6id.12xlarge) ≈ **100 GB**, plus the accumulating chunk parts
(up to the full archive) and one join generation, which transiently doubles
them: **≈ 260 GB peak** for the contour pass.

**No GeoJSON ever touches disk.** `gdal_contour -f GeoJSONSeq … /vsistdout/`
pipes straight into tippecanoe. This is the single biggest departure from the
obvious pipeline: the two-step `gdal_contour` → GPKG → `ogr2ogr` → GeoJSON route
costs **66.7 MB per degree tile** at 100 m (19.7 MB GPKG + 47.0 MB GeoJSON),
which globally is the 0.28–1.24 TB of intermediates the naive design has to find
room for. Streaming makes that number **zero**. Even writing GeoJSONSeq to a
file instead of GPKG+GeoJSON is 29.7 MB, a 2.2× saving, so the one-step format
is worth it on its own.

### Raster

| item | worst case |
|---|---|
| global z8 elevation raster | 65,536² Float32 = 17.2 GB (≈ 5 GB on disk, DEFLATE + predictor 3) |
| per super-cell (64×64 tiles = 16,384² px), hillshade | 1.07 GB Float32 (DEFLATE, ≈ 0.4 GB on disk) + encoded ≈ **0.7 GB** |
| per super-cell, terrain-RGB | 1.07 GB raw Float32 + 0.80 GB raw RGB = **1.87 GB** |
| × `SC_JOBS` = `JOBS/4` = 12 | ≈ 8 GB hillshade, ≈ 23 GB terrain-RGB |
| accumulating per-zoom MBTiles | full archive |
| final merge + `pmtiles convert` | **2 × full archive** (both files exist at once) |

So raster peak ≈ **2 × archive + ~50 GB**:

* hillshade WebP: ≈ **250 GB**
* hillshade PNG: ≈ **780 GB**
* terrain-RGB PNG: ≈ **1.75 TB** — fits m6id.12xlarge's 2,850 GB, but without
  much room; prefer c6id.24xlarge.

`pmtiles convert` also needs its own temp space for deduplication; point
`--tmpdir` at the stripe if the default `/tmp` is small.

`CHUNK_DEG` and `SUPERCELL` are the two dials. Contour peak scales with
`CHUNK_DEG²`; raster peak scales with `SUPERCELL²`.

---

## Why raster chunks count tiles, not degrees

Degree cells are right for the contour pass, which works in the DEM's own
EPSG:4326 grid. They are **wrong** for the raster pass, because Mercator
stretches vertically by sec(lat). A 5°×5° cell at 80°N is ~5.7× taller in pixels
than the same cell at the equator: 14,564 × 83,000 px at z12, which is **4.8 TB
as Float32 for one chunk**. Counting tiles instead makes every super-cell
identical in size everywhere on the globe by construction.

---

## The overview trap

**This is the most important correctness property in the build.**

`gdaladdo -r average` over a Terrain-RGB image averages R, G and B
**independently**. The encoding is a base-256 positional number, so averaging
the digits ignores every carry between them. Measured on the probe tile at a
single 2× reduction:

```
downsample elevation, then encode : max err     0.050 m   mean  0.025 m
average R,G,B (what gdaladdo does): max err  3289.691 m   mean 14.604 m
                                    14.5% of pixels wrong by more than 10 m
```

Reproduced independently through pure GDAL (max 3289.700 m, mean 13.009 m on
the Mercator-warped grid). It looks plausible and is garbage.

`-r nearest` is exactly correct — it copies one source triple verbatim — but
aliases badly on shaded relief.

The same argument applies to hillshade: a hillshade of a downsampled DEM is not
a downsampled hillshade, because the 3×3 slope window is just as non-linear.

**So every zoom is generated from elevation resampled to that zoom's resolution
and then encoded. Zooms are never derived from each other's pixels.** The cost
is a 1/(1−¼) = 1.33× multiplier over building only the deepest zoom.

---

## The raster path to PMTiles

Verified against GDAL 3.13.3 on this workstation, not assumed:

* **GDAL cannot write raster PMTiles at any released version.** Its PMTiles
  driver registers as `-vector-`; `gdalinfo --formats` confirms
  `PMTiles -vector- (rw+v)`. Every PMTiles entry in GDAL's NEWS through 3.13.3
  is MVT.
* **`gdal raster tile`** (GDAL 3.11+) writes a *directory tree* of tiles. No
  container output; it cannot emit PMTiles or MBTiles.
* **`gdal_translate -of MBTILES`** works and is the workhorse. It writes only
  the base zoom — but that is fine here, because the overview trap means each
  zoom is built separately anyway.
* **`pmtiles convert`** (go-pmtiles, pinned 1.31.2) does the MBTiles → PMTiles
  step, and does handle raster. It picks `tile_type` from the MBTiles
  `metadata` row named `format`; GDAL writes exactly `png`/`jpg`/`webp` there,
  which are exactly the strings go-pmtiles matches.

There is no raster equivalent of `tile-join`, so super-cells are merged at the
**MBTiles** level: MBTiles is SQLite, so `ATTACH` + `INSERT OR REPLACE` is the
whole operation.

`-co ZOOM_LEVEL` is advertised by the driver but **rejected by its CreateCopy
path** ("driver MBTiles does not support creation option ZOOM_LEVEL"). It is
unnecessary — the source is warped onto the target zoom's exact grid, so the
driver derives the zoom from the resolution. The build **asserts** the resulting
zoom rather than assuming it, because a silently misplaced zoom would put real
tiles at wrong addresses and still look like a successful build.

### tippecanoe picks its container from the file extension

`tile-join -o "$out.part"` writes an **MBTiles** file. tippecanoe and `tile-join`
choose PMTiles vs MBTiles from the output *extension*, so the conventional
write-to-temp-then-rename idiom produces a SQLite database with a `.pmtiles`
name: right name, plausible size, zero exit status, wrong format. Nothing
downstream complains until a viewer gets it.

This was caught by running the contour pass for real rather than by reading it
— the archive was 6,402,048 bytes instead of 4,838,849, and `pmtiles show`
printed nothing. Temp names keep the `.pmtiles` suffix, and
`pmtiles::assert_archive` checks the 7-byte `PMTiles` magic on every archive
the build emits, including each `tile-join` generation.

---

## Terrain-RGB without Python

`gdal_translate -of MBTILES -co ELEVATION_TYPE=terrain-rgb` **does not encode
anything.** It writes `elevation_type=terrain-rgb` into the MBTiles metadata
table and nothing else. Measured on GDAL 3.13.3, from both a native EPSG:4326
Float32 DEM *and* a reprojected EPSG:3857 one: the tiles come back **2-band
grey+alpha Byte, 700 bytes each, values 0–255**. `gdal_translate` rescaled
Float32 into Byte on the way in and the option never got a chance to act — and
it exits 0. Feed the same driver an already-encoded 3-band RGB and it passes it
through correctly (97 KB, 3 bands). **The option is a label for pre-encoded
input, not an encoder.** The `Create()` path is at least honest:
`gdalwarp -of MBTILES` on Float32 fails with `ERROR 6: Only Byte supported`.

`rio rgbify` is the usual answer and is **not** taken: PyPI 0.4.0 dates from
April 2022 and the repository's recent traffic is dependency bots. That is not a
dependency to put under a shipped artifact.

So the encoding is done **in this binary**, over a raw pipe of GDAL's own
making. `gdalwarp -of ENVI` writes a flat little-endian Float32 plane — header
offset 0, `data type = 4`, `byte order = 0`, exactly `nx·ny·4` bytes — the
packer reads it, writes interleaved RGB, and a `VRTRawRasterBand` VRT hands the
result back to `gdal_translate -of MBTILES`.

**It is a file, not a pipe, and that is not a preference.** Every GDAL raw
driver is `Create()`-based and needs a seekable output. Measured on GDAL 3.13.3:
`-of ENVI /vsistdout/` fails with `ERROR 6: Read or update mode not supported on
/vsistdout`, `/vsistdout_redirect/` fails with `ERROR 4: Attempt to create file
… failed`, and a FIFO hangs on the reopen. There is no `CreateCopy` raw Float32
driver to fall back to — `SRTMHGT` and `DTED` are Int16, `AAIGrid` and `GSAG`
are ASCII. `GTiff` with `-co STREAMABLE_OUTPUT=YES` *does* write to
`/vsistdout/`, but reading it means parsing TIFF IFDs.

One VRT detail that is easy to miss: `VRTRawRasterBand` refuses an **absolute**
`SourceFilename` unless `GDAL_VRT_RAWRASTERBAND_ALLOWED_SOURCE` is set, and
errors with *"is invalid because the relativeToVRT flag is not set"*. The VRT is
therefore written beside the raw file and names it by basename with
`relativeToVRT="1"`.

The packing itself:

```
v = round((h + 10000) * 10)   clamped to 0 ..= 16777215
R = v >> 16      G = (v >> 8) & 255      B = v & 255
```

`* 10`, not `/ 0.1`. The two are the same function in exact arithmetic and
different ones in binary — over 2,261,416 `f32` heights strided across −500 m to
9000 m they disagree on 98,304, i.e. **4.35%**.

**Differential against the encoder this replaced**, over all 12,960,000 pixels
of the probe tile, run before the muparser path was deleted:

| encoder | pixels differing from muparser |
|---|---|
| this binary | **0** |
| a half-to-even (numpy-style) control | 2,893 (0.0223%) |

5,908 pixels (0.046%) of the tile have an exactly-half-integer packed value, so
the tie rule is genuinely exercised — the zero is not vacuous, and the control
reproduces the 2,893 recorded when the muparser encoder was first checked
against numpy. Both tie rules land exactly half a quantum (0.05 m) from truth,
so neither is more correct; what matters is that the replacement is
byte-identical to what shipped.

### Why hillshade is the default

`gdaldem hillshade` is GDAL's own C, present in every GDAL including AL2023's
3.10.3, and has no encode step to get wrong. If the requirement is "the map
shows terrain", that is the entire job — and it is 2.3× smaller than terrain-RGB
as PNG, or 8.2× smaller as WebP q75, which a hillshade may safely use and
terrain-RGB may not.

Terrain-RGB is what you want when the **client** needs the elevation:
relighting at an arbitrary azimuth, exaggeration, elevation readout, or draping
the 3D volumetric view. That work is long-tail and unscheduled, so it does not
set the v1 dependency budget. The mode is implemented and verified, so switching
costs a rebuild, not a redesign.

**Terrain-RGB must be stored losslessly.** One count of error in the R channel
is 6,553.6 m, so lossy WebP or JPEG does not make it soft, it destroys it.
`verify_raster_settings` enforces PNG.

### Hillshade's one approximation

EPSG:3857 "metres" are not ground metres — they are inflated by 1/cos(lat), so a
slope computed against raw Mercator pixel spacing is too shallow by cos(lat), a
factor of 2 at 60°N. `gdaldem -s` takes one scalar, so the build passes each
super-cell's **centre latitude**. A super-cell spans little latitude at the
zooms where relief is legible, so the residual is small, but it is an
approximation and terrain-RGB does not have it (encoding is per-pixel).

`-compute_edges` is passed to stop a one-pixel dark frame at every super-cell
edge, which would otherwise draw a grid over the planet.

---

## Toolchain on Amazon Linux 2023

`bootstrap-al2023.sh` is both the toolchain installer and the **EC2 user-data**.
Its size is load-bearing: user-data is capped at 16,384 bytes after base64, and
the shell build this replaced was 23,964 — 1.46× over, which is what forced a
fetch-a-tarball design. The bootstrap alone is now 2,916 base64 characters
gzipped (6,168 plain), so it fits as the whole payload.

Facts, verified against AL2023 repo metadata:

* **The package is `gdal310`, not `gdal`.** No `gdal310*` package Provides a
  bare `gdal`, so `dnf install gdal` fails outright. It is in the **core** repo.
  `gdal310` carries `gdal_contour`, `gdaldem`, `gdal_translate`, `gdalwarp`,
  `gdalbuildvrt`. The binaries are unversioned (`/usr/bin/gdal_contour`); only
  the package name carries the suffix.
* **GDAL 3.10.3 is enough.** Nothing needs 3.11 any more; the Terrain-RGB
  encoding no longer goes through a VRT expression pixel function.
* **EPEL is not binary compatible with AL2023** and is not needed. AWS's SPAL
  repo does **not** carry GDAL, despite AWS's own SPAL documentation naming
  GDAL as an example of what SPAL unlocks.
* **`gdal310` only appeared around AL2023 release 2023.8.** AL2023 pins repos
  per release, so an older AMI sees zero `gdal*` packages. The bootstrap guards
  on this and tells you to `dnf upgrade --releasever=latest`.

`gdal310-python-tools` and `python3-gdal310` are deliberately **not**
dependencies, and neither is a Rust toolchain.

tippecanoe is not packaged on any RHEL-family distro and is built from a tag
(`TIPPECANOE_REF`, default 2.79.0) so a rebuild produces the same tiler.
go-pmtiles is fetched as a static binary at a pinned version; note its Linux
release assets use an underscore (`go-pmtiles_1.31.2_Linux_x86_64.tar.gz`) while
macOS uses a hyphen.

### Deploying the binary

The bootstrap fetches `squallar-terrain` from `TERRAIN_BIN_URL` rather than
building it, which is what keeps Rust off the instance. **Build it against musl,
not glibc.** AL2023 ships glibc 2.34; a binary linked against a newer glibc will
not start there, and the failure is a loader error with no hint about versions.

```sh
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
# upload target/x86_64-unknown-linux-musl/release/squallar-terrain somewhere
# the instance can reach, and pass its URL as TERRAIN_BIN_URL
```

Building in an `amazonlinux:2023` container against the stock toolchain is the
alternative if a static build is inconvenient. Neither has been executed on this
workstation: the musl target is not installed here.

---

## Running it

As user-data, the bootstrap runs the build itself:

```sh
TERRAIN_BIN_URL=https://…/squallar-terrain ./bootstrap-al2023.sh
```

By hand:

```sh
TERRAIN_NO_RUN=1 TERRAIN_BIN_URL=… ./bootstrap-al2023.sh
WORK=/mnt/terrain-work squallar-terrain build              # archives + floor grid
WORK=/mnt/terrain-work squallar-terrain build contours     # just the vector one
RASTER_ENCODING=terrain-rgb squallar-terrain build raster
WORK=/mnt/terrain-work squallar-terrain build floor        # just the floor grid
```

### The floor grid

`build floor` emits `squallar-min-elevation-1deg.bin` — 360 × 180 signed `i16`
metres, 129,600 bytes, big-endian, row 0 northernmost. The app compiles it in
and reads the minimum over a radar box's footprint to put the 3D volume's base
at the true ground rather than at sea level. Format, reader and builder all live
in `squallar_geo::min_elevation`; this pass only drives them, so writer and
reader cannot disagree about byte order or row origin.

**One grid cell is one COG**: GLO-30 publishes exactly one 1°×1° tile per
populated cell, so the pass takes each tile's minimum into the cell that tile
names. Cells GLO-30 does not publish — every ocean cell — keep the `i16::MIN`
sentinel, which is what lets the reader tell "no ground here" from "ground at
zero"; a coastal radar overlaps ocean on every scan and a minimum that adopted
the sentinel would answer −32,768 m.

It reads every COG at **full resolution**, because `gdalinfo -mm` is the only
exact minimum available. `-oo OVERVIEW_LEVEL=2` would read a sixty-fourth of the
bytes and is *wrong for this purpose, not merely coarse*: GLO-30's overviews are
averaged, so an overview's minimum sits above the true one, and a box floor
above the ground clips the ground. `-approx_stats` fails the same way. The pass
is therefore resumable through `floor-minima.txt` in the work directory: each
tile's answer is appended as it lands and a re-run skips what is recorded. It
refuses to write a grid unless every cell in the pinned tile list is accounted
for.

Three cells, measured 2026-08-30 with `gdalinfo -mm` over `/vsis3/`:

| cell | measured minimum | note |
|---|---:|---|
| `N36_00_W117_00` | −91.451 m | Death Valley. **Not** Badwater's surveyed −86 m — this is the lowest pixel in a whole degree cell of a surface model. |
| `N31_00_E035_00` | −427.834 m | The Dead Sea. |
| `N39_00_W106_00` | 1552.236 m | Colorado. Note the denominator: the committed z10 fixture tile over the same cell reads 2396.2 m, because it covers about a fortieth of it. A tile minimum is not a cell minimum. |

The first two are the pins the app-side reader carries.

Knobs (all environment, all listed by `squallar-terrain --help`):
`WORK` `OUT` `TMP` `JOBS` `DEM_BUCKET` `CHUNK_DEG` `SUPERCELL`
`RASTER_ENCODING` `RASTER_MINZOOM` `RASTER_MAXZOOM` `RASTER_GLOBAL_MAXZOOM`
`RASTER_TILE_FORMAT` `RASTER_BBOX`. `ONLY_CHUNK` and `ONLY_SUPERCELL` are
substring filters on the cell name, for smoke-testing one region or re-running
one that failed.

### Building a region

`RASTER_BBOX=west,south,east,north` in degrees clips the raster pass.
`RASTER_BBOX=-125,24,-66,50` is CONUS.

**`ONLY_SUPERCELL` cannot do this and never could.** It is
`name.contains(filter)` against a name like `sc_z11_000320_000640`, and a region
is a two-dimensional block range — CONUS at z11 with `SUPERCELL=64` is block
columns 5–10 by rows 10–13, whose names agree on neither field. No substring
selects them, so asking for a region with `ONLY_SUPERCELL` and no box either
misses most of it or builds the globe — the land subset of a 32×32 z11 block
grid rather than a 7×4 corner of it.
`tiles::tests::no_substring_filter_can_express_the_conus_block_range` proves the
substring claim exhaustively rather than by assertion.

The clip is per super-cell, not per tile — a block that overlaps the box at all
is built whole, so the region produced is the box rounded outward to super-cell
boundaries. A box that is inverted, degenerate or off the globe is refused by
`verify()`, and a box that contains no land at all is refused by the raster pass
before it warps anything, because a region with nothing in it is a typo and not
a build. `ONLY_SUPERCELL` is deliberately **not** held to that second rule: it
names one zoom by construction, so selecting nothing at the other zooms of a
range is what a one-cell smoke test looks like.

`RASTER_BBOX` does **not** shrink the global mercator mosaic. That is built
whole whenever `RASTER_MINZOOM <= RASTER_GLOBAL_MAXZOOM`. A regional run above
the global zoom skips the mosaic entirely, which is the case that matters.

### Scope, resume, and not destroying the last run

Both passes **resume**: a chunk whose output archive already exists is skipped,
and a super-cell drops a `.done` marker. Killing and restarting is safe. A pass
that lost chunks reports how many and exits non-zero rather than joining a
partial archive and exiting 0.

A `.done` marker names a super-cell and a zoom (`sc_z11_000320_000640`) and
**nothing else**, so resume is only sound between runs that would produce the
same tiles for that cell. The raster pass therefore keys its intermediates —
`raster-acc-<tag>/`, `raster-stage-<tag>/`, `raster-all-<tag>.mbtiles` — on a
**scope**: encoding, tile format, `RASTER_GLOBAL_MAXZOOM`, `SUPERCELL`,
`RASTER_BBOX` and `ONLY_SUPERCELL`. Runs differing in any of those cannot see
each other's markers.

The zoom range is deliberately **not** in that scope. `raster-acc/z{z}.mbtiles`
and the marker names already carry the zoom, so a run that stops at z11 and a
run that continues from z11 share intermediates on purpose — that sharing is the
resume feature, and `config::tests::the_intermediate_scope_ignores_the_zoom_range`
pins it.

The archive filename is keyed on the **encoding alone**, because
`squallar-terrain-hillshade.pmtiles` is the published object name and is pinned
by `squallar-egui/src/tiles.rs`, `squallar-web/sw.js` and two test files in the
app workspace. Two differently scoped runs therefore aim at one path, so a
scope stamp is written beside the archive (`<archive>.pmtiles.scope`) and a run whose
scope does not match it **refuses to start** rather than deleting hours of work
in `remove_file`. Point `OUT` at a separate directory per scope:

```sh
WORK=/mnt/terrain-work OUT=/mnt/terrain-work/out-global \
RASTER_ENCODING=terrain-rgb RASTER_MINZOOM=0 RASTER_MAXZOOM=11 \
  squallar-terrain build raster
WORK=/mnt/terrain-work OUT=/mnt/terrain-work/out-conus \
RASTER_ENCODING=terrain-rgb RASTER_MINZOOM=11 RASTER_MAXZOOM=12 \
RASTER_BBOX=-125,24,-66,50 \
  squallar-terrain build raster
```

The grid arithmetic is inspectable without running a build:

```sh
squallar-terrain extent 12 -106 39 -105 40      # EPSG:3857 metres + pixels
squallar-terrain bbox 12 848 1584 911 1647      # whole-degree box + centre lat
squallar-terrain chunks 5 < tileList.txt
squallar-terrain supercells 12 64 < tileList.txt
```

### Tests

```sh
cargo test                 # arithmetic, parsing, packing, the pins
cargo test -- --ignored    # the above plus GDAL end-to-end on a real tile
```

The GDAL suite is `#[ignore]`d rather than skipped-when-absent: a test that
quietly passes without its prerequisites is a gate that cannot fail. It needs
`gdalwarp`, `gdal_translate`, `gdal_contour`, `tippecanoe`, `sqlite3` and a
Copernicus tile at `TERRAIN_PROBE`.

### Exact commands, if you want them without the wrapper

These are what the binary spawns; running them by hand is how each was
verified.

```sh
# contours, one degree tile, one band -- no GeoJSON on disk
gdal_contour -q -a elev -i 100 -f GeoJSONSeq in.tif /vsistdout/ \
  | tippecanoe -q --force -Z13 -z14 -l contour -y elev \
      --no-feature-limit --no-tile-size-limit -o band.pmtiles
tile-join -q --force -o contours.pmtiles band_z10.pmtiles band_z11_12.pmtiles band_z13_14.pmtiles

# raster, one super-cell at one zoom
gdalwarp -t_srs EPSG:3857 -te $XMIN $YMIN $XMAX $YMAX -ts $NX $NY \
  -r average -ot Float32 -dstnodata 0 src.vrt elev.tif
gdaldem hillshade -alg Horn -z 1 -s $(cos lat) -az 315 -alt 45 -compute_edges elev.tif hs.tif
gdal_translate -of MBTILES -co TILE_FORMAT=PNG hs.tif cell.mbtiles
pmtiles convert all.mbtiles terrain.pmtiles

# terrain-RGB: the elevation comes out raw, the packer puts it back as a VRT
gdalwarp -of ENVI -t_srs EPSG:3857 -te $XMIN $YMIN $XMAX $YMAX -ts $NX $NY \
  -r average -ot Float32 -dstnodata 0 src.vrt elev.img
gdal_translate -of MBTILES -co TILE_FORMAT=PNG elev.rgb.vrt cell.mbtiles
```
