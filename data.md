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

**Every CORS value in this file was probed** — `GET` and `OPTIONS` preflight,
with an `Origin:` header, against the exact endpoint named in the row — on
**2026-08-21**, or on **2026-09-11** where the row says so (the rows added
after reading the [master-weather-model-list](https://github.com/KnownStormChaser/master-weather-model-list)
catalogue, and every bucket that catalogue named for a row this file had
called `blocked`). Nothing here is inferred. `squallar-source/src/origins.rs` remains the
authority for the origins the app actually uses; this file covers those plus
every candidate. To re-probe a row:

```sh
curl -sS -o /dev/null -D - -H 'Origin: https://squallar.example' "$URL"
curl -sS -o /dev/null -D - -X OPTIONS \
     -H 'Origin: https://squallar.example' \
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

**A NODD bucket is usually `open`, but not always.** Of the twenty-six AWS
Open Data buckets probed across 2026-08-21 and 2026-09-11, twenty-four carry
`ACAO: *`; **`noaa-rap-pds` and `noaa-ndfd-pds` do not** (`200` with no `ACAO`,
`OPTIONS` `403`). When a row is `blocked`, look for a bucket mirror before
accepting it — and probe the bucket, because the bucket is not the verdict.

---

## Radar Data

| Data Source                       | Status | Domain                          | CORS    | Public Access                                                                                                                                            |
| --------------------------------- | ------ | ------------------------------- | ------- | -------------------------------------------------------------------------------------------------------------------------------------------------------- |
| NEXRAD Level 2 (archive)          | ✅      | US + territories                | open    | ✅ Free — AWS Open Data `unidata-nexrad-level2`                                                                                                           |
| NEXRAD Level 2 real-time chunks   | ✅      | US + territories                | open    | ✅ Free — AWS Open Data `unidata-nexrad-level2-chunks`; ~55 pieces per volume, each landing seconds after collection                                      |
| NEXRAD Level 3                    | ✅      | US + territories                | open    | ✅ Free — AWS Open Data `unidata-nexrad-level3` (SRM tilts 1–3 discontinued upstream)                                                                     |
| MRMS (Multi-Radar/Multi-Sensor)   | ✅      | CONUS shipped; AK, HI, Guam, Caribbean available | open    | ✅ Free — AWS Open Data `noaa-mrms-pds`, `us-east-1`. GRIB2, gzipped. **See the verified detail below.**                                                  |
| MRMS via NCEP                     | ❌      | same                            | blocked | ✅ Free — `mrms.ncep.noaa.gov/2D/`. No `ACAO`. The bucket above is the same data and is `open`; prefer it.                                                |
| NOAA Enterprise Rain Rate (RRQPE) | ❌      | **70°N – 60°S**, all longitudes | open    | ✅ Free — AWS Open Data `noaa-enterprise-rainrate-pds`. Satellite QPE; the precipitation answer outside ground-radar coverage                             |
| Rain Viewer v2 tiles              | ❌      | varies by provider              | open    | ⚠️ Attribution required — `api.rainviewer.com/public/weather-maps.json`. Free, no key; see the terms below. A self-hosted LibreWXR serves the same shape |

**Radar outside the United States.** Every row above runs out of world at the
US border. The catalogue lists sixteen national and multinational networks
with open gridded feeds; the ones probed on 2026-09-11 are below. **ODIM HDF5
is the common container across Europe**, and the HDF5 half of it is already
solved (`hdf5-pure`) — the decoder cost is the ODIM data model, once, for
many networks.

| Data Source                          | Status | Domain                    | CORS    | Public Access                                                                                                                                                                  |
| ------------------------------------ | ------ | ------------------------- | ------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| OPERA pan-European composite         | ❌      | Europe                    | open    | ✅ Free, CC BY 4.0 — EUMETNET via the MeteoGate EDR API `api.meteogate.eu/eu-eumetnet-weather-radar` (anonymous, rate-limited; free key raises it). ODIM HDF5: max reflectivity 1 km / 5 min, rain rate 15 min. The CloudFerro S3 cache `openradar-24h` is `blocked` (no `ACAO`) |
| UK Met Office 1 km rain-rate composite | ❌    | UK                        | open    | ⚠️ CC BY-**SA** 4.0 (ShareAlike) — `met-office-radar-obs-data` (AWS `eu-west-2`), `radar/YYYY/MM/DD/*.h5`, ODIM HDF5, every 15 min, ~1.4 MB, 2-year rolling window. Best-effort service, no SLA |
| FMI Finland composites + volumes     | ❌      | Finland                   | open    | ✅ Free, CC BY 4.0 — `fmi-opendata-radar-geotiff` (composites, **GeoTIFF**, scaled integers) and `fmi-opendata-radar-volume-hdf5` (ODIM volumes), AWS `eu-west-1`, ~5 min cadence |
| MET Norway Nordic composite          | ❌      | Norway, Sweden, Finland   | open    | ✅ Free, CC BY 4.0 — `thredds.met.no`, NetCDF pCAPPI reflectivity mosaic every 5 min. Fair-use policy: one request at a time, no parallel downloads                              |
| DWD Germany (RADOLAN + composites)   | ❌      | Germany                   | blocked | ✅ Free, GeoNutzV (attribution) — `opendata.dwd.de/weather/radar/`. No `ACAO`, `OPTIONS` `405`. RADOLAN binary / ODIM HDF5, hours of retention                                    |
| ČHMÚ, SHMÚ, HungaroMet, KAUR, MeteoSwiss, DMI, DPC SRI, KNMI, Hochficht | ❌ | one country each | — | ✅ Free — in the catalogue, not probed here. Mostly ODIM HDF5; KNMI needs a free account, DPC SRI's live feed does too                                                          |
| Canada (ECCC / MSC)                  | ❌      | Canada                    | n/a     | ❌ No gridded feed exists — Datamart is rendered GIF and GeoMet is WMS imagery                                                                                                     |

### MRMS, verified

Read off the bucket on 2026-08-21, not from documentation:

- **Top-level prefixes:** `CONUS/`, `CONUS_5KM/`, `ALASKA/`, `HAWAII/`,
  `GUAM/`, `CARIB/`, `ANC/`, plus `ProbSevere/`, `ConvectProb/`, `unsupported/`.
- **The two products worth having** exist exactly as
  `CONUS/MergedReflectivityQCComposite_00.50/` and `CONUS/PrecipRate_00.00/`.
- **The bucket carries the whole severe-weather suite on the same grid, same
  container, same decoder** — listed under `CONUS/` on 2026-09-11, none of it
  in this file until then: `MESH_00.50` plus `MESH_Max_{30,60,120,240,360,1440}min`
  (hail size and hail swaths), `RotationTrack{30…1440}min` and
  `RotationTrackML*` (low- and mid-level rotation tracks),
  `MergedAzShear_0-2kmAGL` / `_3-6kmAGL`, `POSH`, `SHI`, `VII`, `VIL_Density`,
  `EchoTop_18`, `H50_Above_0C` / `H60_Above_-20C`, `Reflectivity_-10C`,
  `ReflectivityAtLowestAltitude`, `MergedRhoHV_*` at 33 tilts, `PrecipFlag`,
  `SeamlessHSR`, `RadarQualityIndex`, `LightningProbabilityNext{30,60}minGrid`,
  and the **FLASH** flash-flood family (`FLASH_QPE_FFG{01,03,06}H`,
  `FLASH_QPE_ARI*`, `FLASH_CREST_MAXUNITSTREAMFLOW`, …). The reserved-value
  rule below is per product; each one added needs its own measured row.
- **`ProbSevere/YYYYMMDD/MRMS_PROBSEVERE_YYYYMMDD_HHMMSS.json`** — NSSL
  ProbSevere storm objects (probability of severe hail / wind / tornado per
  storm polygon), ~2 min cadence, back to 2020-10-14. **GeoJSON, not GRIB2**:
  the one MRMS product `serde_json` reads today.
- **Files are `.grib2.gz`** — gzip around GRIB2, e.g.
  `MRMS_PrecipRate_00.00_20260101-000000.grib2.gz`, ~660 KB.
- **Grid definition template 3.0** (plain lat/lon), 24,500,000 points —
  3500 × 7000 at 0.01°. Simpler than HRRR, which is template 3.30 (Lambert
  conformal) and needed `hrrr::lambert` written by hand.
- **Data representation template 5.41 — PNG.** Not 5.3, and not JPEG 2000.
- **Section 6 `bitmap_indicator = 255` — there is no bitmap.** Missing data is
  in-band, and **the reserved values are per product**, measured over whole
  granules rather than read from documentation:

  | Product | Reserved | Share of a 24.5 M-point granule |
  |---|---|---|
  | `MergedReflectivityQCComposite_00.50` | −999 no coverage, −99 below threshold | 34.0 % and 61.2 % |
  | `PrecipRate_00.00` | **−3** no coverage; **no −999, no −99 at all** | 34.4 % |

  Taking the composite's set for the rate leaves a third of that mosaic
  reporting **−3 mm/h** as a measurement. A blanket "negative means missing"
  rule fixes the rate and breaks the composite, which carries 8 045 genuine
  returns below 0 dBZ — 347 of them at exactly −3.0. The table lives in
  `MrmsProduct::missing_codes`; both granules are committed to
  `squallar-overlays/testdata/` because a suite built on one of them declared
  the other correct.
- **Timestamps are not clock-aligned** — `000039`, `000242`, `000442`, `000641`
  across one observed hour — so a key is never constructed from a rounded wall
  clock. Every fetch lists a day prefix.
- **`CONUS_5KM/` is dead**, stopping ~2021-02-24. It is still in the listing and
  reads like a cheaper CONUS. It is not.
- Retained back to **2020-10-14**; a granule is ~1.3 MB gzipped and decodes to
  **98 MB** of `f32`, which is why the layer's cache is byte-budgeted.

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
  Delivered as **NetCDF4, which is HDF5** — the same container `squallar-netcdf`
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

The Level 3 bucket also carries the **storm attribute products** this app has
never asked for, and which a storm-tracking view wants: `NST` (storm tracking
information), `NMD` (mesocyclone detection), `NTV` (tornado vortex signature),
`NHI` (hail index), `DVL` (digital VIL), `EET`, `DSP` / `DTA` / `OHA`
(precipitation accumulations), and the dual-pol tilt set `N0C` / `N0K` /
`N0H` / `N0X`. All present for `TLX_*_2026_09_10` on 2026-09-11. The attribute
products are point lists inside the same radial-packet container
`nexrad-level3` already parses.

Both radar networks share those buckets, which is not the same as sharing the
products. The Level 2 archive carries TDWR volumes under the same
`YYYY/MM/DD/SITE/` prefix as WSR-88D ones, keyed `_V08` rather than `_V06`. The
Level 3 bucket carries TDWR products too, but the legacy single-pol set (`TZL`,
`TZ0`-`TZ2`, `TV0`-`TV2`, `NCR`, `NHI`, `NMD`, …) — not one of the four codes
this app asks for, so the five Level 3 products above are a WSR-88D feature.

## Numerical Weather Prediction (NWP) Models

| Model                                  | Status | Domain            | CORS    | Public Access                                                                                                                                                                                                                                         |
| -------------------------------------- | ------ | ----------------- | ------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| HRRR (High-Resolution Rapid Refresh)   | ✅      | **CONUS only**    | open    | ✅ Free — AWS Open Data `noaa-hrrr-bdp-pds`, `.idx` byte-ranged. Analysis hour only: f00, or f01 for the windowed updraft-helicity maxima. **v4 is the final version**, frozen since 2020-12-02; retires with RRFSv2, no date                             |
| RRFS (Rapid Refresh Forecast System)   | ❌      | North America     | open    | ✅ Free — AWS Open Data `noaa-rrfs-ops-pds`, `rrfs.YYYYMMDD/HH/rrfs.tHHz.{2dfld,prslev}.3km.fFFF.{conus,ak}.grib2` + `.idx`, plus `13km.*.na`, `2p5km.*.{hi,pr}` and `.subh.` sub-hourly files (probed 2026-09-11). **The 3 km CONUS file is on HRRR's exact grid** — template 3.30, 1,905,141 points — so `hrrr::lambert` applies unchanged; the 13 km `na` file is template 3.1 (rotated lat/lon), which is new. **Operational 2026-10-14 12 UTC** (SCN 26-48). The old `noaa-rrfs-pds` is frozen — do not use |
| REFS (RRFS ensemble)                   | ❌      | North America     | open    | ✅ Free — same bucket, `refs.YYYYMMDD/HH/ensprod/`. Replaces HREF and SREF on 2026-10-14; 60 h                                                                                                                                                         |
| RAP (Rapid Refresh)                    | ❌      | North America     | blocked | ✅ Free — AWS Open Data `noaa-rap-pds` exists (`rap.YYYYMMDD/rap.tHHz.awip32fFF.grib2`) but **carries no `ACAO`**, `OPTIONS` `403` (2026-09-11). Native-only, like NOMADS. Retires with RRFSv2                                                          |
| NAM (North American Mesoscale)         | ❌      | North America     | open    | ✅ Free — AWS Open Data `noaa-nam-pds`, `.idx` sidecars (2026-09-11). **Retires 2026-10-14** with NAM Nest, HiresW, HREF and SREF (SCN 26-47). Do not build on it                                                                                      |
| GFS (Global Forecast System)           | ❌      | **global**        | open    | ✅ Free — AWS Open Data `noaa-gfs-bdp-pds`, `gfs.YYYYMMDD/HH/atmos/gfs.tHHz.pgrb2.0p25.fFFF` + `.idx` (2026-09-11) — **the same shape as HRRR**. Also `open`: GCS `global-forecast-system`, Azure `noaagfs.blob.core.windows.net`. NOMADS remains `blocked` |
| GEFS (Global Ensemble Forecast System) | ❌      | global            | open    | ✅ Free — AWS Open Data `noaa-gefs-pds`, `gefs.YYYYMMDD/HH/atmos/{pgrb2ap5,pgrb2sp25}/` (2026-09-11)                                                                                                                                                    |
| SREF (Short-Range Ensemble Forecast)   | ❌      | North America     | blocked | ✅ Free — NOAA NOMADS only. **Retires 2026-10-14**; REFS is the replacement                                                                                                                                                                             |
| NBM (National Blend of Models)         | ❌      | North America     | open    | ✅ Free — AWS Open Data `noaa-nbm-grib2-pds`, `blend.YYYYMMDD/HH/core/blend.tHHz.core.fFFF.{co,ak,…}.grib2` + `.idx` (2026-09-11). v5.0 since 2026-05-05. The COG bucket `noaa-nbm-pds` and NDFD's `noaa-ndfd-pds` were not / are not `open`           |
| HREF (High-Res Ensemble Forecast)      | ❌      | CONUS             | blocked | ✅ Free — NOAA NOMADS only; the AWS registry entry the catalogue cites is `404` and no `noaa-href*` bucket exists (2026-09-11). **Retires 2026-10-14**; REFS is the replacement                                                                          |
| ECMWF IFS + AIFS (open data)           | ❌      | global            | open    | ✅ Free, **CC BY 4.0 since 2025-10-01** — AWS `ecmwf-forecasts` (`eu-central-1`), `YYYYMMDD/HHz/{ifs,aifs-single,aifs-ens}/0p25/oper/*-oper-fc.grib2` + `.index` (2026-09-11); GCS `ecmwf-open-data` also `open`. `data.ecmwf.int` stays `blocked`. CCSDS packing → DRT 5.42 |
| UKMO Global + UKV                      | ❌      | global / UK       | open    | ✅ Free — `met-office-atmospheric-model-data` (AWS `eu-west-2`, 2026-09-11)                                                                                                                                                                             |
| ICON global / ICON-EU / ICON-D2        | ❌      | global / Europe   | blocked | ✅ Free — DWD `opendata.dwd.de/weather/nwp/`. No `ACAO`, `OPTIONS` `405` (2026-09-11). GRIB2, one message per file, bzip2                                                                                                                              |
| GDPS / RDPS / HRDPS (ECCC)             | ❌      | global / Canada   | blocked | ✅ Free — MSC Datamart `dd.weather.gc.ca`. No `ACAO` (2026-09-11). GRIB2                                                                                                                                                                                |
| AROME / ARPEGE (Météo-France)          | ❌      | France / global   | —       | ✅ Free — `data.gouv.fr` packages behind a Météo-France API key. The community AWS mirror `mf-models-on-aws` is gone (`NoSuchBucket`)                                                                                                                  |

**HRRR is CONUS-only, and that is why every row under it exists.** Each one is
either wider coverage or more members. A specificity-first stack — regional
model where one exists, global model everywhere else — is where this table
points; nothing in the tree implements a fallback chain.

**The NOMADS verdicts were true and are no longer the whole story.** Every
NOMADS row was `blocked` on 2026-08-21 and still is; but on 2026-09-11 the
NODD mirrors for GFS, GEFS, NAM, NBM and RRFS all answered `ACAO: *` on both
`GET` and `OPTIONS`, and so did ECMWF's own bucket. GFS in particular is a
`.idx`-sidecar GRIB2 bucket laid out like HRRR's — the global fallback needs
no new access path at all. **RAP is the exception**: its bucket exists and is
not `open`. NOMADS OPeNDAP and FTPPRD were retired on 2026-02-23 (SCN 25-81 /
25-82); the `filter_*.pl` scripts still work for native.

**2026-10-14 12 UTC is a cliff.** NWS SCN 26-47 retires NAM, NAM Nest, HiresW
(all but Guam), HREF and SREF on that cycle; SCN 26-48 brings RRFSv1 and REFS
into operations on the same one. Three of the rows above are dead rows after
that date; RRFS/REFS is the only thing worth building. HRRR and RAP survive
into RRFSv2 with no retirement notice issued, but HRRR is frozen at v4 — the
one model this app ships against has an end of life with no date on it.

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
| Simulated reflectivity                               | ✅      | ✅ HRRR: `REFC`, `REFD` at 1 km / 4 km / −10 °C, `MAXREF`, plus `RETOP` and `VIL`         |
| Wildfire smoke (near-surface and column)             | ❌      | ✅ HRRR: `MASSDEN:8 m above ground` and `COLMD:entire atmosphere` — in the `wrfsfc` file the app already indexes (2026-09-11). Also `RAP_Smoke/` and `HYSPLIT_Smoke/` on `noaa-nws-naqfc-pds` |
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
| Fire Weather Outlooks (Day 1–8)        | ✅       | CONUS  | simple only | ✅ Free — SPC GeoJSON endpoints; 28 paths, all `200` when probed 2026-08-21                                                  |
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
| HAFS (hurricane model, HFSA + HFSB)       | ❌      | storm-following | open    | ✅ Free — AWS Open Data `noaa-nws-hafs-pds`, `hfsa/`, `hfsb/` (probed 2026-09-11). GRIB2 + `.idx`, 3-hourly, and `trak.atcfunix` tracks. v2.2 on 2026-10-13 adds 10 m gust |
| ATCF a-decks (model track guidance)       | ❌      | all basins      | blocked | ✅ Free — `ftp.nhc.noaa.gov/atcf/aid_public/` over HTTPS, `200` with no `ACAO` (2026-09-11). Every model's track for every active storm — the spaghetti plot. Fixed-width text |

Both endpoints were fetched on 2026-08-21 and both are live. The WPC file opens
`ASUS02 KWBC` / `CODSUS` / `CODED SURFACE FRONTAL POSITIONS` — plain coded text,
not GeoJSON. `CurrentStorms.json` returns an `activeStorms` array whose entries
carry `id`, `name`, `classification`, `intensity`, `pressure`,
`latitudeNumeric`/`longitudeNumeric`, `movementDir`/`movementSpeed` and
`lastUpdate`; the cone and track are KMZ links off that record.

**Every NHC host is `blocked`, so the analysed products are native-only** until
something fronts them — the clearest case in the file for the server crate.
HAFS is the exception: model output arrives through a NODD bucket like HRRR.

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
| Himawari-9 (W Pacific)         | ❌      | W Pacific disk      | open    | ✅ Free — AWS Open Data `noaa-himawari9`: `AHI-L1b-FLDK/`, `AHI-L1b-Japan/`, `AHI-L1b-Target/`, plus L2 cloud/wind sets. `noaa-himawari8` is the backup       |
| GEO-KOMPSAT-2A (Asia-Pacific)  | ❌      | 128.2°E disk        | open    | ✅ Free — AWS Open Data `noaa-gk2a-pds`, `AMI/L1B/FD/YYYYMM/DD/HH/gk2a_ami_le1b_<band>_fd020ge_<stamp>.nc` (probed 2026-09-11). 16 bands, full disk every 10 min, NetCDF-4, since 2023-02. The disk between Himawari and Meteosat-IODC |
| Meteosat MTG / MSG-IODC        | ❌      | Europe / Indian     | open    | ✅ Free of charge — EUMETSAT; the WMS row below is the practical route. No AWS Open Data mirror was found                                                     |
| Polar-orbiting (JPSS/VIIRS)    | ❌      | global, swaths      | open    | ✅ Free — AWS Open Data `noaa-nesdis-n21-pds` / `-n20-pds` / `-snpp-pds` (probed 2026-09-11, `ACAO: *`). NOAA CLASS at `www.class.noaa.gov` is still `blocked` and is only the deep archive |
| Polar-orbiting (MetOp)         | ❌      | global, mid-morning | —       | ✅ Free with registration — EUMETSAT Data Store; no anonymous bucket                                                                                            |

**Pre-made composites — someone else renders:**

| Data Source                   | Status | Domain              | CORS    | Public Access                                                                                                         |
| ----------------------------- | ------ | ------------------- | ------- | --------------------------------------------------------------------------------------------------------------------- |
| NOAA GMGSI global mosaic      | ✅      | **72.7°N – 72.7°S** | open    | ✅ Free — AWS Open Data `noaa-gmgsi-pds`. Four channels shipped. **See the verified detail below.**                    |
| CIRA GeoColor, GOES East/West | ❌      | Americas            | open    | ✅ Free — NASA GIBS **WMS**. Layers verified present: `GOES-East_ABI_GeoColor`, `GOES-West_ABI_GeoColor`               |
| MTG GeoColour + MSG / IODC    | ❌      | Europe / Indian     | open    | ✅ Free of charge — EUMETView **WMS**. Verified present: `mtg_fd:rgb_geocolour`, plus `msg_fes:*` and `msg_iodc:*`     |
| Himawari via CIRA SLIDER      | ❌      | W Pacific           | blocked | ⚠️ No published licence — slippy tiles at `slider.cira.colostate.edu`. No `ACAO` either; use `noaa-himawari9` instead |

### GMGSI, verified

The three NOAA pages describing GMGSI **disagree with each other**: the AWS
registry says global at ~8 km, NOAA VLab says 71°N–71°S at 8 km, and the OSPO
product page says 60°N–60°S at ~3 km. None of them matches what the bucket
contains.

**Re-measured 2026-08-22 against the shipping decoder**, on the four
2025-06-01 12:00 UTC granules. Several figures previously recorded here came
from the *retired* product and have been corrected; see the two-generations
note below.

- Grid **3000 × 5000** = 15,000,000 points, matching
  `quality_information:total_number_retrievals = 15000000`. (The 3000 × 4999
  recorded here earlier is the legacy granule, which is a `ncks -d xc,0,4998`
  crop of the same field.)
- Latitude runs **+72.71541° to −72.73677°**; longitude covers the full turn.
- **The grid is separable but NOT a plain lat/lon grid.** Latitude depends only
  on the row and longitude only on the column — verified exactly, deviation 0.0
  — but the latitude axis is uniform in **Mercator y**, not in latitude. Rows
  step −14.52°, −24.41°, −33.86°, −33.83°, −24.36° per 500 rows, a 2.3×
  spread, while Mercator y steps a constant −0.628397 to within 1e−6.
- **The declared attributes cannot rebuild either axis.**
  `geospatial_lat_resolution = geospatial_lon_resolution = 0.0722`, where the
  longitude array steps **0.0720089** (measured across the 4998 steps from
  column 1 to column 4999) — 0.955° of accumulated drift. Interpolating the
  declared corners linearly puts row 500 at 48.4653° where the array says
  **58.19307°**, off by **9.73°**. Both axes must be read from the arrays.
- **`lat` and `lon` are two-dimensional `(yc, xc)`** — 15,000,000 floats each,
  60 MB apiece — not the 1-D axes the separability would allow. Each is stored
  as **one** chunk, SHUFFLE + DEFLATE, so any read of either used to inflate and
  unshuffle the whole 60 MB; `squallar_netcdf::Granule::read_picked_f32` gathers
  the 11,141 elements an axis walk wants out of the inflating stream instead.
- **The grid itself moves between product generations.** The arrays are
  byte-identical — stored bytes, not just values — across all four channels and
  across dates within a generation, and the column count went **5000 → 4999**
  between 2025-12-25 and 2026-09-08: the 4999-column axis is the 5000-column one
  with its last column dropped, the latitude axis unchanged. Measured on six
  real granules (LW/SW/VIS/WV, 2025-06-01, 2025-06-02, 2025-12-25, 2026-09-08)
  on 2026-09-10. So the axes may be remembered from the last granule — keyed on
  the stored bytes, which is what `gmgsi::decode::AxisCache` does — but never
  shipped as a constant.
- **The longitude axis is not monotonic.** Column 0 holds `+179.99962` and
  column 1 `−179.92838`: the grid starts a hair west of the antimeridian and
  every longitude is already wrapped into [−180, 180]. `geospatial_lon_min` /
  `_max` are the axis *extremes*, i.e. columns 1 and 0, not its first and last.
- **Values are 0–255 integer counts, not Kelvin**, despite `units = "K"`.
  `long_name = "0-255 Brightness Temperature"` is the honest attribute: over all
  15,000,000 samples every value is an integer in `0..=255`, none fractional,
  none outside. Measured equator readings at (row 1499, column 2500), 12 UTC:
  **LW 82, SW 65, VIS 118, WV 166**. A Kelvin-scaled ramp renders the layer
  entirely blank. Higher count is colder, so the greyscale ascends.
- `_FillValue = -9999` and it **never occurs in a healthy granule** — 0 of
  15,000,000 on all four channels, with
  `percentage_optimal_retrievals = 100`.
- **NetCDF4, i.e. HDF5** — chunked `(1, 793, 1322)`, SHUFFLE + DEFLATE level 5.
  `hdf5-pure` reads all of that; verified against the real NOAA bytes.
- `platform = "Meteosat9,Meteosat10,G19,H-9,G18"`,
  `instrument = "MSG-SEVIRI,GOES-ABI,Himawari-AHI"`, `processing_level = "Level 3"`.
  The `source` attribute lists the actual inputs, and **MSG-IODC is among them**.
- **Cadence is hourly, but each granule is a 10-minute window**:
  `time_coverage_start` → `time_coverage_end` spans `00:00:00Z` to `00:09:59Z`.
  It is a snapshot published hourly, not an hourly composite.
- **The object name cannot be constructed.** It ends in the blend's creation
  stamp — `_c202506011234579` — which trailed the observation by 34 to 42
  minutes and by a different amount per channel. The key is always listed.

#### Two product generations shared the bucket

Until mid-2025 each hour also carried a legacy McIDAS-derived granule named
`GLOBCOMP{LIR,SIR,VIS,WV}_nc.YYYYMMDDHH`: 4999 columns, `units = "none"`, no
`geospatial_*`, no `quality_information`, no `dqf`. **It is no longer
produced** — listing `GMGSI_LW/2026/08/20/12/` returns the `v3r0_blend` object
alone. Squallar reads the `v3r0_blend` product and skips the legacy name when a
historical hour offers both.

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

**GMGSI was the cheapest global cloud layer available**, and it shipped: already
merged, already on a separable grid, and in a container `hdf5-pure` already
reads. No per-satellite navigation, no disk reprojection, no new dependency —
the only new geometry is `GridCoords::Separable`, one axis per dimension.

### The GOES buckets carry more than imagery

Listed at the top of `noaa-goes19` on 2026-09-11, each in the `C` / `F` / `M`
(CONUS / full-disk / mesoscale) triplet: **`ABI-L2-FDC`** (fire / hot-spot
characterization — the satellite fire-detection layer), `ABI-L2-DMW` and
`DMWV` (derived motion winds), `ABI-L2-ACHA` (cloud-top height), `ACTP`
(cloud-top phase), `TPW` (total precipitable water), `LST`, `AOD`, `ACM`
(clear-sky mask), `CTP`, `RRQPE` (full-disk only), plus `ABI-Flood-Hourly`
as shapefiles and TIFs. Same container as GLM, same fixed-grid reprojection
cost as the imagery — the reprojection is paid once for the whole family.

### GeoColor is a recipe, not a file

Nothing in the AWS GOES buckets is "GeoColor" — it is CIRA's day/night RGB
blend. Either build it from ABI bands (`MCMIP` carries the multiband product
that makes this tractable) or fetch it rendered. Both WMS routes are `open`, and
both were confirmed to serve it by name.

**What the fetch route costs: a WMS client.** Nothing in the tree speaks WMS.
That is the single blocker on the two GeoColor rows, and it is shared between
them.

### The ABI decoder is closer than the ❌ suggests

GLM L2 LCFA is NetCDF4, which is HDF5, and `squallar-netcdf` already reads it with
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
| Basemap (PMTiles vector archive) | ✅    | global | open    | ✅ Self-hosted — tiles.squallar.app, OpenMapTiles schema over OpenStreetMap data, rendered client-side                          |
| NEXRAD site list              | ✅       | US     | n/a     | ✅ Compiled in — `squallar-radar/src/sites.rs`, no network                                                                       |
| County/state/CWA boundaries   | Partial | US     | open    | ✅ Free — NWS alert zone geometry is fetched per alert from api.weather.gov and cached for a year; no standalone boundary layer |
| Roads / terrain / topo        | ❌       | global | open    | ✅ Free — OpenStreetMap / USGS (roads arrive as part of the basemap tiles, not as data)                                         |
| Elevation / DEM               | ❌       | global | open    | ✅ Free — USGS `elevation.nationalmap.gov` (`ACAO: *`, preflight OK) / SRTM                                                     |
| Land use / population density | ❌       | US     | blocked | ✅ Free — USGS NLCD / Census TIGER (`www2.census.gov`, no `ACAO`)                                                               |

## In neither list

The catalogue is a model list; this file grew out of a radar app. Between them
they miss most of what a severe-weather view wants beside the radar. Every row
was probed on 2026-09-11.

**Severe weather, real-time:**

| Data Source                                  | Status | Domain | CORS        | Public Access                                                                                                                                                            |
| -------------------------------------------- | ------ | ------ | ----------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Local Storm Reports (LSR), live              | ❌      | US     | open        | ✅ Free — IEM `mesonet.agron.iastate.edu/geojson/lsr.php?sts=…&ets=…` (`ACAO: *`, `OPTIONS` `204`); also `api.weather.gov/products/types/LSR` as text. SPC's CSV is the *filtered* daily set; these are the raw NWS reports as they land |
| Storm-based warning polygons, live           | ✅      | US     | open        | ✅ Free — already via api.weather.gov; IEM `geojson/sbw.php` is the same set as one GeoJSON, and IEM's VTEC archive is the historical one                                 |
| NWS text products (AFD, HWO, NOW, SPS, PNS)  | ❌      | US     | open        | ✅ Free — `api.weather.gov/products/types/{AFD,…}` — the forecast discussion, hazardous-weather outlook and damage-survey PNS behind every office                          |
| NWS Damage Assessment Toolkit (tornado tracks, survey points) | ❌ | US | open   | ✅ Free — `services.dat.noaa.gov/arcgis/rest/services/nws_damageassessmenttoolkit/DamageViewer/FeatureServer` (`ACAO` echoes the origin). ArcGIS REST, GeoJSON `f=geojson` |
| SpotterNetwork positions                     | ❌      | US     | open        | ⚠️ Agreement — `www.spotternetwork.org/feeds/gr.txt` answers `200`, `ACAO: *`, in **GRLevelX placefile** format; their API terms govern redistribution                     |
| SPC watch outlines (own geometry)            | Partial | CONUS | simple only | ✅ Free — `www.spc.noaa.gov/products/watch/` (`GET` `ACAO: *`, `OPTIONS` `403`, the SPC rule). The parallelogram itself, not the county list the alerts API gives           |
| Aviation hazards (SIGMET, AIRMET, PIREP, TAF) | ❌     | US     | blocked     | ✅ Free — `aviationweather.gov/api/data/{airsigmet,pirep,taf,metar}` — `200` with no `ACAO`. Convective SIGMETs and PIREPs are the aviation view of the same storms          |

**Hydrology:**

| Data Source                            | Status | Domain | CORS    | Public Access                                                                                                                                                   |
| -------------------------------------- | ------ | ------ | ------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| MRMS FLASH (flash flooding)            | ❌      | CONUS  | open    | ✅ Free — in the MRMS bucket already listed: `FLASH_QPE_FFG*` (QPE-to-guidance ratio), `FLASH_QPE_ARI*`, `FLASH_CREST_MAXUNITSTREAMFLOW`. Same decoder as PrecipRate |
| National Water Model                   | ❌      | CONUS, AK, HI, PR | open | ✅ Free — AWS Open Data `noaa-nwm-pds`, `nwm.YYYYMMDD/{analysis_assim,short_range,medium_range,…}/`, NetCDF. Channel routing on 2.7 M reaches — needs the reach geometry to draw |
| River gauges (stage, flood category)   | ❌      | US     | open    | ✅ Free — NWPS `api.water.noaa.gov/nwps/v1/gauges/{lid}` (`ACAO: *`; `HEFO2` answers `200`, and `?bbox.xmin=…` lists by extent); USGS `waterservices.usgs.gov/nwis/iv/` (`ACAO: *`) for the instantaneous values                     |
| Tides and coastal water level          | ❌      | US coast | open  | ✅ Free — CO-OPS `api.tidesandcurrents.noaa.gov/api/prod/datagetter` (`ACAO: *`), JSON                                                                                |
| Snow analysis (SNODAS, snowfall)       | ❌      | CONUS  | blocked | ✅ Free — NOHRSC via NSIDC `noaadata.apps.nsidc.org/NOAA/G02158/` and `www.nohrsc.noaa.gov/snowfall/data/` — neither carries `ACAO`                                        |
| Drought Monitor                        | ❌      | US     | open    | ✅ Free — `droughtmonitor.unl.edu/data/json/usdm_current.json` (`ACAO: *`), weekly                                                                                         |

**Fire and smoke:**

| Data Source                        | Status | Domain | CORS    | Public Access                                                                                                                                                              |
| ---------------------------------- | ------ | ------ | ------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| GOES fire detection (`ABI-L2-FDC`) | ❌      | Americas | open  | ✅ Free — in `noaa-goes19` / `noaa-goes18` already listed, `ABI-L2-FDCC/YYYY/DDD/HH/`. Hot-spot mask + fire radiative power, every 5 min CONUS                                 |
| Active fire perimeters (WFIGS)     | ❌      | US     | open    | ✅ Free — NIFC ArcGIS `services3.arcgis.com/T4QMspbfLg3qTGWY/arcgis/rest/services/WFIGS_Interagency_Perimeters_Current/FeatureServer/0` (`ACAO: *`), `f=geojson`             |
| HMS smoke plumes and fire points   | ❌      | N. America | blocked | ✅ Free — `satepsanone.nesdis.noaa.gov/pub/FIRE/web/HMS/`, shapefile/KML, no `ACAO`. Analyst-drawn; the HRRR `MASSDEN` row above is the model view                        |
| VIIRS/MODIS active fire (FIRMS)    | ❌      | global | —       | ✅ Free with a MAP_KEY — `firms.modaps.eosdis.nasa.gov/api/area/`; `400` without one, so CORS is not the gate                                                                |

**Everything else:**

| Data Source                       | Status | Domain | CORS | Public Access                                                                                                                                                            |
| --------------------------------- | ------ | ------ | ---- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| NWS point / gridded forecast      | ❌      | US     | open | ✅ Free — `api.weather.gov/gridpoints/{office}/{x},{y}` and `/forecast`, `/forecast/hourly`. The NDFD grid the `noaa-ndfd-pds` bucket refuses to serve cross-origin       |
| NWS station observations          | ❌      | US     | open | ✅ Free — `api.weather.gov/stations/{id}/observations/latest` — the same METARs as the IEM row, from an origin that survives a preflight                                    |
| Aurora (OVATION)                  | ❌      | global | open | ✅ Free — SWPC `services.swpc.noaa.gov/json/ovation_aurora_latest.json` (`ACAO: *`), 30-min probability grid                                                                |
| Synoptic Data (Mesowest) mesonets | ❌      | global | open | ⚠️ Token — `api.synopticdata.com/v2/stations/latest` (`ACAO: *`, `401` without a token). One origin for most US mesonets; the free tier is rate-limited                    |

**Two of these rows, and the HRRR smoke row above, are free.** MRMS FLASH,
GOES FDC and `MASSDEN`/`COLMD` are in buckets this file already reads, through
decoders the tree already has.
The rest are JSON or GeoJSON from `open` origins and cost a fetch each. The
`blocked` ones (aviation, HMS, NOHRSC, ATCF) join the NHC section as the
server crate's queue.

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
| HDF5 / NetCDF4                                       | `hdf5-pure` (`squallar-netcdf`)                                         |
| GeoJSON / JSON                                       | `serde_json`                                                    |
| XML (S3 `ListObjectsV2`)                             | `xml` crate (`archive.rs`); `roxmltree` in `glm::fetch`, `mrms::fetch` |
| XML / RSS (SPC mesoscale discussions)                | `roxmltree`                                                     |
| CSV (SPC storm reports)                              | hand-parsed, `spc::reports::fetch_csv`                          |
| zlib / deflate / gzip                                | `flate2`                                                        |
| Raster slippy tiles (basemap only)                   | `walkers`, in `squallar-egui`                                    |
| ArcGIS REST / GeoJSON (`f=geojson`)                  | `serde_json` — DAT, WFIGS, IEM, NWPS all arrive this way         |

**What each ❌ row actually needs.** Verification shrank this list considerably —
three of the four biggest new sources need **nothing new at all**:

| Source                      | Decoder needed                                                                               |
| --------------------------- | -------------------------------------------------------------------------------------------- |
| ~~MRMS~~ (**shipped**)      | **None**, as predicted. gzip + GRIB2 DRT 5.41, both already enabled.                         |
| RRQPE                       | **None.** NetCDF4/HDF5 via `hdf5-pure`, plain 0.02° lat/lon grid.                            |
| ~~GMGSI~~ (**shipped**)     | **None**, as predicted. NetCDF4/HDF5 via `hdf5-pure`. Not a plain lat/lon grid, though: separable, uniform in Mercator y. |
| GOES ABI imagery            | Fixed-grid (geostationary perspective) → Mercator reprojection. The reader already exists.   |
| GIBS / EUMETView GeoColor   | A WMS `GetMap` client. New — nothing in the tree speaks WMS.                                 |
| SLIDER / Rain Viewer tiles  | Small. `walkers` already fetches slippy tiles; this is the same shape at a weather layer.    |
| NHC cones                   | KMZ — zip plus KML. New, both halves.                                                        |
| WPC surface fronts          | A parser for the coded surface analysis text. New, bespoke, nothing to lean on.              |
| GFS, GEFS, NAM, NBM, RRFS   | **None** for the container — `.idx`-sidecar GRIB2 buckets laid out like HRRR. Grid templates read off the live files 2026-09-11: GFS 0.25° is **3.0** (plain lat/lon, 1,038,240 points); NAM CONUS nest, NBM `co` and RRFS 3 km CONUS are **3.30** Lambert — the nest and RRFS on HRRR's own 1,905,141-point grid; RRFS 13 km `na` is **3.1** rotated lat/lon, the one new geometry. |
| ECMWF IFS / AIFS            | `ccsds-unpack-with-rust-aec` (DRT 5.42), pure Rust, one feature flip at the current pin. `.index` sidecar is JSON-lines, not the wgrib2 `.idx` shape.                          |
| MRMS severe suite, FLASH    | **None.** Same container as PrecipRate; a measured reserved-value row per product.                                                                                            |
| ProbSevere                  | **None.** GeoJSON.                                                                                                                                                             |
| GOES `ABI-L2-FDC` and kin   | The same fixed-grid → Mercator reprojection as the imagery row. Pay it once.                                                                                                   |
| European ODIM HDF5 radar    | HDF5 is `hdf5-pure`; the ODIM data model (`/dataset1/data1/data`, `what/gain`+`offset`, `where` projection) is new and shared by OPERA, UK, FMI volumes, DWD, ČHMÚ, SHMÚ, DMI… |
| FMI GeoTIFF composites      | A GeoTIFF reader. New — nothing in the tree reads TIFF.                                                                                                                        |
| ATCF a-decks                | Fixed-width comma text; trivial, but the host is `blocked`.                                                                                                                    |
| GRLevelX placefiles         | Line-oriented text; trivial. SpotterNetwork only.                                                                                                                              |

Two standing constraints. **No C dependency may become unconditional**:
`openjpeg-sys`, `libaec-sys`, `proj-sys` and `libsqlite3-sys` were all
deliberately dropped from the `grib` pin because they do not cross-compile to
wasm32 or iOS, and the Android arm of `squallar-overlays/Cargo.toml` records what
re-enabling them costs. And a `cfg(target_arch = "wasm32")` may select a value, a
dependency or a type alias — never fork behaviour inside a function body.
