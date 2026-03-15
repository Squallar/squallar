# Data Sources

This document details which data sources are needed for this project. Not all are implemented yet.

## Radar Data

|           Data Source           | Status |
| ------------------------------- | ------ |
| NEXRAD Level 2                  | ✅     |
| NEXRAD Level 3                  | ✅     |
| MRMS (Multi-Radar/Multi-Sensor) | ❌     |

## Numerical Weather Prediction (NWP) Models

|                 Model                  | Status |
| -------------------------------------- | ------ |
| HRRR (High-Resolution Rapid Refresh)   | ❌     |
| RAP (Rapid Refresh)                    | ❌     |
| NAM (North American Mesoscale)         | ❌     |
| GFS (Global Forecast System)           | ❌     |
| GEFS (Global Ensemble Forecast System) | ❌     |
| SREF (Short-Range Ensemble Forecast)   | ❌     |
| NBM (National Blend of Models)         | ❌     |
| ECMWF (if licensing allows)            | ❌     |
| HREF (High-Res Ensemble Forecast)      | ❌     |

|                    NWP Parameter                     | Status |
| ---------------------------------------------------- | ------ |
| Temperature, dewpoint, wind (surface + upper air)    | ❌     |
| CAPE, CIN, SRH (Storm Relative Helicity), bulk shear | ❌     |
| Simulated reflectivity, updraft helicity             | ❌     |
| Precipitation (QPF), snow, ice                       | ❌     |
| 500 mb heights/vorticity, jet stream, thickness      | ❌     |
| Precipitable water (PWAT), LCL, LFC, EL              | ❌     |

## SPC (Storm Prediction Center) Data

|              Data Source               | Status |
| -------------------------------------- | ------ |
| Convective Outlooks (Day 1–8)          | ✅     |
| Mesoscale Discussions (MDs)            | ✅     |
| Precipitation Discussions              | ❌     |
| Watches (Tornado/Severe Tstorm)        | ❌     |
| Storm Reports (preliminary & filtered) | ❌     |
| Fire Weather Outlooks                  | ❌     |
| SPC Mesoanalysis graphics              | ❌     |
| Sounding data / SPC skew-T parameters  | ❌     |

## Weather Alerts & Warnings

|                      Data Source                      | Status |
| ----------------------------------------------------- | ------ |
| NWS Alerts API                                        | ✅     |
| Weather.gov API /alerts                               | ✅     |
| Warning polygons (tornado, severe, flash flood, etc.) | ✅     |

## Observational / Surface Data

|                Data Source                 | Status |
| ------------------------------------------ | ------ |
| METAR/ASOS (surface obs)                   | ❌     |
| Upper-air soundings (RAOB)                 | ❌     |
| Mesonets (state/regional surface networks) | ❌     |
| Buoy / Marine obs                          | ❌     |
| ASOS 1-min data                            | ❌     |
| Storm spotter reports (mPING)              | ❌     |
| State traffic/weather cameras              | ❌     |

## Satellite Imagery

|         Data Source         | Status |
| --------------------------- | ------ |
| GOES-16/18 (GOES-East/West) | ❌     |
| Himawari (Pacific)          | ❌     |
| Polar-orbiting (JPSS/VIIRS) | ❌     |

## Lightning Data

|             Data Source              | Status |
| ------------------------------------ | ------ |
| GLM (Geostationary Lightning Mapper) | ❌     |
| Blitzortung                          | ❌     |
| ENTLN / Vaisala / Allison House?     | ❌     |

## Climate & Historical Data

|        Data Source        | Status |
| ------------------------- | ------ |
| Historical storm reports  | ❌     |
| Climate normals & records | ❌     |
| Historical radar archives | ✅     |
| Reanalysis (ERA5, NARR)   | ❌     |

## Geographic / Base Layer Data

|          Data Source          | Status |
| ----------------------------- | ------ |
| County/state/CWA boundaries   | ❌     |
| Roads / terrain / topo        | ❌     |
| Elevation / DEM               | ❌     |
| Land use / population density | ❌     |
