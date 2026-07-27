# Rustdar Feature Matrix

Feature comparison across weather platforms. ✅ = implemented, ❌ = not implemented.

---

## 📡 Radar

|           Feature            |  GR2A   | RadarScope | Rustdar |
| ---------------------------- | ------- | ---------- | ------- |
| NEXRAD Level 2 (super-res)   | ✅      | ✅         | ✅      |
| NEXRAD Level 3 products      | ✅      | ✅         | ✅      |
| Dual-pol (CC, ZDR, KDP, HCA) | ✅      | ✅         | ✅      |
| 3D volumetric rendering      | ✅      | ❌         | ❌      |
| Vertical cross-sections      | ✅      | ❌         | ❌      |
| MRMS national mosaic         | ❌      | ❌         | ❌      |
| Storm-relative velocity      | ✅      | ✅         | ✅      |
| VAD wind profiles            | ✅      | ❌         | ❌      |
| Radar loop animation         | ✅      | ✅         | ✅      |
| Multi-radar compositing      | Partial | ❌         | ❌      |
| Archive radar playback       | ✅      | ❌         | ✅      |
| Custom color tables          | ✅      | ❌         | ❌      |

## 🌀 Model Data

|              Feature               | Pivotal | WeatherBell | Windy | Rustdar |
| ---------------------------------- | ------- | ----------- | ----- | ------- |
| HRRR                               | ✅      | ✅          | ✅    | ✅      |
| NAM / NAM Nest                     | ✅      | ✅          | ❌    | ❌      |
| GFS                                | ✅      | ✅          | ✅    | ❌      |
| GEFS (ensemble)                    | ✅      | ✅          | ❌    | ❌      |
| ECMWF (open data)                  | ❌      | ✅          | ✅    | ❌      |
| RAP                                | ✅      | ✅          | ❌    | ❌      |
| HREF / SREF                        | ✅      | ✅          | ❌    | ❌      |
| NBM                                | ✅      | ✅          | ❌    | ❌      |
| Interactive (not just images)      | ❌      | ❌          | ✅    | ❌      |
| Ensemble spread / spaghetti        | ✅      | ✅          | ❌    | ❌      |
| Model comparison (side by side)    | Partial | ❌          | ✅    | ❌      |
| Model soundings (virtual profiles) | ✅      | ❌          | ❌    | ❌      |
| Custom fields / derived params     | Partial | ❌          | ❌    | ❌      |
| Animated model loops               | ✅      | ❌          | ✅    | ❌      |

## ⚠️ SPC & Severe Weather

|              Feature              |  GR2A   | RadarScope | Pivotal | Rustdar |
| --------------------------------- | ------- | ---------- | ------- | ------- |
| Convective outlooks (Day 1–8)     | Overlay | ❌         | ✅      | ✅      |
| Mesoscale discussions             | Overlay | ❌         | ❌      | ✅      |
| Watch/Warning polygons            | ✅      | ✅         | ❌      | ✅      |
| Tornado/Severe/FFW polygons       | ✅      | ✅         | ❌      | ✅      |
| Storm reports (prelim & filtered) | Overlay | ❌         | ✅      | ✅      |
| SPC mesoanalysis parameters       | ❌      | ❌         | ✅      | ❌      |
| Significant hail/tornado probs    | ❌      | ❌         | ✅      | ✅      |
| Fire weather outlooks             | ❌      | ❌         | ❌      | ❌      |

## 📊 Analysis Tools

|            Feature            | GR2A | Pivotal | SHARPpy | Rustdar |
| ----------------------------- | ---- | ------- | ------- | ------- |
| Skew-T / Log-P diagrams       | ❌   | ✅      | ✅      | ❌      |
| Hodographs                    | ❌   | Partial | ✅      | ❌      |
| Supercell composite / STP     | ❌   | ✅      | ✅      | ❌      |
| CAPE/CIN/Shear calculators    | ❌   | ✅      | ✅      | ❌      |
| Cross-section (model)         | ❌   | ✅      | ❌      | ❌      |
| Time-height cross-sections    | ❌   | ❌      | ❌      | ❌      |
| Point forecast meteograms     | ❌   | ❌      | ❌      | ❌      |
| Multi-model comparison panels | ❌   | Partial | ❌      | ❌      |

## 🛰️ Satellite

|          Feature          | Windy | COD | Rustdar |
| ------------------------- | ----- | --- | ------- |
| GOES-16/18 visible        | ✅    | ✅  | ❌      |
| GOES IR / Water vapor     | ✅    | ✅  | ❌      |
| GOES GLM (lightning)      | ❌    | ❌  | ✅      |
| Mesoscale sectors         | ❌    | ✅  | ❌      |
| Sandwich product (Vis+IR) | ❌    | ❌  | ❌      |
| Day/Night band            | ❌    | ❌  | ❌      |
| Animated loops            | ✅    | ✅  | ❌      |
| Multi-satellite (global)  | ✅    | ❌  | ❌      |

## ⚡ Lightning

|           Feature           | RadarScope | Baron | Rustdar |
| --------------------------- | ---------- | ----- | ------- |
| Real-time strikes           | ✅ (paid)  | ✅    | ❌      |
| GLM (satellite-based)       | ❌         | ✅    | ✅      |
| Blitzortung (community)     | ❌         | ❌    | ❌      |
| Lightning density / history | ❌         | ✅    | ❌      |

## 📍 Surface Observations

|            Feature             |  GR2A   | RadarScope | Rustdar |
| ------------------------------ | ------- | ---------- | ------- |
| METAR/ASOS station plots       | Overlay | ❌         | ✅      |
| Personal weather stations      | ❌      | ❌         | ❌      |
| Mesonet data (OK, etc.)        | Overlay | ❌         | ❌      |
| Buoy / marine obs              | ❌      | ❌         | ❌      |
| mPING spotter reports          | ❌      | ❌         | ❌      |
| Station model plots (standard) | ❌      | ❌         | ❌      |

## 🔔 Alerting & Notifications

|               Feature               | Baron | DTN | RadarScope | Rustdar |
| ----------------------------------- | ----- | --- | ---------- | ------- |
| Custom geo-fenced alerts            | ✅    | ✅  | Partial    | ❌      |
| Threshold-based (wind >60mph, etc.) | ✅    | ✅  | ❌         | ❌      |
| Push notifications                  | ✅    | ✅  | ✅         | ❌      |
| Multi-hazard dashboard              | ✅    | ✅  | ❌         | ❌      |
| Email/SMS/webhook alerts            | ✅    | ✅  | ❌         | ❌      |

## 💻 Platform & UX

|           Feature           |    GR2A    | RadarScope | Windy |       Rustdar      |
| --------------------------- | ---------- | ---------- | ----- | ------------------ |
| Desktop app                 | ✅ (Win)   | ✅         | ❌    | ✅ (Linux/Mac/Win) |
| Web app                     | ❌         | ❌         | ✅    | ✅                 |
| Mobile (iOS/Android)        | ❌         | ✅         | ✅    | Android only       |
| Offline capability          | ✅         | ❌         | ❌    | ❌                 |
| Dark mode / themes          | ❌         | ❌         | ✅    | ✅                 |
| Customizable layer stack    | ✅         | Partial    | ✅    | ✅                 |
| Plugin / extension system   | Placefiles | ❌         | ❌    | ❌                 |
| Open API for developers     | ❌         | ❌         | Paid  | ❌                 |
| GR2Analyst placefile compat | ✅         | ❌         | ❌    | ❌                 |
