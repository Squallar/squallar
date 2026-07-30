# Data Sources

This document details which data sources are needed for this project. Not all are implemented yet.

## Radar Data

| Data Source                     | Status | Public Access                                                                                                        |
| ------------------------------- | ------ | -------------------------------------------------------------------------------------------------------------------- |
| NEXRAD Level 2 (archive)        | ✅     | ✅ Free — AWS Open Data `unidata-nexrad-level2`                                                                      |
| NEXRAD Level 2 real-time chunks | ✅     | ✅ Free — AWS Open Data `unidata-nexrad-level2-chunks`; ~55 pieces per volume, each landing seconds after collection |
| NEXRAD Level 3                  | ✅     | ✅ Free — AWS Open Data `unidata-nexrad-level3` (CORS-clean; SRM tilts 1–3 discontinued upstream)                    |
| MRMS (Multi-Radar/Multi-Sensor) | ❌     | ✅ Free — NCEP / Iowa State                                                                                          |

Products derived locally from the Level 2 volume — HHC, POSH, MEHS, NROT,
interpolated echo tops and storm-relative velocity — need no source of their
own. KDP, EET, VIL, VIL density and precipitation rate are fetched from the
Level 3 bucket (`RadarProduct::is_level3`).

## Numerical Weather Prediction (NWP) Models

| Model                                  | Status | Public Access                                                                                                                                          |
| -------------------------------------- | ------ | ------------------------------------------------------------------------------------------------------------------------------------------------------ |
| HRRR (High-Resolution Rapid Refresh)   | ✅     | ✅ Free — AWS Open Data `noaa-hrrr-bdp-pds`, `.idx` byte-ranged (CORS-clean). Analysis hour only: f00, or f01 for the windowed updraft-helicity maxima |
| RAP (Rapid Refresh)                    | ❌     | ✅ Free — NOAA NOMADS                                                                                                                                  |
| NAM (North American Mesoscale)         | ❌     | ✅ Free — NOAA NOMADS                                                                                                                                  |
| GFS (Global Forecast System)           | ❌     | ✅ Free — NOAA NOMADS                                                                                                                                  |
| GEFS (Global Ensemble Forecast System) | ❌     | ✅ Free — NOAA NOMADS                                                                                                                                  |
| SREF (Short-Range Ensemble Forecast)   | ❌     | ✅ Free — NOAA NOMADS                                                                                                                                  |
| NBM (National Blend of Models)         | ❌     | ✅ Free — NOAA NOMADS                                                                                                                                  |
| ECMWF (if licensing allows)            | ❌     | ⚠️ Restricted — paid license for high-res operational data                                                                                            |
| HREF (High-Res Ensemble Forecast)      | ❌     | ✅ Free — NOAA NOMADS                                                                                                                                  |

| NWP Parameter                                        | Status | Public Access                                                                             |
| ---------------------------------------------------- | ------ | ----------------------------------------------------------------------------------------- |
| Temperature, dewpoint, wind (surface)                | ✅     | ✅ HRRR: 2 m temperature, 2 m dewpoint, surface wind gust                                 |
| Temperature, dewpoint, wind (upper air)              | ❌     | ✅ Derived from public models above                                                       |
| CAPE, CIN, SRH (Storm Relative Helicity), bulk shear | ✅     | ✅ HRRR: SB/ML/MU CAPE, SB/ML CIN, lifted index, 0–1 km and 0–3 km SRH, 0–6 km bulk shear |
| Updraft helicity                                     | ✅     | ✅ HRRR: 0–2 km and 2–5 km maxima, from f01 (the f00 record is identically zero)          |
| Simulated reflectivity                               | ❌     | ✅ Derived from public models above                                                       |
| Precipitation (QPF), snow, ice                       | ❌     | ✅ Derived from public models above                                                       |
| 500 mb heights/vorticity, jet stream, thickness      | ❌     | ✅ Derived from public models above                                                       |
| Precipitable water (PWAT)                            | ✅     | ✅ HRRR                                                                                   |
| LCL, LFC, EL                                         | ❌     | ✅ Derived from public models above                                                       |
| Surface visibility                                   | ✅     | ✅ HRRR                                                                                   |

## SPC (Storm Prediction Center) Data

| Data Source                            | Status  | Public Access                                                                                                                |
| -------------------------------------- | ------- | ---------------------------------------------------------------------------------------------------------------------------- |
| Convective Outlooks (Day 1–8)          | ✅      | ✅ Free — SPC GeoJSON endpoints                                                                                              |
| Mesoscale Discussions (MDs)            | ✅      | ✅ Free — SPC RSS feed                                                                                                       |
| Precipitation Discussions              | ❌      | ✅ Free — WPC website                                                                                                        |
| Watches (Tornado/Severe Tstorm)        | Partial | ✅ Free — arrive through the NWS Alerts API as county/zone geometry; the SPC watch parallelograms themselves are not fetched |
| Storm Reports (preliminary & filtered) | ✅      | ✅ Free — SPC CSV files                                                                                                      |
| Fire Weather Outlooks                  | ❌      | ✅ Free — SPC GeoJSON endpoints                                                                                              |
| SPC Mesoanalysis graphics              | ❌      | ✅ Free — SPC website (raster images)                                                                                        |
| Sounding data / SPC skew-T parameters  | ❌      | ✅ Free — SPC / University of Wyoming                                                                                        |

## Weather Alerts & Warnings

| Data Source                                           | Status | Public Access                     |
| ----------------------------------------------------- | ------ | --------------------------------- |
| NWS Alerts API                                        | ✅     | ✅ Free — api.weather.gov         |
| Weather.gov API /alerts                               | ✅     | ✅ Free — api.weather.gov         |
| Warning polygons (tornado, severe, flash flood, etc.) | ✅     | ✅ Free — NWS API zone geometries |

## Observational / Surface Data

| Data Source                                | Status | Public Access                                                                                                            |
| ------------------------------------------ | ------ | ------------------------------------------------------------------------------------------------------------------------ |
| METAR/ASOS (surface obs)                   | ✅     | ✅ Free — Iowa Environmental Mesonet `currents.json`, per state (CORS-clean)                                             |
| Upper-air soundings (RAOB)                 | ❌     | ✅ Free — University of Wyoming / UCAR                                                                                   |
| Environmental 0 °C / −20 °C heights        | ✅     | ✅ Free — Open-Meteo `/v1/forecast` (CORS-clean). Only these two levels, only to scale the hail products; not a sounding |
| Mesonets (state/regional surface networks) | ❌     | ⚠️ Mixed — varies by state; some free (e.g. Oklahoma, Iowa), many restricted or paywalled                               |
| Buoy / Marine obs                          | ❌     | ✅ Free — NDBC (NOAA)                                                                                                    |
| ASOS 1-min data                            | ❌     | ✅ Free — NCEI (NOAA)                                                                                                    |
| Storm spotter reports (mPING)              | ❌     | ✅ Free — NSSL mPING API                                                                                                 |
| State traffic/weather cameras              | ❌     | ⚠️ Mixed — varies by state DOT; many have public feeds, no unified API                                                  |

## Satellite Imagery

| Data Source                    | Status | Public Access                                                                                                                                   |
| ------------------------------ | ------ | ----------------------------------------------------------------------------------------------------------------------------------------------- |
| GOES-19/18 imagery (East/West) | ❌     | ✅ Free — AWS Open Data `noaa-goes19` / `noaa-goes18`. Both buckets are already read for GLM; no imagery product (ABI radiances/CMI) is decoded |
| Himawari (Pacific)             | ❌     | ✅ Free — JMA (may have usage terms)                                                                                                            |
| Polar-orbiting (JPSS/VIIRS)    | ❌     | ✅ Free — NOAA CLASS                                                                                                                            |

## Lightning Data

| Data Source                          | Status | Public Access                                                                    |
| ------------------------------------ | ------ | -------------------------------------------------------------------------------- |
| GLM (Geostationary Lightning Mapper) | ✅     | ✅ Free — AWS Open Data `noaa-goes19` (East) / `noaa-goes18` (West), L2 LCFA     |
| Blitzortung                          | ❌     | ⚠️ Community — free for non-commercial; requires registration, rate-limited API |
| ENTLN / Vaisala / Allison House?     | ❌     | ❌ Paid — commercial license required                                            |

## Climate & Historical Data

| Data Source               | Status | Public Access                                       |
| ------------------------- | ------ | --------------------------------------------------- |
| Historical storm reports  | ❌     | ✅ Free — SPC archives                              |
| Climate normals & records | ❌     | ✅ Free — NCEI (NOAA)                               |
| Historical radar archives | ✅     | ✅ Free — AWS Open Data (NOAA)                      |
| Reanalysis (ERA5, NARR)   | ❌     | ✅ Free — Copernicus CDS (free registration) / NCEP |

## Geographic / Base Layer Data

| Data Source                   | Status  | Public Access                                                                                                                   |
| ----------------------------- | ------- | ------------------------------------------------------------------------------------------------------------------------------- |
| Basemap tiles (raster slippy) | ✅      | ✅ Free — CartoDB light/dark, labels and no-labels, over OpenStreetMap data                                                     |
| NEXRAD site list              | ✅      | ✅ Compiled in — `rustdar-radar/src/sites.rs`, no network                                                                       |
| County/state/CWA boundaries   | Partial | ✅ Free — NWS alert zone geometry is fetched per alert from api.weather.gov and cached for a year; no standalone boundary layer |
| Roads / terrain / topo        | ❌      | ✅ Free — OpenStreetMap / USGS (roads arrive as part of the basemap tiles, not as data)                                         |
| Elevation / DEM               | ❌      | ✅ Free — USGS / SRTM                                                                                                           |
| Land use / population density | ❌      | ✅ Free — USGS NLCD / Census                                                                                                    |
