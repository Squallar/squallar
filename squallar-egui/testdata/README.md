# `squallar-egui` test data

## `terrain-hillshade-z10-224-395.webp`

One real tile out of the published terrain hillshade archive
(`tiles.squallar.app/terrain/4ca64469750e-20260829/`), fetched 2026-08-29:
z10 224/395, flat Kansas farmland with a river valley through it — flat
ground, shadow and lit slope in 666 bytes. `terrain::tests` pins the remap
against it pixel by pixel.

It is **lossy** VP8 WebP, and that is a property the tests depend on rather
than an accident: the encoder's RGB→YUV→RGB round trip cannot represent the
gdaldem flat level 181 (this tile decodes with zero pixels at 181; the
histogram jumps 180 → 182), which is what `terrain::HILLSHADE_FLAT_TOLERANCE`
exists to absorb. Do not re-encode or "optimise" it.

## `terrain-hillshade-mini.pmtiles`

The same WebP tile wrapped in a minimal, hand-built PMTiles v3 **raster**
archive: 803 bytes, `tile_type = 4` (WebP), `tile_compression = 1` (none),
`internal_compression = 1` (none), one root-directory entry, no leaves,
zooms 10..=10. Built by a one-shot script from the tile above (the varint
directory and 127-byte header are spelled per the PMTiles v3 spec).

It exists so `tile_source::tests::archive_decode` can prove the decode seam
takes its routing from a real header the reader parsed — `tile_type = 4`
routes bodies to the hillshade decoder, against `monaco.pmtiles` whose
`tile_type = 1` routes to `Tile::from_mvt` — rather than from sniffing the
bytes.

## `monaco.pmtiles`

A real PMTiles v3 vector-tile archive covering Monaco, committed so that
`basemap_archive`'s reader tests run everywhere instead of only on a workstation
that happens to hold a regional build.

### How it was built

```
java -Xmx4g -jar planetiler.jar --area=monaco --download \
  --output=monaco.pmtiles --force
```

planetiler v0.10.2, the stock OpenMapTiles profile, all defaults, 2026-08-27.

**It does not need regenerating.** PMTiles v3 is a frozen format, and the tests
pin properties of *this* file rather than of the current state of OpenStreetMap.
Rebuilding it from a newer OSM extract would move every counter below for no
gain. If it is ever replaced, the constants in
`src/basemap_archive/tests.rs` have to move with it.

### What it is

| property | value |
| --- | --- |
| size | 419,355 bytes |
| spec version | PMTiles v3 |
| tile type | MVT |
| tile compression | gzip |
| zoom range | 0..=14 |
| clustered | yes |
| root directory | 511 bytes, 157 entries |
| **leaf directories** | **0 bytes** |
| `n_addressed_tiles` | 246 |
| `n_tile_entries` | 157 |
| `n_tile_contents` | 108 |
| declared centre | lon 7.502127, lat 43.6183735 |
| bounding box | lon 7.408583..7.595671, lat 43.483817..43.752930 |
| vector layers | 15 |

The three dedup counters are three *different* numbers, and that is the reason
this archive is worth committing rather than a synthetic one. PMTiles collapses
tiles twice — `run_length` merges consecutive tile ids, content hashing merges
identical bodies that are not adjacent — and an archive where all three counters
agreed would exercise neither mechanism while looking exactly as authoritative.

The 15 vector layers are the OpenMapTiles set minus `aerodrome_label`: Monaco
has no aerodrome. `mountain_peak` *is* present.

### The zero leaf directories, and what they cost

All 246 tiles are addressed from the 511-byte root directory, so the archive has
no leaf directories at all. That is a consequence of it being small, and it is
the one property that makes it an unsuitable input for one test.

`directory_cache_is_load_bearing` counts range requests to prove the directory
cache saves a read: the first fetch of a tile pays for its leaf directory plus
the tile body, and a neighbour in the same leaf then costs one request rather
than two. On this archive there is no leaf to fetch and no second read to save,
so there is nothing for the assertion to measure.

That test therefore **detects the zero and skips loudly**, with a banner naming
this specific cause so it is not mistaken for a missing fixture. It was not
weakened to pass here — a test that goes green against an input which cannot
exercise it is worse than one that admits it did not run. To run it for real,
point `SQUALLAR_PMTILES_ARCHIVE` at a build large enough to have leaves (a US
state extract is ample):

```
SQUALLAR_PMTILES_ARCHIVE=/path/to/oklahoma.pmtiles \
  cargo test -p squallar-egui
```

Under that override `the_header_matches_the_built_archive` skips instead, since
every number in the table above describes this file specifically. The other
archive tests read their seed coordinate out of whichever archive they were
handed, so they follow the override rather than breaking under it.
