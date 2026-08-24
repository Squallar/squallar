# Squallar Feature Matrix

Feature comparison across weather platforms.

| Mark    | Meaning                                                                      |
| ------- | ---------------------------------------------------------------------------- |
| ✅       | Implemented and reachable by a user                                          |
| 🚧       | Partly built — code exists, no way for a user to reach it yet                |
| Partial | Implemented, but narrower than the column it sits beside                     |
| ❌       | Not implemented                                                              |
| ❓       | Not verified — the competitor's own documentation was ambiguous or unchecked |

Squallar's column is checked against this repository. Competitor columns are not,
and the paid tiers move: RadarScope's entries below reflect its **Pro** tiers
(Tier One adds real-time lightning and extended loops; Tier Two adds the
archive, MRMS, satellite and soundings), not the free app.

**On ❓.** It means the claim is not checked, and it is the honest mark for most
competitor cells — vendor feature pages are marketing, paid tiers sit behind
logins, and two of these products are Windows desktop applications. One
exception was checked on 2026-08-21 and is noted where it applies: the College
of DuPage satellite column. Everything in Squallar's column, and every source
fact quoted in the notes, was verified against the tree, the origin, or the file
itself — see `data.md`, which records the measurements and the probe recipe.

---

## 📡 Radar

| Feature                                       | GR2A    | RadarScope | Squallar |
| --------------------------------------------- | ------- | ---------- | ------- |
| NEXRAD Level 2 (super-res)                    | ✅       | ✅          | ✅       |
| Level 2 real-time chunks (sub-volume latency) | ✅       | ❓          | ✅       |
| NEXRAD Level 3 products                       | ✅       | ✅          | ✅       |
| TDWR terminal radars                          | ❓       | ✅          | ❌       |
| Dual-pol moments (CC, ZDR, PHI, KDP)          | ✅       | ✅          | ✅       |
| Hydrometeor classification (HHC)              | ✅       | ✅          | ✅       |
| Echo tops (EET, and tilt-interpolated)        | ✅       | ✅          | ✅       |
| VIL and VIL density (VILD)                    | ✅       | ❓          | ✅       |
| Hail products (POSH, MEHS)                    | ✅       | ❓          | ✅       |
| Precipitation rate (DPR)                      | ✅       | ✅          | ✅       |
| Normalized rotation (NROT)                    | ✅       | ❌          | ✅       |
| Storm-relative velocity                       | ✅       | ✅          | ✅       |
| 3D volumetric rendering                       | ✅       | ❌          | Partial |
| Vertical cross-sections                       | ✅       | ❌          | ✅       |
| MRMS national mosaic (CREF, precip rate)      | ✅       | ✅          | ❌       |
| Global satellite mosaic (GMGSI, 4 channels)   | ✅       | ❌          | ❌       |
| Satellite QPE (precip beyond radar range)     | ❌       | ❓          | ❌       |
| Third-party tile ingest (Rain Viewer et al.)  | ❌       | ❓          | ❌       |
| VAD wind profiles                             | ✅       | ❓          | ❌       |
| Radar loop animation                          | ✅       | ✅          | ✅       |
| Multi-radar compositing                       | Partial | ❌          | ❌       |
| Archive radar playback                        | ✅       | ✅          | ✅       |
| Custom color tables                           | ✅       | ✅          | ❌       |

Notes on the Squallar column:

- **Level 2 real-time chunks** — `squallar-radar/src/chunks.rs` reads the
  `unidata-nexrad-level2-chunks` bucket, so the 0.5° tilt arrives seconds after
  the radar collects it rather than at end-of-volume. A WebSocket push feed
  (`squallar-radar/src/chunk_notify.rs`) drives the fetch.
- **HHC, POSH, MEHS, NROT** are derived locally from the Level 2 volume;
  **KDP, EET, VIL, VILD and DPR** come from the Level III bucket
  (`RadarProduct::is_level3`).
- **TDWR terminal radars** — the table carries all 45 of them, the picker offers
  them, and the Level 2 archive holds their volumes under the ordinary site-day
  prefix (`_V08` keys beside a WSR-88D's `_V06`). What comes back does not draw
  as a sweep: `nexrad-decode` frames each message from wherever the previous
  one's parse stopped, and a TDWR pads every Message 31 body to an 8-byte
  boundary, so a record loses framing after its first radial and the renderer
  fans the survivors into wedges. The Level 3 bucket does serve TDWR sites, but
  only the legacy single-pol codes (`TZL`, `TZ0`-`TZ2`, `TV0`-`TV2`, `NCR`,
  `NHI`, `NMD`, …); none of `EET`, `DVL`, `DPR` or `N0K` exists for one, checked
  2026-08-11 against `PIT`, `OKC`, `MIA` and `DCA`.
- **3D volumetric rendering** — a pane can be switched to a 3D view
  (View → 3D volume view), which resamples the volume onto a Cartesian voxel
  grid (`squallar-radar/src/voxel.rs`) and raymarches it
  (`squallar-volumetric/src/volume.wgsl`, `volume_raymarch.rs`, wired by
  `volume_bridge.rs`). Drag to orbit, scroll or pinch to zoom. **Reflectivity
  only**: the other five samplable moments have colour tables that are opaque at
  the bottom of their scale, and a volume drawn through one of those saturates
  into a solid block rather than a picture — the pane says so rather than
  drawing it. Two limits behind that: the box is a fixed 160 × 160 × 18 km
  around the site with no zoom or pan of its own, and the resample is
  150–200 ms per volume. That resample is **not** on the frame thread: it is a
  job kind of its own (`offload::JobRequest::Voxels`), dispatched through the
  same funnel every raster goes through, so it runs on a render thread natively
  and in the rasterization Web Worker in a browser.
- **Vertical cross-sections** — arm "Draw cross-section" from the toolbar or
  the View menu and drag a line across a map pane; the line and its endpoint
  grab zones stay drawn on the map and the cut appears in a section pane
  (`squallar-egui/src/ui_section_pane.rs`). The volume sampler
  (`squallar-radar/src/sampler.rs`) and the section rasterizer
  (`squallar-radar/src/xsect.rs`) are behind it, and the cut is a job kind
  (`offload::JobRequest::Section`) so it is rasterized off the frame thread on
  both targets. Loops cut a section per frame.
- **VAD wind profiles** — a VAD fit *is* computed
  (`nrot::WindProfileBuilder`), but only as an input to storm-relative velocity
  and NROT. It is never displayed as a wind profile, so this is ❌ as a feature.
- **Radar loop animation** — frame ceiling is per device class: 60 desktop, 20
  mobile, 12 web (`squallar-device-profile/src/constants.rs`).
- **Archive radar playback** — `squallar-radar/src/archive.rs`, over the full
  public NEXRAD Level 2 archive rather than a rolling window.
- **MRMS national mosaic** ships: the `Mrms` layer draws
  `MergedReflectivityQCComposite_00.50` and `PrecipRate_00.00` from
  `noaa-mrms-pds`, polling on the product's own ~2-minute cadence. It cost no
  new dependency and no new feature flag, exactly as the 2026-08-21 bucket
  inspection predicted: gzipped GRIB2 on grid template 3.0 (plain lat/lon,
  7000 × 3500 at 0.01°) with data representation template **5.41 — PNG**, which
  `flate2` and the already-enabled `png-unpack-with-png-crate` decode. The
  layer's own colour bars are in `squallar-overlays/src/mrms/fields.rs` rather
  than borrowed from the radar palette — that edge is cut and enforced.
  **CONUS only**; the bucket also carries Alaska, Hawaii, Guam and the
  Caribbean, each of which is a variant and a longitude envelope away.
- **Global satellite imagery** ships: the `Gmgsi` layer draws NOAA's Global
  Mosaic of Geostationary Satellite Imagery — longwave IR, shortwave IR,
  visible and water vapour — from `noaa-gmgsi-pds`, hourly, on a 3000 x 5000
  global grid blended from MSG, GOES and Himawari. It cost no new dependency:
  the granules are NetCDF4, so they go through the same `hdf5-pure` reader and
  the same CF-convention unpacking the GLM lightning layer already used. The
  one new piece of geometry is `GridCoords::Separable` — one axis per
  dimension, because the grid is uniform in Mercator y rather than in latitude
  and its declared resolution attribute is wrong by 9.7 degrees mid-grid.
  **The values are 0-255 counts, not Kelvin**, despite what `units` says; the
  colour bars in `squallar-overlays/src/gmgsi/fields.rs` are stated in counts,
  and a test pins that a Kelvin-scaled ramp would paint nothing at all.
- **Multi-radar compositing**, **satellite QPE** and **third-party tile ingest**
  remain three ❌ rows and three different projects, not one feature spelled
  three ways. Everything else drawn today is *per site*: one volume, one raster,
  stacked by the client. Compositing our own site renders is the expensive path:
  it means reproducing overlap resolution, beam-height weighting, terrain
  blockage and edge feathering. Satellite QPE (NOAA Enterprise Rain Rate) is the
  *coverage* answer where no ground radar reaches — verified 18000 × 6501 at
  0.02°, exactly 70°N to 60°S, in mm/h — and it is a rain rate rather than
  reflectivity, a different quantity with its own colour table, not a drop-in.
  It is NetCDF4/HDF5, so it too needs no new decoder. See `data.md` § Radar Data.
- **Custom color tables** is ❌ and is worth separating from the rows above: the
  palettes are compiled in (`squallar-radar/src/palette.rs`) and there is no
  import path for a user-supplied table.

## 🌀 Model Data

| Feature                            | Pivotal | WeatherBell | Windy | Squallar |
| ---------------------------------- | ------- | ----------- | ----- | ------- |
| HRRR                               | ✅       | ✅           | ✅     | Partial |
| NAM / NAM Nest                     | ✅       | ✅           | ❌     | ❌       |
| GFS                                | ✅       | ✅           | ✅     | ❌       |
| GEFS (ensemble)                    | ✅       | ✅           | ❌     | ❌       |
| ECMWF (open data)                  | ❌       | ✅           | ✅     | ❌       |
| RAP                                | ✅       | ✅           | ❌     | ❌       |
| HREF / SREF                        | ✅       | ✅           | ❌     | ❌       |
| NBM                                | ✅       | ✅           | ❌     | ❌       |
| Interactive (not just images)      | ❌       | ❌           | ✅     | ✅       |
| Ensemble spread / spaghetti        | ✅       | ✅           | ❌     | ❌       |
| Model comparison (side by side)    | Partial | ❌           | ✅     | Partial |
| Model soundings (virtual profiles) | ✅       | ❌           | ❌     | ❌       |
| Custom fields / derived params     | Partial | ❌           | ❌     | ❌       |
| Animated model loops               | ✅       | ❌           | ✅     | ❌       |
| Wind vectors (barbs / streamlines) | ❓       | ❓           | ❓     | ❌       |
| Coverage beyond CONUS              | ✅       | ✅           | ✅     | ❌       |

Squallar's HRRR support is **Partial**: sixteen fields
(`squallar-overlays/src/hrrr/mod.rs`, `ModelParameter::all()`) at the analysis
hour only — f00, or f01 for the two windowed updraft-helicity maxima. There is
no forecast-hour selector and so no model loop. The fields are SBCAPE, MLCAPE,
MUCAPE, SBCIN, MLCIN, lifted index, 0–1 km and 0–3 km SRH, 0–2 km and 2–5 km
max UH, 0–6 km bulk shear, surface gust, PWAT, 2 m temperature, 2 m dewpoint and
visibility. "Interactive" and "side by side" are ✅/Partial because the grids are
decoded and rasterized locally into panes rather than fetched as images, and
panes can show different parameters at once.

Three things stand between that column and the rest of the table:

- **HRRR is CONUS-only**, so every ✅ in Squallar's model coverage stops at the
  US border. Every other model row is either wider coverage (GFS, GEFS, ECMWF
  IFS are global) or more members. Nothing implements a fallback chain —
  regional model where one exists, global model elsewhere.
- **Every one of the eight ❌ model rows is CORS-blocked**, probed 2026-08-21.
  Seven are NOMADS, which answers no `Access-Control-Allow-Origin`; ECMWF open
  data is the eighth (its `GET` carries no `ACAO`, though its `OPTIONS` does,
  which is not enough). HRRR works in the browser only because it is read from
  the AWS Open Data mirror instead. Any model added through NOMADS is
  native-only until a mirror is found or a server fronts it — look for the
  bucket mirror first, since that is how HRRR was solved.
- **JPEG 2000 GRIB is not the blocker it looks like.** The `grib` pin is
  `default-features = false` with the C backends off, but 0.17.1 also defines
  `jpeg2000-unpack-with-hayro` and `ccsds-unpack-with-rust-aec` — both pure
  Rust, both non-default, and the pure-Rust backend takes priority when
  present. DRT 5.40/5.42 is a feature flag away, with no `openjpeg-sys` and no
  wasm/iOS cross-compile problem.

**Wind vectors** is ❌ rather than Partial on purpose: the surface wind field
Squallar decodes is HRRR *gust magnitude*, a scalar. Barbs, streamlines and
particle animation all need the `UGRD`/`VGRD` component pair, which nothing
fetches.

## ⚠️ SPC & Severe Weather

| Feature                           | GR2A    | RadarScope | Pivotal | Squallar |
| --------------------------------- | ------- | ---------- | ------- | ------- |
| Convective outlooks (Day 1–8)     | Overlay | ❓          | ✅       | ✅       |
| Mesoscale discussions             | Overlay | ❓          | ❌       | ✅       |
| Watch/Warning polygons            | ✅       | ✅          | ❌       | ✅       |
| Tornado/Severe/FFW polygons       | ✅       | ✅          | ❌       | ✅       |
| Storm reports (prelim & filtered) | Overlay | ✅          | ✅       | ✅       |
| SPC mesoanalysis parameters       | ❌       | ❌          | ✅       | ❌       |
| Significant hail/tornado probs    | ❌       | ❌          | ✅       | ✅       |
| Fire weather outlooks (Day 1–8)   | ❌       | ❌          | ❌       | ✅       |

Squallar's watch and warning geometry comes from the NWS Alerts API, which is
county/zone-shaped; the SPC watch parallelograms themselves are not fetched.

## 🌀 Tropical & Surface Analysis

Neither category exists in the tree. They are listed as one section because they
share a shape: both are **analysed products** — a human or a model has already
turned observations into geometry — rather than fields we rasterize ourselves.

| Feature                           | Windy | RadarScope | Squallar |
| --------------------------------- | ----- | ---------- | ------- |
| Active tropical cyclone positions | ✅     | ❓          | ❌       |
| Forecast cone / track             | ✅     | ❓          | ❌       |
| Wind-speed probabilities          | ❓     | ❓          | ❌       |
| Historical / past track           | ❓     | ❓          | ❌       |
| Analysed surface fronts           | ✅     | ❌          | ❌       |
| Pressure centres (H / L)          | ✅     | ❌          | ❌       |

Competitor columns in this section are unchecked, per the note at the top of the
file. What the Squallar column costs:

- **Tropical** is `nhc.noaa.gov/CurrentStorms.json` for the index, then a KMZ
  per storm for the cone and track. The JSON is trivial; **KMZ is zip + KML and
  the tree reads neither**, so the decoder is the work.
- **Surface fronts** come from the WPC coded surface analysis, a bespoke coded
  *text* format rather than GeoJSON — it needs its own parser, and there is no
  existing one to lean on.
- **Both origins are verified browser-blocked**, probed 2026-08-21:
  `tgftp.nws.noaa.gov` and `www.nhc.noaa.gov` each answer `200` with no
  `Access-Control-Allow-Origin`. Neither category can ship to the web build
  without something fronting those hosts, which makes this the clearest case in
  either document for the planned server crate. (`origins.rs` records `tgftp`
  answering `403` to an `Origin:` header; that no longer reproduces, though the
  verdict is unchanged.) Both endpoints are live — `CurrentStorms.json` returned
  an active storm, and the WPC file returned `CODED SURFACE FRONTAL POSITIONS`.
  See `data.md` § Surface Analysis & Tropical.

## 📊 Analysis Tools

| Feature                       | GR2A | RadarScope | Pivotal | SHARPpy | Squallar |
| ----------------------------- | ---- | ---------- | ------- | ------- | ------- |
| Skew-T / Log-P diagrams       | ❌    | ✅          | ✅       | ✅       | ❌       |
| Hodographs                    | ❌    | ❓          | Partial | ✅       | ❌       |
| Supercell composite / STP     | ❌    | ❌          | ✅       | ✅       | ❌       |
| CAPE/CIN/Shear calculators    | ❌    | ❌          | ✅       | ✅       | ❌       |
| Cross-section (model)         | ❌    | ❌          | ✅       | ❌       | ❌       |
| Time-height cross-sections    | ❌    | ❌          | ❌       | ❌       | ❌       |
| Point forecast meteograms     | ❌    | ❌          | ❌       | ❌       | ❌       |
| Multi-model comparison panels | ❌    | ❌          | Partial | ❌       | ❌       |

RadarScope's Pro Tier Two forecast soundings are what puts a ✅ on the Skew-T
row. Squallar reads environmental heights from Open-Meteo
(`squallar-radar/src/sounding.rs`) but only the 0 °C and −20 °C levels, only to
scale the hail products, and never draws them — that is not a sounding tool.

## 🛰️ Satellite

| Feature                   | Windy | COD | RadarScope | Squallar |
| ------------------------- | ----- | --- | ---------- | ------- |
| GOES-19/18 visible        | ✅     | ✅   | ✅          | ❌       |
| GOES IR / Water vapor     | ✅     | ✅   | ✅          | ❌       |
| GOES GLM (lightning)      | ❌     | ❌   | ❌          | ✅       |
| Mesoscale sectors         | ❌     | ✅   | ❓          | ❌       |
| Sandwich product (Vis+IR) | ❌     | ✅   | ❌          | ❌       |
| Day/Night band            | ❌     | ❌   | ❌          | ❌       |
| Animated loops            | ✅     | ✅   | ✅          | ❌       |
| Multi-satellite (global)  | ✅     | ❌   | ❌          | ❌       |
| CIRA GeoColor (day/night) | ❓     | ❌   | ❓          | ❌       |
| Global IR mosaic (GMGSI)  | ❓     | ❌   | ❓          | ❌       |
| Meteosat MTG / MSG-IODC   | ❓     | ❌   | ❓          | ❌       |
| Himawari (W Pacific)      | ❓     | ❌   | ❓          | ❌       |

Squallar reads the `noaa-goes19` and `noaa-goes18` buckets, but only for GLM
lightning — no imagery product is decoded. GOES-19 replaced GOES-16 in the
GOES-East slot in April 2025, which is why the row is no longer "GOES-16/18".

Every ❌ above is reachable two different ways, and the choice is the whole
design decision:

- **Decode the raw product.** The container is already solved — GLM L2 LCFA is
  NetCDF4, which is plain HDF5, and `glm::h5` reads it with `hdf5-pure`. ABI L2
  CMI is NetCDF4 too. What is missing is not a reader but the GOES ABI
  fixed-grid (geostationary perspective) → Mercator reprojection.
- **Fetch someone else's composite.** GeoColor is a *recipe*, not a file: no
  object in the GOES buckets is "GeoColor". CIRA renders it, and NASA GIBS and
  EUMETView serve it made — both verified `open` to a browser, and both
  confirmed to carry it by layer name (`GOES-East_ABI_GeoColor`,
  `GOES-West_ABI_GeoColor`, `mtg_fd:rgb_geocolour`). **The cost is a WMS
  client**, which the tree does not have. CIRA SLIDER is the third route and is
  CORS-blocked, so the `noaa-himawari9` bucket beats it for Himawari.

GMGSI is the cheapest global layer of the lot: already merged across
GOES-East/West, Meteosat-9/10 and Himawari-9, already on a plain lat/lon grid,
and in NetCDF4 — which is HDF5, the container `glm::h5` already reads with
`hdf5-pure`. **It needs no new decoder and no reprojection.** Measured from a
granule rather than quoted: 3000 × 4999, +72.71541° to −72.73677°, ~8 km at the
equator, published hourly as a 10-minute snapshot. Three NOAA pages give three
different extents for this product and none matches the file — the file declares
its own (`geospatial_lat_max = 72.7154f`). **Four live channels** (`GMGSI_LW`
longwave IR, `GMGSI_SW` shortwave IR, `GMGSI_VIS` visible, `GMGSI_WV` water
vapour), plus a fifth prefix `GMGSI_SSR` that is **discontinued** — its last
granule is dated 2025-06-03 while the others run current. See `data.md`
§ Satellite Imagery for the measurements, the licence obligations and the CORS
status of every origin.

The College of DuPage column was checked against its NEXLAB satellite page on
2026-08-21: it enumerates its RGB products (True-Color, Airmass, Natural Color,
NT Microphysics, Day Cloud Phase, Simple WV, Sandwich) and GeoColor is not among
them, and it serves GOES-East/West only. That fixes two cells — Sandwich was
recorded ❌ for COD and is ✅ — and sets the four ❌s in its column. The Windy
column could not be checked: the site is a JavaScript application that serves no
feature text to a fetch, so those cells stay ❓.

One licence note belongs here rather than in `data.md` alone, because it shapes
what a satellite or radar layer may look like in the product: **Rain Viewer's
free API requires a visible "Weather data by RainViewer" credit linking back to
their site**, and scopes free use to personal, educational and small-scale
community use. EUMETSAT imagery is free of charge at ≥1 hour latency for any
use. CIRA SLIDER publishes no licence at all and is CORS-blocked, so the
`noaa-himawari9` bucket is the better route to the same imagery.

## ⚡ Lightning

| Feature                            | RadarScope | Baron | Squallar |
| ---------------------------------- | ---------- | ----- | ------- |
| Real-time strikes (ground network) | ✅ (Pro)    | ✅     | ❌       |
| GLM (satellite-based)              | ❌          | ✅     | ✅       |
| Blitzortung (community)            | ❌          | ❌     | ❌       |
| Lightning density / history        | ❌          | ✅     | ❌       |

GLM is satellite-based *total* lightning — in-cloud plus cloud-to-ground, over
the GOES East and West disks. It neither replaces nor is replaced by a ground
network: the ✅ and the ❌ on the first two rows measure different things.

## 📍 Surface Observations

| Feature                        | GR2A    | RadarScope | Squallar |
| ------------------------------ | ------- | ---------- | ------- |
| METAR/ASOS station plots       | Overlay | ❌          | ✅       |
| Personal weather stations      | ❌       | ❌          | ❌       |
| Mesonet data (OK, etc.)        | Overlay | ❌          | ❌       |
| Buoy / marine obs              | ❌       | ❌          | ❌       |
| mPING spotter reports          | ❌       | ❌          | ❌       |
| Station model plots (standard) | ❌       | ❌          | Partial |

Squallar draws a station model (`squallar-overlays/src/render/station_model.rs`)
from the Iowa Environmental Mesonet's METAR feed. It is a reduced plot, not the
full WMO station model, hence Partial.

## 🔔 Alerting & Notifications

| Feature                             | Baron | DTN | RadarScope | Squallar |
| ----------------------------------- | ----- | --- | ---------- | ------- |
| Custom geo-fenced alerts            | ✅     | ✅   | Partial    | ❌       |
| Threshold-based (wind >60mph, etc.) | ✅     | ✅   | ❌          | ❌       |
| Push notifications                  | ✅     | ✅   | ✅          | ❌       |
| Multi-hazard dashboard              | ✅     | ✅   | ❌          | ❌       |
| Email/SMS/webhook alerts            | ✅     | ✅   | ❌          | ❌       |

Squallar has a push feed, but it notifies the *app* that a new Level 2 chunk
exists so the fetch can start immediately. Nothing notifies the user of weather.

## 💻 Platform & UX

| Feature                     | GR2A       | RadarScope  | Windy | Squallar           |
| --------------------------- | ---------- | ----------- | ----- | ----------------- |
| Desktop app                 | ✅ (Win)    | ✅ (Win/Mac) | ❌     | ✅ (Linux/Mac/Win) |
| Web app                     | ❌          | ❌           | ✅     | ✅                 |
| Mobile (iOS/Android)        | ❌          | ✅           | ✅     | Partial           |
| Multi-pane / dual-pane      | ✅          | ✅ (Pro)     | ❌     | ✅                 |
| Offline capability          | ✅          | ❌           | ❌     | ❌                 |
| Dark mode / themes          | ❌          | ❓           | ✅     | Partial           |
| Customizable layer stack    | ✅          | Partial     | ✅     | ✅                 |
| Plugin / extension system   | Placefiles | ❌           | ❌     | ❌                 |
| Open API for developers     | ❌          | ❌           | Paid  | ❌                 |
| GR2Analyst placefile compat | ✅          | ❌           | ❌     | ❌                 |

- **Mobile** is Partial: Android ships (the `squallar` crate's android arm), and iOS builds an
  `.ipa` in CI (`packaging/ios/Makefile`, the `ios-aarch64` row of `build.yaml`), but it
  is unsigned and undistributed.
- **Multi-pane** is up to six panes on desktop and four on mobile
  (`MAX_PANES_DESKTOP` / `MAX_PANES_MOBILE` in `squallar-egui/src/pane.rs`).
- **Dark mode** follows the OS and only the OS — `dark_light::detect` on
  desktop, a JNI read of `Configuration.uiMode` on Android — with light and dark
  basemap label tiles behind it. There is no in-app override and no theme
  beyond the two, hence Partial.
- **Offline capability** is ❌ despite the PWA service worker: `sw.js` caches the
  application shell and basemap tiles, so the app launches offline and shows a
  map, but it caches no weather data at all — by design, and enforced by a
  never-cache host list.
