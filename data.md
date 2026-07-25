# Data Sources

This document details which data sources are needed for this project. Not all are implemented yet.

## Radar Data

|           Data Source           | Status | Public Access |
| ------------------------------- | ------ | ------------- |
| NEXRAD Level 2                  | ✅     | ✅ Free — AWS Open Data (NOAA) |
| NEXRAD Level 3                  | ✅     | ✅ Free — AWS Open Data `unidata-nexrad-level3` (CORS-clean; SRM tilts 1–3 discontinued upstream) |
| MRMS (Multi-Radar/Multi-Sensor) | ❌     | ✅ Free — NCEP / Iowa State |

## Numerical Weather Prediction (NWP) Models

|                 Model                  | Status | Public Access |
| -------------------------------------- | ------ | ------------- |
| HRRR (High-Resolution Rapid Refresh)   | ✅     | ✅ Free — AWS Open Data `noaa-hrrr-bdp-pds`, `.idx` byte-ranged (CORS-clean) |
| RAP (Rapid Refresh)                    | ❌     | ✅ Free — NOAA NOMADS |
| NAM (North American Mesoscale)         | ❌     | ✅ Free — NOAA NOMADS |
| GFS (Global Forecast System)           | ❌     | ✅ Free — NOAA NOMADS |
| GEFS (Global Ensemble Forecast System) | ❌     | ✅ Free — NOAA NOMADS |
| SREF (Short-Range Ensemble Forecast)   | ❌     | ✅ Free — NOAA NOMADS |
| NBM (National Blend of Models)         | ❌     | ✅ Free — NOAA NOMADS |
| ECMWF (if licensing allows)            | ❌     | ⚠️ Restricted — paid license for high-res operational data |
| HREF (High-Res Ensemble Forecast)      | ❌     | ✅ Free — NOAA NOMADS |

|                    NWP Parameter                     | Status | Public Access |
| ---------------------------------------------------- | ------ | ------------- |
| Temperature, dewpoint, wind (surface + upper air)    | ❌     | ✅ Derived from public models above |
| CAPE, CIN, SRH (Storm Relative Helicity), bulk shear | ❌     | ✅ Derived from public models above |
| Simulated reflectivity, updraft helicity             | ❌     | ✅ Derived from public models above |
| Precipitation (QPF), snow, ice                       | ❌     | ✅ Derived from public models above |
| 500 mb heights/vorticity, jet stream, thickness      | ❌     | ✅ Derived from public models above |
| Precipitable water (PWAT), LCL, LFC, EL              | ❌     | ✅ Derived from public models above |

## SPC (Storm Prediction Center) Data

|              Data Source               | Status | Public Access |
| -------------------------------------- | ------ | ------------- |
| Convective Outlooks (Day 1–8)          | ✅     | ✅ Free — SPC GeoJSON endpoints |
| Mesoscale Discussions (MDs)            | ✅     | ✅ Free — SPC RSS feed |
| Precipitation Discussions              | ❌     | ✅ Free — WPC website |
| Watches (Tornado/Severe Tstorm)        | ❌     | ✅ Free — SPC / NWS API |
| Storm Reports (preliminary & filtered) | ✅     | ✅ Free — SPC CSV files |
| Fire Weather Outlooks                  | ❌     | ✅ Free — SPC GeoJSON endpoints |
| SPC Mesoanalysis graphics              | ❌     | ✅ Free — SPC website (raster images) |
| Sounding data / SPC skew-T parameters  | ❌     | ✅ Free — SPC / University of Wyoming |

## Weather Alerts & Warnings

|                      Data Source                      | Status | Public Access |
| ----------------------------------------------------- | ------ | ------------- |
| NWS Alerts API                                        | ✅     | ✅ Free — api.weather.gov |
| Weather.gov API /alerts                               | ✅     | ✅ Free — api.weather.gov |
| Warning polygons (tornado, severe, flash flood, etc.) | ✅     | ✅ Free — NWS API zone geometries |

## Observational / Surface Data

|                Data Source                 | Status | Public Access |
| ------------------------------------------ | ------ | ------------- |
| METAR/ASOS (surface obs)                   | ✅     | ✅ Free — Iowa Environmental Mesonet `currents.json`, per state (CORS-clean) |
| Upper-air soundings (RAOB)                 | ❌     | ✅ Free — University of Wyoming / UCAR |
| Mesonets (state/regional surface networks) | ❌     | ⚠️ Mixed — varies by state; some free (e.g. Oklahoma, Iowa), many restricted or paywalled |
| Buoy / Marine obs                          | ❌     | ✅ Free — NDBC (NOAA) |
| ASOS 1-min data                            | ❌     | ✅ Free — NCEI (NOAA) |
| Storm spotter reports (mPING)              | ❌     | ✅ Free — NSSL mPING API |
| State traffic/weather cameras              | ❌     | ⚠️ Mixed — varies by state DOT; many have public feeds, no unified API |

## Satellite Imagery

|         Data Source         | Status | Public Access |
| --------------------------- | ------ | ------------- |
| GOES-16/18 (GOES-East/West) | ❌     | ✅ Free — AWS Open Data / NOAA CLASS |
| Himawari (Pacific)          | ❌     | ✅ Free — JMA (may have usage terms) |
| Polar-orbiting (JPSS/VIIRS) | ❌     | ✅ Free — NOAA CLASS |

## Lightning Data

|             Data Source              | Status | Public Access |
| ------------------------------------ | ------ | ------------- |
| GLM (Geostationary Lightning Mapper) | ✅     | ✅ Free — AWS Open Data (NOAA) |
| Blitzortung                          | ❌     | ⚠️ Community — free for non-commercial; requires registration, rate-limited API |
| ENTLN / Vaisala / Allison House?     | ❌     | ❌ Paid — commercial license required |

## Climate & Historical Data

|        Data Source        | Status | Public Access |
| ------------------------- | ------ | ------------- |
| Historical storm reports  | ❌     | ✅ Free — SPC archives |
| Climate normals & records | ❌     | ✅ Free — NCEI (NOAA) |
| Historical radar archives | ✅     | ✅ Free — AWS Open Data (NOAA) |
| Reanalysis (ERA5, NARR)   | ❌     | ✅ Free — Copernicus CDS (free registration) / NCEP |

## Geographic / Base Layer Data

|          Data Source          | Status | Public Access |
| ----------------------------- | ------ | ------------- |
| County/state/CWA boundaries   | ❌     | ✅ Free — Census TIGER / NWS shapefiles |
| Roads / terrain / topo        | ❌     | ✅ Free — OpenStreetMap / USGS |
| Elevation / DEM               | ❌     | ✅ Free — USGS / SRTM |
| Land use / population density | ❌     | ✅ Free — USGS NLCD / Census |
