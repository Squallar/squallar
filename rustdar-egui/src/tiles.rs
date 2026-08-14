use crate::tile_source::HttpsTiles;
use walkers::{
    TileId,
    sources::{Attribution, TileSource},
};

/// CartoDB tile source variants.
/// Base maps use `nolabels` so city/road names are not obscured by the radar
/// overlay. A separate `labels-only` layer is drawn on top of the radar.
#[derive(Clone)]
pub enum CartoDbStyle {
    LightNoLabels,
    DarkNoLabels,
    LightLabelsOnly,
    DarkLabelsOnly,
}

#[derive(Clone)]
pub struct CartoDb {
    style: CartoDbStyle,
}

impl CartoDb {
    pub fn light() -> Self {
        Self {
            style: CartoDbStyle::LightNoLabels,
        }
    }

    pub fn dark() -> Self {
        Self {
            style: CartoDbStyle::DarkNoLabels,
        }
    }

    pub fn light_labels() -> Self {
        Self {
            style: CartoDbStyle::LightLabelsOnly,
        }
    }

    pub fn dark_labels() -> Self {
        Self {
            style: CartoDbStyle::DarkLabelsOnly,
        }
    }
}

impl TileSource for CartoDb {
    fn tile_url(&self, tile_id: TileId) -> String {
        let style_name = match self.style {
            CartoDbStyle::LightNoLabels => "light_nolabels",
            CartoDbStyle::DarkNoLabels => "dark_nolabels",
            CartoDbStyle::LightLabelsOnly => "light_only_labels",
            CartoDbStyle::DarkLabelsOnly => "dark_only_labels",
        };

        // Use one of the available subdomains (a, b, c, d)
        let subdomain = match tile_id.x % 4 {
            0 => "a",
            1 => "b",
            2 => "c",
            _ => "d",
        };

        format!(
            "https://cartodb-basemaps-{}.global.ssl.fastly.net/{}/{}/{}/{}.png",
            subdomain, style_name, tile_id.zoom, tile_id.x, tile_id.y
        )
    }

    fn attribution(&self) -> Attribution {
        Attribution {
            text: "© OpenStreetMap © CartoDB",
            url: "https://www.openstreetmap.org/copyright",
            logo_light: None,
            logo_dark: None,
        }
    }
}

// Slippy-map tile coordinate helpers (standard OSM / Web Mercator formulas)

/// The latitude Web Mercator ends at — [`rustdar_radar::types`]' constant, not
/// a second copy of it.
///
/// Re-exported under this path because that is where it was defined when the
/// tile helpers, `overlay_cache` and `rustdar-overlays`'s rasterizer were each
/// given their own spelling of it; it now lives one crate down, which is the
/// lowest point all three can reach. The constant's own doc carries the
/// projection's reasoning and what the truncated copies cost.
///
/// Not applied as a clamp by [`lat_to_tile_y`], which needs no branch: the
/// index clamp below already carries every latitude past this to the edge row.
pub use rustdar_radar::types::MERCATOR_LAT_LIMIT_DEG;

/// Carry a fractional tile coordinate to an index on `0..2^zoom`.
///
/// **Both ends, which is the whole reason this is a function.** These helpers
/// clamped only at zero, so every input past the far edge — a longitude at or
/// east of +180, a latitude at or south of [`MERCATOR_LAT_LIMIT_DEG`] — handed
/// the caller an index of `2^zoom` or more, for a grid whose last tile is
/// `2^zoom − 1`. `mercantile`, the reference implementation this is checked
/// against, clamps at both ends (`mercantile/__init__.py::tile`), and
/// `tests::no_input_produces_an_index_off_the_grid` now holds us to that.
///
/// The saturating `as` conversion is load-bearing on the way in as well as the
/// way out. `lat_to_tile_y` fed −90° through the old `ln(tan φ + sec φ)` and
/// got `u32::MAX`; `ui_map_overlays::draw_tile_layer` then computes
/// `lat_to_tile_y(min_lat) + 1`, which is an **overflow panic in a debug
/// build**. Nothing reaches −90° from `walkers::Projector::unproject` — it
/// would take a pane ~113 world-heights tall — so this was latent rather than
/// live, but it was one arithmetic hop from a caller that already exists.
#[inline]
fn tile_index(coord: f64, zoom: u8) -> u32 {
    // NaN floors to NaN and `NaN as u32` is 0, which is the low edge — the same
    // place an unrepresentable coordinate would land if it had a sign.
    let last = 2u32.saturating_pow(u32::from(zoom)).saturating_sub(1);
    (coord.floor().max(0.0) as u32).min(last)
}

/// Convert longitude to tile X index at the given zoom level.
///
/// Clamped to the grid at both ends — see [`tile_index`]. Longitudes outside
/// ±180 are **clamped, not wrapped**: a viewport straddling the antimeridian
/// gets the tiles on its own side of the seam and nothing for the far side.
/// See `ui_map_overlays::draw_tile_layer` for what that costs.
pub fn lon_to_tile_x(lon: f64, zoom: u8) -> u32 {
    let n = 2f64.powi(zoom as i32);
    tile_index((lon + 180.0) / 360.0 * n, zoom)
}

/// Convert latitude to tile Y index at the given zoom level.
///
/// # `asinh(tan φ)`, not `ln(tan φ + sec φ)`
///
/// The same function — `asinh t ≡ ln(t + √(t²+1))` and `√(tan²φ + 1)` is
/// `sec φ` on this domain — and the identity is exact, so this is not a change
/// of convention. It is a change of *spelling*, and the spelling matters
/// because the sum cancels: south of the equator `tan φ` is negative and
/// `sec φ` is positive, and near the pole the two are the same enormous number
/// with opposite signs. Measured, against `asinh(tan φ)` evaluated in the same
/// `f64`:
///
/// | latitude | old form's error |
/// |---|---|
/// | anywhere north | 0 ulp, at every latitude tested |
/// | −89.99° | 0.065 px at zoom 18 |
/// | −89.999° | 1.15 px at zoom 18 |
/// | −89.9999° | 188 px at zoom 18 |
/// | −89.99999° | 80 279 px at zoom 18 |
/// | −90° | no digits left — `u32::MAX` here, `NaN` in CPython's libm |
///
/// The asymmetry is why this was never going to show up in use: the northern
/// hemisphere, where this application's radars are, is exact in both forms.
///
/// Both independent implementations checked against use a stable form —
/// `walkers-0.56.0/src/mercator.rs` writes `tan().asinh()`, `mercantile` writes
/// `log((1+sin φ)/(1−sin φ))/4` — and this now agrees with the first bit for
/// bit over the whole sweep.
pub fn lat_to_tile_y(lat: f64, zoom: u8) -> u32 {
    let n = 2f64.powi(zoom as i32);
    let y = lat.to_radians().tan().asinh();
    tile_index((1.0 - y / std::f64::consts::PI) / 2.0 * n, zoom)
}

/// Convert tile X index back to the western longitude of the tile.
pub fn tile_to_lon(x: u32, zoom: u8) -> f64 {
    let n = 2f64.powi(zoom as i32);
    x as f64 / n * 360.0 - 180.0
}

/// Convert tile Y index back to the northern latitude of the tile.
pub fn tile_to_lat(y: u32, zoom: u8) -> f64 {
    let n = 2f64.powi(zoom as i32);
    (std::f64::consts::PI * (1.0 - 2.0 * y as f64 / n))
        .sinh()
        .atan()
        .to_degrees()
}

// ---------------------------------------------------------------------------
// MapTileState — shared map tile management
// ---------------------------------------------------------------------------

/// Shared map tile state across all panes.
///
/// Keeps both light and dark tile caches alive so theme toggles don't discard
/// already-fetched tiles. Label-only tiles are lazily initialized when any
/// pane enables the city-labels layer.
pub struct MapTileState {
    pub tiles_light: Option<HttpsTiles>,
    pub tiles_dark: Option<HttpsTiles>,
    pub label_tiles_light: Option<HttpsTiles>,
    pub label_tiles_dark: Option<HttpsTiles>,
    pub current_theme_is_dark: bool,
}

impl Default for MapTileState {
    fn default() -> Self {
        Self {
            tiles_light: None,
            tiles_dark: None,
            label_tiles_light: None,
            label_tiles_dark: None,
            current_theme_is_dark: true,
        }
    }
}

impl MapTileState {
    /// Ensure the base-map tiles for the current theme are initialized.
    pub fn ensure_base_tiles(&mut self, is_dark: bool, ctx: &egui::Context) {
        self.current_theme_is_dark = is_dark;
        if is_dark {
            if self.tiles_dark.is_none() {
                self.tiles_dark = Some(HttpsTiles::new(CartoDb::dark(), ctx.to_owned()));
            }
        } else if self.tiles_light.is_none() {
            self.tiles_light = Some(HttpsTiles::new(CartoDb::light(), ctx.to_owned()));
        }
    }

    /// Ensure label-only tiles are initialized for the current theme.
    pub fn ensure_label_tiles(&mut self, is_dark: bool, ctx: &egui::Context) {
        if is_dark && self.label_tiles_dark.is_none() {
            self.label_tiles_dark = Some(HttpsTiles::new(CartoDb::dark_labels(), ctx.to_owned()));
        } else if !is_dark && self.label_tiles_light.is_none() {
            self.label_tiles_light = Some(HttpsTiles::new(CartoDb::light_labels(), ctx.to_owned()));
        }
    }

    /// Temporarily take the base tiles out of self for per-pane rendering.
    pub fn take_base_tiles(&mut self) -> Option<HttpsTiles> {
        if self.current_theme_is_dark {
            self.tiles_dark.take()
        } else {
            self.tiles_light.take()
        }
    }

    /// Restore the base tiles after per-pane rendering.
    pub fn restore_base_tiles(&mut self, tiles: Option<HttpsTiles>) {
        if self.current_theme_is_dark {
            self.tiles_dark = tiles;
        } else {
            self.tiles_light = tiles;
        }
    }

    /// Temporarily take the label tiles out of self for per-pane rendering.
    pub fn take_label_tiles(&mut self) -> Option<HttpsTiles> {
        if self.current_theme_is_dark {
            self.label_tiles_dark.take()
        } else {
            self.label_tiles_light.take()
        }
    }

    /// Restore label tiles after per-pane rendering.
    pub fn restore_label_tiles(&mut self, tiles: Option<HttpsTiles>) {
        if self.current_theme_is_dark {
            self.label_tiles_dark = tiles;
        } else {
            self.label_tiles_light = tiles;
        }
    }

    /// Clear all tile state (called on suspend/graphics reset).
    pub fn clear(&mut self) {
        self.tiles_light = None;
        self.tiles_dark = None;
        self.label_tiles_light = None;
        self.label_tiles_dark = None;
        // Flip theme tracking to force tile recreation on next render
        self.current_theme_is_dark = !self.current_theme_is_dark;
    }
}

/// The side of one slippy tile in **points**, at zoom bias 0.
///
/// 256 is the tile grid's own unit and is hard-coded inside `walkers`'
/// projector (`mercator.rs`'s `TILE_SIZE`), which is why a tile source cannot
/// change it without desynchronising the raster from `Projector::project`. See
/// `draw_tile_layer`.
pub const TILE_SIDE_POINTS: f32 = 256.0;

/// How many tiles a rect keeps resident, at a zoom bias, across `layers` raster
/// layers.
///
/// The `+ 1` on each axis is the tile the rect straddles at each edge: a pane
/// whose width is an exact multiple of the tile side still overlaps one more
/// column unless it happens to start on a tile boundary, and it never does.
///
/// # What it is for
///
/// `tile_source::TILE_CACHE_ENTRIES` is a single LRU shared by every pane and
/// every layer, and each zoom bias level costs four times the tiles. A bias
/// applied to a working set larger than the LRU does not merely fail to help —
/// it evicts the tiles still being drawn, on every frame, so the fetcher never
/// settles and the pane flickers between levels. So the bias is only taken when
/// this fits, which makes it a computed decision rather than an argument about a
/// hypothetical pane size.
pub fn tiles_resident_for(rect: egui::Rect, zoom_bias: u8, layers: usize) -> usize {
    let side = TILE_SIDE_POINTS / 2f32.powi(i32::from(zoom_bias));
    if side <= 0.0 || !rect.width().is_finite() || !rect.height().is_finite() {
        return 0;
    }
    let across = (rect.width().max(0.0) / side).ceil() as usize + 1;
    let down = (rect.height().max(0.0) / side).ceil() as usize + 1;
    across.saturating_mul(down).saturating_mul(layers)
}

#[path = "tiles/tests.rs"]
#[cfg(test)]
mod tests;
