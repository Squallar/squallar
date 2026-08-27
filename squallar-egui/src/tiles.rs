use crate::tile_source::HttpsTiles;
use walkers::{
    TileId,
    sources::{Attribution, TileSource},
};

/// The basemap credit, drawn once per map panel.
///
/// A const rather than only a [`TileSource::attribution`] field because the
/// painter needs it and that trait hands back `&'static str`: routing the
/// drawn string through the trait would mean leaking one or holding a `Lazy`.
/// The trait impl below reads these, so the two cannot drift.
///
/// `\u{a9}` is U+00A9, registered in `ui_glyphs` as the basemap attribution
/// copyright -- never spelled `(c)`.
pub const ATTRIBUTION_TEXT: &str = "\u{a9} OpenStreetMap \u{a9} CartoDB";

/// Where the credit links. ODbL wants the notice reachable, not just shown.
pub const ATTRIBUTION_URL: &str = "https://www.openstreetmap.org/copyright";

/// CartoDB tile source variants.
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
            text: ATTRIBUTION_TEXT,
            url: ATTRIBUTION_URL,
            logo_light: None,
            logo_dark: None,
        }
    }
}

// Slippy-map tile coordinates (standard OSM / Web Mercator formulas), from
// the workspace's geodesy floor.

// The tile transforms and `MERCATOR_LAT_LIMIT_DEG` live in `squallar-geo`.
// The mercantile reference vectors in `tiles/tests.rs` pin their exact bits.

// ---------------------------------------------------------------------------

/// Shared map tile state across all panes.
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
        self.current_theme_is_dark = !self.current_theme_is_dark;
    }
}

/// The side of one slippy tile in **points**, at zoom bias 0.
pub const TILE_SIDE_POINTS: f32 = 256.0;

/// How many tiles a rect keeps resident, at a zoom bias, across `layers` raster
/// layers.
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
