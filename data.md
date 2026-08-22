# Data Sources

This document details which data sources are needed for this project. Not all are implemented yet.

## How to read this file

| Column            | Meaning                                                                                              |
| ----------------- | ---------------------------------------------------------------------------------------------------- |
| **Status**        | ✅ implemented · Partial implemented but narrower than the row · ❌ not implemented                    |
| **Domain**        | Where on Earth the source has data. A row usually exists because the row above it runs out of world. |
| **CORS**          | Whether the **web** build can reach the origin. One of `open`, `simple only`, `blocked`, `n/a`.      |
| **Public Access** | Cost, endpoint, and the format the bytes arrive in.                                                  |

### The CORS values

| Value         | Meaning                                                                                                                                                          |
| ------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `open`        | `Access-Control-Allow-Origin: *` on the `GET`, and preflight succeeds. Usable from a browser as an ordinary request.                                             |
| `simple only` | Plain `GET` carries `ACAO: *`, but `OPTIONS` is refused — so **any** custom header (a `User-Agent` included) makes the request preflighted and it never happens. |
| `blocked`     | No `ACAO` on the `GET`. The browser refuses the response regardless of status code. Native-only until something fronts it.                                       |
| `n/a`         | No network request: compiled in, or a source we cannot use at all.                                                                                               |

**Every CORS value in this file was probed on 2026-08-21** — `GET` and `OPTIONS`
preflight, with an `Origin:` header, against the exact endpoint named in the
row. Nothing here is inferred. `rustdar-source/src/origins.rs` remains the
authority for the origins the app actually uses; this file covers those plus
every candidate. To re-probe a row:

```sh
curl -sS -o /dev/null -D - -H 'Origin: https://rustdar.example' "$URL"
curl -sS -o /dev/null -D - -X OPTIONS \
     -H 'Origin: https://rustdar.example' \
     -H 'Access-Control-Request-Method: GET' "$URL"
```

**On ⚠️ in the Public Access column.** It marks a licence obligation that
survives the free price, and each one below was read on 2026-08-21:

- **Rain Viewer** — the public API is *"free to use and open to the public"*,
  no key on the free tier. **Attribution is mandatory**: a visible "Weather data
  by RainViewer" credit linking to rainviewer.com. Free use is scoped to
  personal, educational and small-scale community use with no SLA; high-volume
  or commercial integration is arranged case by case, and a keyed tier is rate
  limited (~1000 requests/day, `429` past it). They also disclaim availability
  outright, since upstream owners can pull data at any time.
- **EUMETSAT** — free of charge, not merely free to look at. Basic access
  licences are free; Meteosat data at ≥1 hour latency is available without
  charge *for any use*, and full-resolution 15-minute imagery is free three
  hours after sensing. Some product families (OSI SAF) are explicitly CC BY 4.0.
  Attribution is the practical obligation.
- **CIRA SLIDER** — **no licence or terms-of-use page exists.** `about.html`
  and `faq.html` are 404, and RAMMB publishes only a "Disclaimer" and an
  "Experimental Products Disclaimer". Combined with the origin being CORS
  `blocked`, there is no reason to prefer it: the `noaa-himawari9` bucket is
  `open` and carries the same underlying imagery.

None of this is legal advice; it is what the publishers say, with the date read.

**One correction to `origins.rs`.** Its comment records `tgftp.nws.noaa.gov`
answering `403` when an `Origin:` header is present. On 2026-08-21 it answered
`200` with **no** `ACAO` header. The verdict is unchanged — `blocked` either
way — but the recorded evidence no longer reproduces.

**All ten AWS Open Data buckets probed came back `open`.** When a row is
`blocked`, look for a bucket mirror before accepting it.

---

## Radar Data

| Data Source                       | Status | Domain                          | CORS    | Public Access                                                                                                                                            |
| --------------------------------- | ------ | ------------------------------- | ------- | -------------------------------------------------------------------------------------------------------------------------------------------------------- |
| NEXRAD Level 2 (archive)          | ✅      | US + territories                | open    | ✅ Free — AWS Open Data `unidata-nexrad-level2`                                                                                                           |
| NEXRAD Level 2 real-time chunks   | ✅      | US + territories                | open    | ✅ Free — AWS Open Data `unidata-nexrad-level2-chunks`; ~55 pieces per volume, each landing seconds after collection                                      |
| NEXRAD Level 3                    | ✅      | US + territories                | open    | ✅ Free — AWS Open Data `unidata-nexrad-level3` (SRM tilts 1–3 discontinued upstream)                                                                     |
| MRMS (Multi-Radar/Multi-Sensor)   | ❌      | CONUS, AK, HI, Guam, Caribbean  | open    | ✅ Free — AWS Open Data `noaa-mrms-pds`, `us-east-1`. GRIB2, gzipped. **See the verified detail below.**                                                  |
| MRMS via NCEP                     | ❌      | same                            | blocked | ✅ Free — `mrms.ncep.noaa.gov/2D/`. No `ACAO`. The bucket above is the same data and is `open`; prefer it.                                                |
| NOAA Enterprise Rain Rate (RRQPE) | ❌      | **70°N – 60°S**, all longitudes | open    | ✅ Free — AWS Open Data `noaa-enterprise-rainrate-pds`. Satellite QPE; the precipitation answer outside ground-radar coverage                             |
| Rain Viewer v2 tiles              | ❌      | varies by provider              | open    | ⚠️ Attribution required — `api.rainviewer.com/public/weather-maps.json`. Free, no key; see the terms below. A self-hosted LibreWXR serves the same shape |

### MRMS, verified

Read off the bucket on 2026-08-21, not from documentation:

- **Top-level prefixes:** `CONUS/`, `CONUS_5KM/`, `ALASKA/`, `HAWAII/`,
  `GUAM/`, `CARIB/`, `ANC/`, plus `ProbSevere/`, `ConvectProb/`, `unsupported/`.
- **The two products worth having** exist exactly as
  `CONUS/MergedReflectivityQCComposite_00.50/` and `CONUS/PrecipRate_00.00/`.
- **Files are `.grib2.gz`** — gzip around GRIB2, e.g.
  `MRMS_PrecipRate_00.00_20260101-000000.grib2.gz`, ~660 KB.
- **Grid definition template 3.0** (plain lat/lon), 24,500,000 points —
  3500 × 7000 at 0.01°. Simpler than HRRR, which is template 3.30 (Lambert
  conformal) and needed `hrrr::lambert` written by hand.
- **Data representation template 5.41 — PNG.** Not 5.3, and not JPEG 2000.

**That last line means MRMS needs no new decoder feature at all.** The `grib`
pin already enables `png-unpack-with-png-crate`, and `flate2` already handles
the gzip. Of the three mosaic paths this is by far the cheapest.

### The three mosaic paths are three different projects

Everything the app draws today is *per site*: one volume, one raster, stacked on
the map by the client. A national mosaic is a different data model.

- **MRMS** is mosaicked upstream and, per the above, decodable today.
- **RRQPE** is the coverage fallback, and it is a *rate* rather than
  reflectivity — a different quantity with its own colour table, not a drop-in.
  Verified from a granule: 18000 × 6501 `short` values, `scale_factor` 0.1,
  units `mm/h`, on a plain 0.02° lat/lon grid running exactly 70°N to 60°S.
  Delivered as **NetCDF4, which is HDF5** — the same container `glm::h5`
  already reads with `hdf5-pure`. Sub-prefixes are `BLEND/` plus per-satellite
  `G16/`, `G18/`, `G19/`, `Himawari-9/`; `BLEND` is the global one. New
  granules every 10 minutes.
- **Compositing our own site renders** is the expensive one, and needs no
  source row because it needs no source: overlap resolution, beam-height
  weighting, terrain blockage and edge feathering are what MRMS exists to do.
  Prefer ingesting MRMS to reproducing it.

### Derived and Level 3 products

Products derived locally from the Level 2 volume — HHC, POSH, MEHS, NROT,
interpolated echo tops and storm-relative velocity — need no source of their
own. KDP, EET, VIL, VIL density and precipitation rate are fetched from the
Level 3 bucket (`RadarProduct::is_level3`).

Both radar networks share those buckets, which is not the same as sharing the
products. The Level 2 archive carries TDWR volumes under the same
`YYYY/MM/DD/SITE/` prefix as WSR-88D ones, keyed `_V08` rather than `_V06`. The
Level 3 bucket carries TDWR products too, but the legacy single-pol set (`TZL`,
`TZ0`-`TZ2`, `TV0`-`TV2`, `NCR`, `NHI`, `NMD`, …) — not one of the four codes
this app asks for, so the five Level 3 products above are a WSR-88D feature.

## Numerical Weather Prediction (NWP) Models

| Model                                  | Status | Domain         | CORS    | Public Access                                                                                                                                           |
| -------------------------------------- | ------ | -------------- | ------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- |
| HRRR (High-Resolution Rapid Refresh)   | ✅      | **CONUS only** | open    | ✅ Free — AWS Open Data `noaa-hrrr-bdp-pds`, `.idx` byte-ranged. Analysis hour only: f00, or f01 for the windowed updraft-helicity maxima                |
| RAP (Rapid Refresh)                    | ❌      | North America  | blocked | ✅ Free — NOAA NOMADS. No `ACAO`.                                                                                                                        |
| NAM (North American Mesoscale)         | ❌      | North America  | blocked | ✅ Free — NOAA NOMADS. No `ACAO`.                                                                                                                        |
| GFS (Global Forecast System)           | ❌      | **global**     | blocked | ✅ Free — NOAA NOMADS `filter_gfs_0p25.pl`. No `ACAO`. The global fallback under every CONUS-only row above                                              |
| GEFS (Global Ensemble Forecast System) | ❌      | global         | blocked | ✅ Free — NOAA NOMADS. No `ACAO`.                                                                                                                        |
| SREF (Short-Range Ensemble Forecast)   | ❌      | North America  | blocked | ✅ Free — NOAA NOMADS. No `ACAO`.                                                                                                                        |
| NBM (National Blend of Models)         | ❌      | North America  | blocked | ✅ Free — NOAA NOMADS. No `ACAO`.                                                                                                                        |
| HREF (High-Res Ensemble Forecast)      | ❌      | CONUS          | blocked | ✅ Free — NOAA NOMADS. No `ACAO`.                                                                                                                        |
| ECMWF IFS (open data)                  | ❌      | global         | blocked | ✅ Free — `data.ecmwf.int/forecasts/`, 0.25°. `GET` carries no `ACAO` (its `OPTIONS` does, which is not enough). High-res operational is ⚠️ paid-licence |

**HRRR is CONUS-only, and that is why every row under it exists.** Each one is
either wider coverage or more members. A specificity-first stack — regional
model where one exists, global model everywhere else — is where this table
points; nothing in the tree implements a fallback chain.

**Eight of the nine rows are `blocked`.** Seven are NOMADS, which answers no
`ACAO`; ECMWF open data likewise. HRRR works in the browser only because it is
read from an AWS Open Data mirror instead. Any model added through NOMADS is
native-only until a mirror is found or the server crate fronts it. **Look for
the bucket mirror first** — it is how HRRR was solved.

**JPEG 2000 GRIB is not a blocker.** `grib` 0.17.1 defines
`jpeg2000-unpack-with-hayro` (DRT 5.40) and `ccsds-unpack-with-rust-aec`
(DRT 5.42), both **pure Rust**, both non-default, and the crate documents that
the pure-Rust backend takes priority when more than one is available. The C
defaults (`jpeg2000-unpack-with-openjpeg` → `openjpeg-sys`,
`ccsds-unpack-with-libaec` → `libaec-sys`) stay off. Whatever packing a new
model uses, there is a pure-Rust path to it.

| NWP Parameter                                        | Status | Public Access                                                                            |
| ---------------------------------------------------- | ------ | ---------------------------------------------------------------------------------------- |
| Temperature, dewpoint, wind (surface)                | ✅      | ✅ HRRR: 2 m temperature, 2 m dewpoint, surface wind gust                                 |
| Wind components `UGRD`/`VGRD` (barbs, streamlines)   | ❌      | ✅ HRRR or GFS. **Gust magnitude is not a vector** — nothing today can draw a barb        |
| Temperature, dewpoint, wind (upper air)              | ❌      | ✅ Derived from public models above                                                       |
| CAPE, CIN, SRH (Storm Relative Helicity), bulk shear | ✅      | ✅ HRRR: SB/ML/MU CAPE, SB/ML CIN, lifted index, 0–1 km and 0–3 km SRH, 0–6 km bulk shear |
| Updraft helicity                                     | ✅      | ✅ HRRR: 0–2 km and 2–5 km maxima, from f01 (the f00 record is identically zero)          |
| Simulated reflectivity                               | ❌      | ✅ Derived from public models above                                                       |
| Precipitation (QPF), snow, ice                       | ❌      | ✅ Derived from public models above                                                       |
| 500 mb heights/vorticity, jet stream, thickness      | ❌      | ✅ Derived from public models above (250 mb `UGRD`/`VGRD` is the usual jet-stream pair)   |
| Precipitable water (PWAT)                            | ✅      | ✅ HRRR                                                                                   |
| Relative humidity (2 m)                              | ❌      | ✅ HRRR or GFS `RH`                                                                       |
| LCL, LFC, EL                                         | ❌      | ✅ Derived from public models above                                                       |
| Surface visibility                                   | ✅      | ✅ HRRR                                                                                   |

Every ✅ in this table is a CONUS value, because every one of them is HRRR.

## SPC (Storm Prediction Center) Data

| Data Source                            | Status  | Domain | CORS        | Public Access                                                                                                               |
| -------------------------------------- | ------- | ------ | ----------- | --------------------------------------------------------------------------------------------------------------------------- |
| Convective Outlooks (Day 1–8)          | ✅       | CONUS  | simple only | ✅ Free — SPC GeoJSON endpoints                                                                                              |
| Mesoscale Discussions (MDs)            | ✅       | CONUS  | simple only | ✅ Free — SPC RSS feed                                                                                                       |
| Watches (Tornado/Severe Tstorm)        | Partial | CONUS  | open        | ✅ Free — arrive through the NWS Alerts API as county/zone geometry; the SPC watch parallelograms themselves are not fetched |
| Storm Reports (preliminary & filtered) | ✅       | CONUS  | simple only | ✅ Free — SPC CSV files                                                                                                      |
| Fire Weather Outlooks                  | ❌       | CONUS  | simple only | ✅ Free — SPC GeoJSON endpoints                                                                                              |
| SPC Mesoanalysis graphics              | ❌       | CONUS  | simple only | ✅ Free — SPC website (raster images)                                                                                        |
| Precipitation Discussions              | ❌       | CONUS  | blocked     | ✅ Free — WPC website (`www.wpc.ncep.noaa.gov`, no `ACAO`)                                                                   |
| Sounding data / SPC skew-T parameters  | ❌       | CONUS  | blocked     | ✅ Free — University of Wyoming (`weather.uwyo.edu`, no `ACAO`)                                                              |

`simple only` is measured: `www.spc.noaa.gov` returns `200` with `ACAO: *` on a
plain `GET` and **`403` with no CORS headers on `OPTIONS`**. Any custom header
makes the request preflighted and it never happens.
`DataSources::spc_sends_user_agent` is `false` in production for this reason.

## Weather Alerts & Warnings

| Data Source                                           | Status | Domain | CORS | Public Access                    |
| ----------------------------------------------------- | ------ | ------ | ---- | -------------------------------- |
| NWS Alerts API                                        | ✅      | US     | open | ✅ Free — api.weather.gov         |
| Weather.gov API /alerts                               | ✅      | US     | open | ✅ Free — api.weather.gov         |
| Warning polygons (tornado, severe, flash flood, etc.) | ✅      | US     | open | ✅ Free — NWS API zone geometries |

## Surface Analysis & Tropical

Neither category exists in the tree. Both are **analysed products** — a human or
a model has already turned observations into geometry — rather than fields we
rasterize ourselves, which is why they sit together.

| Data Source                               | Status | Domain          | CORS    | Public Access                                                                                                   |
| ----------------------------------------- | ------ | --------------- | ------- | --------------------------------------------------------------------------------------------------------------- |
| WPC coded surface analysis (fronts)       | ❌      | North America   | blocked | ✅ Free — `tgftp.nws.noaa.gov/data/raw/as/asus02.kwbc.cod.sus.txt`. **Bespoke coded text**, needs its own parser |
| NHC active storms (index)                 | ❌      | Atlantic + EPac | blocked | ✅ Free — `www.nhc.noaa.gov/CurrentStorms.json`. No `ACAO`.                                                      |
| NHC forecast cones / tracks               | ❌      | Atlantic + EPac | blocked | ✅ Free — KMZ linked from the index. **KMZ is zip + KML**; the tree reads neither                                |
| NHC advisories / wind-speed probabilities | ❌      | Atlantic + EPac | blocked | ✅ Free — NHC product feeds, same host                                                                           |

Both endpoints were fetched on 2026-08-21 and both are live. The WPC file opens
`ASUS02 KWBC` / `CODSUS` / `CODED SURFACE FRONTAL POSITIONS` — plain coded text,
not GeoJSON. `CurrentStorms.json` returns an `activeStorms` array whose entries
carry `id`, `name`, `classification`, `intensity`, `pressure`,
`latitudeNumeric`/`longitudeNumeric`, `movementDir`/`movementSpeed` and
`lastUpdate`; the cone and track are KMZ links off that record.

**Both hosts are `blocked`, so this whole section is native-only** until
something fronts them. It is the clearest case in the file for the server crate.

## Observational / Surface Data

| Data Source                                | Status | Domain     | CORS        | Public Access                                                                                                              |
| ------------------------------------------ | ------ | ---------- | ----------- | -------------------------------------------------------------------------------------------------------------------------- |
| METAR/ASOS (surface obs)                   | ✅      | US         | simple only | ✅ Free — Iowa Environmental Mesonet `currents.json`, per state                                                             |
| Environmental 0 °C / −20 °C heights        | ✅      | global     | open        | ✅ Free — Open-Meteo `/v1/forecast`. Only these two levels, only to scale the hail products; not a sounding                 |
| Upper-air soundings (RAOB)                 | ❌      | global     | blocked     | ✅ Free — University of Wyoming (`weather.uwyo.edu`, no `ACAO`) / UCAR                                                      |
| Mesonets (state/regional surface networks) | ❌      | US, patchy | simple only | ✅ Free — Oklahoma Mesonet `current.csv.txt` probed: `ACAO: *` on `GET`, `403` on `OPTIONS`. Other states vary; probe each. |
| Buoy / Marine obs                          | ❌      | US coastal | blocked     | ✅ Free — NDBC (`www.ndbc.noaa.gov`, no `ACAO`)                                                                             |
| ASOS 1-min data                            | ❌      | US         | open        | ✅ Free — NCEI (`www.ncei.noaa.gov`, `ACAO: *`)                                                                             |
| Storm spotter reports (mPING)              | ❌      | US         | blocked     | ✅ Free — NSSL mPING (`mping.ou.edu`, no `ACAO`)                                                                            |
| State traffic/weather cameras              | ❌      | US, patchy | blocked     | ⚠️ Mixed — three probed (WA, IA, CO): none served a CORS-open public feed. No unified API; probe per state.                |

IEM is preflight-hostile in the same way SPC is, and measured the same way: `GET`
returns `200` with `ACAO: *`, `OPTIONS` returns `405` with no
`Access-Control-Allow-Methods`. `DataSources::metar_sends_user_agent` is `false`
in production.

The two `varies` rows are the only ones in this file without a single verdict,
because they are not a single origin. Probe the specific network before planning
a web feature on either.

## Satellite Imagery

Two strategies. Decoding the raw product gives full control and costs a
reprojection; fetching someone else's composite is nearly free but fixes the
colour recipe and the cadence.

**Raw products — we decode:**

| Data Source                    | Status | Domain              | CORS    | Public Access                                                                                                                                                |
| ------------------------------ | ------ | ------------------- | ------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| GOES-19/18 imagery (East/West) | ❌      | Americas disks      | open    | ✅ Free — `noaa-goes19` / `noaa-goes18`. `ABI-L2-CMIPC/F/M` (single band) and `ABI-L2-MCMIPC/F/M` (multiband) all present                                     |
| GOES-19/18 mesoscale sectors   | ❌      | roving, two per sat | open    | ✅ Free — same buckets, `ABI-L2-CMIPM` / `ABI-L2-MCMIPM`                                                                                                      |
| Himawari-9 (W Pacific)         | ❌      | W Pacific disk      | open    | ✅ Free — AWS Open Data `noaa-himawari9`: `AHI-L1b-FLDK/`, `AHI-L1b-Japan/`, `AHI-L1b-Target/`, plus L2 cloud/wind sets                                       |
| Meteosat MTG / MSG-IODC        | ❌      | Europe / Indian     | open    | ✅ Free of charge — EUMETSAT; the WMS row below is the practical route. No AWS Open Data mirror was found                                                     |
| Polar-orbiting (JPSS/VIIRS)    | ❌      | global, swaths      | blocked | ✅ Free — NOAA CLASS at **`www.class.noaa.gov`** (`200`, no `ACAO`). The bare `class.noaa.gov` resolves but never completes a connection; use the `www.` host |

**Pre-made composites — someone else renders:**

| Data Source                   | Status | Domain              | CORS    | Public Access                                                                                                         |
| ----------------------------- | ------ | ------------------- | ------- | --------------------------------------------------------------------------------------------------------------------- |
| NOAA GMGSI global mosaic      | ❌      | **72.7°N – 72.7°S** | open    | ✅ Free — AWS Open Data `noaa-gmgsi-pds`. **See the verified detail below.**                                           |
| CIRA GeoColor, GOES East/West | ❌      | Americas            | open    | ✅ Free — NASA GIBS **WMS**. Layers verified present: `GOES-East_ABI_GeoColor`, `GOES-West_ABI_GeoColor`               |
| MTG GeoColour + MSG / IODC    | ❌      | Europe / Indian     | open    | ✅ Free of charge — EUMETView **WMS**. Verified present: `mtg_fd:rgb_geocolour`, plus `msg_fes:*` and `msg_iodc:*`     |
| Himawari via CIRA SLIDER      | ❌      | W Pacific           | blocked | ⚠️ No published licence — slippy tiles at `slider.cira.colostate.edu`. No `ACAO` either; use `noaa-himawari9` instead |

### GMGSI, verified

The three NOAA pages describing GMGSI **disagree with each other**: the AWS
registry says global at ~8 km, NOAA VLab says 71°N–71°S at 8 km, and the OSPO
product page says 60°N–60°S at ~3 km. None of them matches what the bucket
contains. Measured from a 2026-08-01 granule with `ncdump`:

- Grid **3000 × 4999**, with explicit 2-D `lat`/`lon` arrays.
- Latitude runs **+72.71541° to −72.73677°**. Longitude covers the full ±180°.
- 360° / 4999 columns = 0.072°, i.e. **~8 km at the equator** — the registry's
  figure, not OSPO's.
- Values are `float`, `units = "K"`, `long_name = "0-255 Brightness Temperature"`,
  with a `dqf` quality byte alongside. Hourly.
- **NetCDF4, i.e. HDF5** — verified by magic bytes, `\x89HDF\r\n\x1a\n`,
  identical to a GLM L2 LCFA granule.
- The granule **declares its own extent**, corroborating the measurement:
  `geospatial_lat_max = 72.7154f`, `geospatial_lat_min = -72.7368f`,
  `geospatial_lat_resolution = geospatial_lon_resolution = 0.0722f`.
- `platform = "Meteosat10,G18,Meteosat9,H-9,G19"`,
  `instrument = "MSG-SEVIRI,GOES-ABI,Himawari-AHI"`, `processing_level = "Level 3"`.
  The `source` attribute lists the actual inputs, and **MSG-IODC is among them**.
- **Cadence is hourly, but each granule is a 10-minute window**:
  `time_coverage_start = "2026-08-01T00:00:00Z"`,
  `time_coverage_end = "2026-08-01T00:09:59Z"`. It is a snapshot published
  hourly, not an hourly composite.
- **Four live channels**, each naming itself in its `summary` attribute:
  `GMGSI_LW/` → `GLOBCOMPLIR`, "longwave infrared"; `GMGSI_SW/` → `GLOBCOMPSIR`,
  "shortwave infrared"; `GMGSI_VIS/` → "VISIBLE"; `GMGSI_WV/` → "mid-wave
  infrared (water vapor)". All four carry `units = "K"`.

### GMGSI_SSR is discontinued — do not plan on it

The fifth prefix, `GMGSI_SSR/`, is dead. Established on 2026-08-21:

- **The last granule is `GMGSI_SSR/2025/06/03/20/GLOBCOMPSSR_nc.2025060320`.**
  The other four channels run to 2026 and are current; SSR stops on 2025-06-03.
- It never migrated to the v3 metadata: it is still `Conventions = "CF-1.4"`,
  `Source = "McIDAS Area File"`, with no `summary`, no `platform`, no
  `geospatial_*` attributes and no `time_coverage_end`.
- Its `units` are `"none"` while the four live channels are `"K"`, and its
  `long_name` is a copy of theirs (`"0-255 Brightness Temperature"`) which the
  units contradict.
- **The acronym is expanded nowhere** — not in the granule, not on the AWS
  registry, not on OSPO, not on VLab. `Satellite Sensor = "DERIVED DATA"` is
  *not* the distinguishing mark: the live LW granule carries that same
  attribute.

Whatever `SSR` stood for, the operational answer is settled: it ended in June
2025 and nothing should be built on it.

**GMGSI is the cheapest global cloud layer available**: already merged, already
on a lat/lon grid, and in a container `hdf5-pure` already reads. No
per-satellite navigation, no disk reprojection, no new dependency.

### GeoColor is a recipe, not a file

Nothing in the AWS GOES buckets is "GeoColor" — it is CIRA's day/night RGB
blend. Either build it from ABI bands (`MCMIP` carries the multiband product
that makes this tractable) or fetch it rendered. Both WMS routes are `open`, and
both were confirmed to serve it by name.

**What the fetch route costs: a WMS client.** Nothing in the tree speaks WMS.
That is the single blocker on the two GeoColor rows, and it is shared between
them.

### The ABI decoder is closer than the ❌ suggests

GLM L2 LCFA is NetCDF4, which is HDF5, and `glm::h5` already reads it with
`hdf5-pure`. ABI L2 CMI is NetCDF4 too, and so — verified above — are GMGSI and
RRQPE. The container is solved for all four. What ABI alone still needs is the
GOES fixed-grid (geostationary perspective) → Mercator reprojection. GMGSI and
RRQPE need no reprojection at all: both are plain lat/lon grids.

## Lightning Data

| Data Source                          | Status | Domain         | CORS    | Public Access                                                                      |
| ------------------------------------ | ------ | -------------- | ------- | ---------------------------------------------------------------------------------- |
| GLM (Geostationary Lightning Mapper) | ✅      | GOES E+W disks | open    | ✅ Free — AWS Open Data `noaa-goes19` (East) / `noaa-goes18` (West), `GLM-L2-LCFA/` |
| Blitzortung                          | ❌      | global, uneven | blocked | ⚠️ Community — free for non-commercial; registration, rate-limited. No `ACAO`.     |
| ENTLN / Vaisala / Allison House      | ❌      | global         | n/a     | ❌ Paid — commercial license required                                               |

GLM is satellite-based **total** lightning — in-cloud plus cloud-to-ground, over
the GOES disks. It is not a ground network, so it neither replaces nor is
replaced by the rows under it.

## Climate & Historical Data

| Data Source               | Status | Domain | CORS        | Public Access                                                                                  |
| ------------------------- | ------ | ------ | ----------- | ---------------------------------------------------------------------------------------------- |
| Historical radar archives | ✅      | US     | open        | ✅ Free — AWS Open Data (NOAA)                                                                  |
| Historical storm reports  | ❌      | US     | simple only | ✅ Free — SPC archives, same host and same preflight limit as the SPC rows above                |
| Climate normals & records | ❌      | US     | open        | ✅ Free — NCEI (`www.ncei.noaa.gov`, `ACAO: *`)                                                 |
| Reanalysis (ERA5, NARR)   | ❌      | global | —           | ✅ Free — Copernicus CDS (registration, API key) / NCEP. Authenticated, so CORS is not the gate |

## Geographic / Base Layer Data

| Data Source                   | Status  | Domain | CORS    | Public Access                                                                                                                  |
| ----------------------------- | ------- | ------ | ------- | ------------------------------------------------------------------------------------------------------------------------------ |
| Basemap tiles (raster slippy) | ✅       | global | open    | ✅ Free — CartoDB light/dark, labels and no-labels, over OpenStreetMap data                                                     |
| NEXRAD site list              | ✅       | US     | n/a     | ✅ Compiled in — `rustdar-radar/src/sites.rs`, no network                                                                       |
| County/state/CWA boundaries   | Partial | US     | open    | ✅ Free — NWS alert zone geometry is fetched per alert from api.weather.gov and cached for a year; no standalone boundary layer |
| Roads / terrain / topo        | ❌       | global | open    | ✅ Free — OpenStreetMap / USGS (roads arrive as part of the basemap tiles, not as data)                                         |
| Elevation / DEM               | ❌       | global | open    | ✅ Free — USGS `elevation.nationalmap.gov` (`ACAO: *`, preflight OK) / SRTM                                                     |
| Land use / population density | ❌       | US     | blocked | ✅ Free — USGS NLCD / Census TIGER (`www2.census.gov`, no `ACAO`)                                                               |

---

## Access protocols and decoders

The tables above record **where** bytes come from. This one records **how** they
are read, because for several ❌ rows the decoder is the whole cost and the
endpoint is trivial.

**What the tree decodes today:**

| Format                                               | Where                                                           |
| ---------------------------------------------------- | --------------------------------------------------------------- |
| NEXRAD Level II (LDM records, Msg 31)                | `vendor/nexrad-decode`, `vendor/nexrad-data`, `vendor/bzip2-rs` |
| NEXRAD Level III (WMO, zlib/BZ2, radial packets)     | `nexrad-level3`                                                 |
| GRIB2 — DRT 5.3 (complex + spatial diff), 5.41 (PNG) | `grib` 0.17.1, `default-features = false`                       |
| HDF5 / NetCDF4                                       | `hdf5-pure` (`glm::h5`)                                         |
| GeoJSON / JSON                                       | `serde_json`                                                    |
| XML (S3 `ListObjectsV2`)                             | `xml` crate (`archive.rs`)                                      |
| XML / RSS (SPC mesoscale discussions)                | `roxmltree`                                                     |
| CSV (SPC storm reports)                              | hand-parsed, `spc::reports::fetch_csv`                          |
| zlib / deflate / gzip                                | `flate2`                                                        |
| Raster slippy tiles (basemap only)                   | `walkers`, in `rustdar-egui`                                    |

**What each ❌ row actually needs.** Verification shrank this list considerably —
three of the four biggest new sources need **nothing new at all**:

| Source                      | Decoder needed                                                                               |
| --------------------------- | -------------------------------------------------------------------------------------------- |
| MRMS                        | **None.** gzip + GRIB2 DRT 5.41, both already enabled.                                       |
| RRQPE                       | **None.** NetCDF4/HDF5 via `hdf5-pure`, plain 0.02° lat/lon grid.                            |
| GMGSI                       | **None.** NetCDF4/HDF5 via `hdf5-pure`, plain lat/lon grid.                                  |
| GOES ABI imagery            | Fixed-grid (geostationary perspective) → Mercator reprojection. The reader already exists.   |
| GIBS / EUMETView GeoColor   | A WMS `GetMap` client. New — nothing in the tree speaks WMS.                                 |
| SLIDER / Rain Viewer tiles  | Small. `walkers` already fetches slippy tiles; this is the same shape at a weather layer.    |
| NHC cones                   | KMZ — zip plus KML. New, both halves.                                                        |
| WPC surface fronts          | A parser for the coded surface analysis text. New, bespoke, nothing to lean on.              |
| GFS and other NOMADS models | Possibly a `grib` feature flip (5.40/5.42), both pure Rust and available at the current pin. |

Two standing constraints. **No C dependency may become unconditional**:
`openjpeg-sys`, `libaec-sys`, `proj-sys` and `libsqlite3-sys` were all
deliberately dropped from the `grib` pin because they do not cross-compile to
wasm32 or iOS, and the Android arm of `rustdar-overlays/Cargo.toml` records what
re-enabling them costs. And a `cfg(target_arch = "wasm32")` may select a value, a
dependency or a type alias — never fork behaviour inside a function body.
