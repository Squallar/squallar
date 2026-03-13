use walkers::{
    sources::{Attribution, TileSource},
    TileId,
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
        Self { style: CartoDbStyle::LightNoLabels }
    }

    pub fn dark() -> Self {
        Self { style: CartoDbStyle::DarkNoLabels }
    }

    pub fn light_labels() -> Self {
        Self { style: CartoDbStyle::LightLabelsOnly }
    }

    pub fn dark_labels() -> Self {
        Self { style: CartoDbStyle::DarkLabelsOnly }
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

/// Convert longitude to tile X index at the given zoom level.
pub fn lon_to_tile_x(lon: f64, zoom: u8) -> u32 {
    let n = 2f64.powi(zoom as i32);
    ((lon + 180.0) / 360.0 * n).floor().max(0.0) as u32
}

/// Convert latitude to tile Y index at the given zoom level.
pub fn lat_to_tile_y(lat: f64, zoom: u8) -> u32 {
    let n = 2f64.powi(zoom as i32);
    let lat_rad = lat.to_radians();
    ((1.0 - (lat_rad.tan() + 1.0 / lat_rad.cos()).ln() / std::f64::consts::PI) / 2.0 * n)
        .floor()
        .max(0.0) as u32
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
