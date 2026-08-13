# Rustdar Feature Matrix

Feature comparison across weather platforms.

| Mark    | Meaning                                                                      |
| ------- | ---------------------------------------------------------------------------- |
| ✅      | Implemented and reachable by a user                                          |
| 🚧      | Partly built — code exists, no way for a user to reach it yet                |
| Partial | Implemented, but narrower than the column it sits beside                     |
| ❌      | Not implemented                                                              |
| ❓      | Not verified — the competitor's own documentation was ambiguous or unchecked |

Rustdar's column is checked against this repository. Competitor columns are not,
and the paid tiers move: RadarScope's entries below reflect its **Pro** tiers
(Tier One adds real-time lightning and extended loops; Tier Two adds the
archive, MRMS, satellite and soundings), not the free app.

---

## 📡 Radar

| Feature                                       | GR2A    | RadarScope | Rustdar |
| --------------------------------------------- | ------- | ---------- | ------- |
| NEXRAD Level 2 (super-res)                    | ✅      | ✅         | ✅      |
| Level 2 real-time chunks (sub-volume latency) | ✅      | ❓         | ✅      |
| NEXRAD Level 3 products                       | ✅      | ✅         | ✅      |
| TDWR terminal radars                          | ❓      | ✅         | ❌      |
| Dual-pol moments (CC, ZDR, PHI, KDP)          | ✅      | ✅         | ✅      |
| Hydrometeor classification (HHC)              | ✅      | ✅         | ✅      |
| Echo tops (EET, and tilt-interpolated)        | ✅      | ✅         | ✅      |
| VIL and VIL density (VILD)                    | ✅      | ❓         | ✅      |
| Hail products (POSH, MEHS)                    | ✅      | ❓         | ✅      |
| Precipitation rate (DPR)                      | ✅      | ✅         | ✅      |
| Normalized rotation (NROT)                    | ✅      | ❌         | ✅      |
| Storm-relative velocity                       | ✅      | ✅         | ✅      |
| 3D volumetric rendering                       | ✅      | ❌         | Partial |
| Vertical cross-sections                       | ✅      | ❌         | ✅      |
| MRMS national mosaic                          | ❌      | ✅         | ❌      |
| VAD wind profiles                             | ✅      | ❓         | ❌      |
| Radar loop animation                          | ✅      | ✅         | ✅      |
| Multi-radar compositing                       | Partial | ❌         | ❌      |
| Archive radar playback                        | ✅      | ✅         | ✅      |
| Custom color tables                           | ✅      | ✅         | ❌      |

Notes on the Rustdar column:

- **Level 2 real-time chunks** — `rustdar-radar/src/chunks.rs` reads the
  `unidata-nexrad-level2-chunks` bucket, so the 0.5° tilt arrives seconds after
  the radar collects it rather than at end-of-volume. A WebSocket push feed
  (`rustdar-frontend/src/chunk_notify.rs`) drives the fetch.
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
  grid (`rustdar-radar/src/voxel.rs`) and raymarches it
  (`rustdar-frontend/src/volume.wgsl`, `volume_raymarch.rs`, wired by
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
  (`rustdar-egui/src/ui_section_pane.rs`). The volume sampler
  (`rustdar-radar/src/sampler.rs`) and the section rasterizer
  (`rustdar-radar/src/xsect.rs`) are behind it, and the cut is a job kind
  (`offload::JobRequest::Section`) so it is rasterized off the frame thread on
  both targets. Loops cut a section per frame.
- **VAD wind profiles** — a VAD fit *is* computed
  (`nrot::WindProfileBuilder`), but only as an input to storm-relative velocity
  and NROT. It is never displayed as a wind profile, so this is ❌ as a feature.
- **Radar loop animation** — frame ceiling is per device class: 60 desktop, 20
  mobile, 12 web (`rustdar-frontend/src/constants.rs`).
- **Archive radar playback** — `rustdar-radar/src/archive.rs`, over the full
  public NEXRAD Level 2 archive rather than a rolling window.

## 🌀 Model Data

| Feature                            | Pivotal | WeatherBell | Windy | Rustdar |
| ---------------------------------- | ------- | ----------- | ----- | ------- |
| HRRR                               | ✅      | ✅          | ✅    | Partial |
| NAM / NAM Nest                     | ✅      | ✅          | ❌    | ❌      |
| GFS                                | ✅      | ✅          | ✅    | ❌      |
| GEFS (ensemble)                    | ✅      | ✅          | ❌    | ❌      |
| ECMWF (open data)                  | ❌      | ✅          | ✅    | ❌      |
| RAP                                | ✅      | ✅          | ❌    | ❌      |
| HREF / SREF                        | ✅      | ✅          | ❌    | ❌      |
| NBM                                | ✅      | ✅          | ❌    | ❌      |
| Interactive (not just images)      | ❌      | ❌          | ✅    | ✅      |
| Ensemble spread / spaghetti        | ✅      | ✅          | ❌    | ❌      |
| Model comparison (side by side)    | Partial | ❌          | ✅    | Partial |
| Model soundings (virtual profiles) | ✅      | ❌          | ❌    | ❌      |
| Custom fields / derived params     | Partial | ❌          | ❌    | ❌      |
| Animated model loops               | ✅      | ❌          | ✅    | ❌      |

Rustdar's HRRR support is **Partial**: sixteen fields
(`rustdar-overlays/src/hrrr/mod.rs`, `ModelParameter::all()`) at the analysis
hour only — f00, or f01 for the two windowed updraft-helicity maxima. There is
no forecast-hour selector and so no model loop. The fields are SBCAPE, MLCAPE,
MUCAPE, SBCIN, MLCIN, lifted index, 0–1 km and 0–3 km SRH, 0–2 km and 2–5 km
max UH, 0–6 km bulk shear, surface gust, PWAT, 2 m temperature, 2 m dewpoint and
visibility. "Interactive" and "side by side" are ✅/Partial because the grids are
decoded and rasterized locally into panes rather than fetched as images, and
panes can show different parameters at once.

## ⚠️ SPC & Severe Weather

| Feature                           | GR2A    | RadarScope | Pivotal | Rustdar |
| --------------------------------- | ------- | ---------- | ------- | ------- |
| Convective outlooks (Day 1–8)     | Overlay | ❓         | ✅      | ✅      |
| Mesoscale discussions             | Overlay | ❓         | ❌      | ✅      |
| Watch/Warning polygons            | ✅      | ✅         | ❌      | ✅      |
| Tornado/Severe/FFW polygons       | ✅      | ✅         | ❌      | ✅      |
| Storm reports (prelim & filtered) | Overlay | ✅         | ✅      | ✅      |
| SPC mesoanalysis parameters       | ❌      | ❌         | ✅      | ❌      |
| Significant hail/tornado probs    | ❌      | ❌         | ✅      | ✅      |
| Fire weather outlooks             | ❌      | ❌         | ❌      | ❌      |

Rustdar's watch and warning geometry comes from the NWS Alerts API, which is
county/zone-shaped; the SPC watch parallelograms themselves are not fetched.

## 📊 Analysis Tools

| Feature                       | GR2A | RadarScope | Pivotal | SHARPpy | Rustdar |
| ----------------------------- | ---- | ---------- | ------- | ------- | ------- |
| Skew-T / Log-P diagrams       | ❌   | ✅         | ✅      | ✅      | ❌      |
| Hodographs                    | ❌   | ❓         | Partial | ✅      | ❌      |
| Supercell composite / STP     | ❌   | ❌         | ✅      | ✅      | ❌      |
| CAPE/CIN/Shear calculators    | ❌   | ❌         | ✅      | ✅      | ❌      |
| Cross-section (model)         | ❌   | ❌         | ✅      | ❌      | ❌      |
| Time-height cross-sections    | ❌   | ❌         | ❌      | ❌      | ❌      |
| Point forecast meteograms     | ❌   | ❌         | ❌      | ❌      | ❌      |
| Multi-model comparison panels | ❌   | ❌         | Partial | ❌      | ❌      |

RadarScope's Pro Tier Two forecast soundings are what puts a ✅ on the Skew-T
row. Rustdar reads environmental heights from Open-Meteo
(`rustdar-radar/src/sounding.rs`) but only the 0 °C and −20 °C levels, only to
scale the hail products, and never draws them — that is not a sounding tool.

## 🛰️ Satellite

| Feature                   | Windy | COD | RadarScope | Rustdar |
| ------------------------- | ----- | --- | ---------- | ------- |
| GOES-19/18 visible        | ✅    | ✅  | ✅         | ❌      |
| GOES IR / Water vapor     | ✅    | ✅  | ✅         | ❌      |
| GOES GLM (lightning)      | ❌    | ❌  | ❌         | ✅      |
| Mesoscale sectors         | ❌    | ✅  | ❓         | ❌      |
| Sandwich product (Vis+IR) | ❌    | ❌  | ❌         | ❌      |
| Day/Night band            | ❌    | ❌  | ❌         | ❌      |
| Animated loops            | ✅    | ✅  | ✅         | ❌      |
| Multi-satellite (global)  | ✅    | ❌  | ❌         | ❌      |

Rustdar reads the `noaa-goes19` and `noaa-goes18` buckets, but only for GLM
lightning — no imagery product is decoded. GOES-19 replaced GOES-16 in the
GOES-East slot in April 2025, which is why the row is no longer "GOES-16/18".

## ⚡ Lightning

| Feature                            | RadarScope | Baron | Rustdar |
| ---------------------------------- | ---------- | ----- | ------- |
| Real-time strikes (ground network) | ✅ (Pro)   | ✅    | ❌      |
| GLM (satellite-based)              | ❌         | ✅    | ✅      |
| Blitzortung (community)            | ❌         | ❌    | ❌      |
| Lightning density / history        | ❌         | ✅    | ❌      |

## 📍 Surface Observations

| Feature                        | GR2A    | RadarScope | Rustdar |
| ------------------------------ | ------- | ---------- | ------- |
| METAR/ASOS station plots       | Overlay | ❌         | ✅      |
| Personal weather stations      | ❌      | ❌         | ❌      |
| Mesonet data (OK, etc.)        | Overlay | ❌         | ❌      |
| Buoy / marine obs              | ❌      | ❌         | ❌      |
| mPING spotter reports          | ❌      | ❌         | ❌      |
| Station model plots (standard) | ❌      | ❌         | Partial |

Rustdar draws a station model (`rustdar-overlays/src/render/station_model.rs`)
from the Iowa Environmental Mesonet's METAR feed. It is a reduced plot, not the
full WMO station model, hence Partial.

## 🔔 Alerting & Notifications

| Feature                             | Baron | DTN | RadarScope | Rustdar |
| ----------------------------------- | ----- | --- | ---------- | ------- |
| Custom geo-fenced alerts            | ✅    | ✅  | Partial    | ❌      |
| Threshold-based (wind >60mph, etc.) | ✅    | ✅  | ❌         | ❌      |
| Push notifications                  | ✅    | ✅  | ✅         | ❌      |
| Multi-hazard dashboard              | ✅    | ✅  | ❌         | ❌      |
| Email/SMS/webhook alerts            | ✅    | ✅  | ❌         | ❌      |

Rustdar has a push feed, but it notifies the *app* that a new Level 2 chunk
exists so the fetch can start immediately. Nothing notifies the user of weather.

## 💻 Platform & UX

| Feature                     | GR2A       | RadarScope   | Windy | Rustdar            |
| --------------------------- | ---------- | ------------ | ----- | ------------------ |
| Desktop app                 | ✅ (Win)   | ✅ (Win/Mac) | ❌    | ✅ (Linux/Mac/Win) |
| Web app                     | ❌         | ❌           | ✅    | ✅                 |
| Mobile (iOS/Android)        | ❌         | ✅           | ✅    | Partial            |
| Multi-pane / dual-pane      | ✅         | ✅ (Pro)     | ❌    | ✅                 |
| Offline capability          | ✅         | ❌           | ❌    | ❌                 |
| Dark mode / themes          | ❌         | ❓           | ✅    | Partial            |
| Customizable layer stack    | ✅         | Partial      | ✅    | ✅                 |
| Plugin / extension system   | Placefiles | ❌           | ❌    | ❌                 |
| Open API for developers     | ❌         | ❌           | Paid  | ❌                 |
| GR2Analyst placefile compat | ✅         | ❌           | ❌    | ❌                 |

- **Mobile** is Partial: Android ships (`rustdar-android`), and iOS builds an
  `.ipa` in CI (`ios/Makefile`, the `ios-aarch64` row of `build.yaml`), but it
  is unsigned and undistributed.
- **Multi-pane** is up to six panes on desktop and four on mobile
  (`MAX_PANES_DESKTOP` / `MAX_PANES_MOBILE` in `rustdar-egui/src/pane.rs`).
- **Dark mode** follows the OS and only the OS — `dark_light::detect` on
  desktop, a JNI read of `Configuration.uiMode` on Android — with light and dark
  basemap label tiles behind it. There is no in-app override and no theme
  beyond the two, hence Partial.
- **Offline capability** is ❌ despite the PWA service worker: `sw.js` caches the
  application shell and basemap tiles, so the app launches offline and shows a
  map, but it caches no weather data at all — by design, and enforced by a
  never-cache host list.
