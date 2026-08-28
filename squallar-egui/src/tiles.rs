use crate::tile_source::HttpsTiles;
use squallar_geo::{lat_to_tile_y, lon_to_tile_x};
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
// Which source the base map is drawn from
// ---------------------------------------------------------------------------

/// The self-hosted PMTiles v3 basemap archive.
///
/// The generation is compiled in. `https://tiles.squallar.app/status/latest.json`
/// is the stable pointer at the same host and is what a client should resolve
/// once it can afford an extra round trip before the first tile; nothing does
/// yet, and a pointer nobody reads would be a claim rather than a mechanism.
#[cfg(all(feature = "basemap-vector", not(target_arch = "wasm32")))]
pub const BASEMAP_ARCHIVE_URL: &str = "https://tiles.squallar.app/basemap/omt-20260828.pmtiles";

/// An archive URL that replaces [`BASEMAP_ARCHIVE_URL`] when it is set.
///
/// Native only, and read once per source construction. It exists because the
/// published archive is the *only* other way to exercise this path, and a
/// change to the draw seam that can only be checked against 83 GB over a
/// network is a change that will stop being checked. `file://` is not a scheme
/// [`crate::basemap_archive::HttpRangeSource`] serves, so a local archive is
/// pointed at through a plain HTTP file server.
#[cfg(all(feature = "basemap-vector", not(target_arch = "wasm32")))]
pub const BASEMAP_ARCHIVE_URL_ENV: &str = "SQUALLAR_BASEMAP_ARCHIVE";

/// The credit the vector basemap carries.
///
/// [`ATTRIBUTION_TEXT`] is still CartoDB's and is what the map panel *draws*;
/// the two disagree while `basemap-vector` is off by default, and reconciling
/// them is the flip, which deletes `CartoDb` outright. This is the honest
/// string for the source that is actually being read.
#[cfg(all(feature = "basemap-vector", not(target_arch = "wasm32")))]
pub const ARCHIVE_ATTRIBUTION_TEXT: &str = "\u{a9} OpenStreetMap contributors \u{a9} OpenMapTiles";

/// The base-map tile source for `is_dark`: CartoDB's pre-rendered rasters.
#[cfg(not(all(feature = "basemap-vector", not(target_arch = "wasm32"))))]
fn base_source(is_dark: bool, ctx: &egui::Context) -> HttpsTiles {
    let source = if is_dark {
        CartoDb::dark()
    } else {
        CartoDb::light()
    };
    HttpsTiles::new(source, ctx.to_owned())
}

/// The base-map tile source for `is_dark`: our own archive, rendered here.
///
/// Falls back to CartoDB if the archive URL will not parse, which is the only
/// failure visible at construction. Everything else — the host being down, the
/// archive not being PMTiles — happens inside the IO task and is logged there;
/// there is nothing to fall back *to* by then, because the frame has already
/// been handed a source.
#[cfg(all(feature = "basemap-vector", not(target_arch = "wasm32")))]
fn base_source(is_dark: bool, ctx: &egui::Context) -> HttpsTiles {
    let url =
        std::env::var(BASEMAP_ARCHIVE_URL_ENV).unwrap_or_else(|_| BASEMAP_ARCHIVE_URL.to_owned());

    let attribution = Attribution {
        text: ARCHIVE_ATTRIBUTION_TEXT,
        url: ATTRIBUTION_URL,
        logo_light: None,
        logo_dark: None,
    };

    match HttpsTiles::from_archive_url(
        &url,
        crate::basemap_style::committed(is_dark),
        attribution,
        ctx.to_owned(),
    ) {
        Ok(tiles) => tiles,
        Err(error) => {
            log::error!("{url} is not a usable basemap archive URL: {error}");
            let source = if is_dark {
                CartoDb::dark()
            } else {
                CartoDb::light()
            };
            HttpsTiles::new(source, ctx.to_owned())
        }
    }
}

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
    /// Adopt `is_dark` as the theme, releasing the other one's tile caches.
    ///
    /// Both `ensure_` methods open with this, so the release does not depend on
    /// which of them happens to see the flip first — city labels can be off in
    /// the very frame the theme changes, and the old theme's label cache has to
    /// go anyway.
    ///
    /// **Nothing on the glass is blanked.** The tiles dropped belong to the
    /// theme that is no longer drawn; the pane is repainting from the other
    /// theme's sources this frame regardless. The accepted cost is a re-download
    /// if the user flips back.
    ///
    /// Without it a single flip took residency from two live sources to four and
    /// held it for the session, because the `ensure_` methods only ever fill and
    /// [`Self::clear`] runs only on suspend and graphics reset. At
    /// [`crate::tile_source::WASM_TILE_CACHE_ENTRIES`] — 96 since `6345952f` —
    /// and 256 KiB a tile, one source's worst case is 24 MiB, so four of them is
    /// 96 MiB against the 288 MiB `squallar-device-profile` allows the whole
    /// application on wasm32.
    fn adopt_theme(&mut self, is_dark: bool) {
        if is_dark == self.current_theme_is_dark {
            return;
        }
        self.current_theme_is_dark = is_dark;

        // The theme that just stopped being drawn. Base and labels together:
        // they are the same theme's tiles and nothing draws either of them now.
        if is_dark {
            self.tiles_light = None;
            self.label_tiles_light = None;
        } else {
            self.tiles_dark = None;
            self.label_tiles_dark = None;
        }
    }

    /// Ensure the base-map tiles for the current theme are initialized.
    pub fn ensure_base_tiles(&mut self, is_dark: bool, ctx: &egui::Context) {
        self.adopt_theme(is_dark);
        if is_dark {
            if self.tiles_dark.is_none() {
                self.tiles_dark = Some(base_source(true, ctx));
            }
        } else if self.tiles_light.is_none() {
            self.tiles_light = Some(base_source(false, ctx));
        }
    }

    /// Ensure label-only tiles are initialized for the current theme.
    pub fn ensure_label_tiles(&mut self, is_dark: bool, ctx: &egui::Context) {
        self.adopt_theme(is_dark);
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

/// The side of one slippy tile in **points**, at zoom bias 0 and a whole zoom.
pub const TILE_SIDE_POINTS: f32 = 256.0;

/// The smallest a tile is ever drawn, as a fraction of [`TILE_SIDE_POINTS`].
///
/// walkers paints a tile `TILE_SIDE_POINTS · 2^(zoom − tile_zoom)` points
/// across, and `ui_map_overlays::draw_tile_layer` picks
/// `tile_zoom = zoom.round() + bias`, so the exponent is
/// `zoom − round(zoom) − bias`. `zoom − round(zoom)` runs over `[−0.5, 0.5)`
/// and **attains** `−0.5`, because Rust rounds a half away from zero: a tile is
/// at its smallest exactly at the half step, `2^−0.5` of a side — 181.02 points
/// at bias 0 — and more of them fit the same window.
pub const MIN_TILE_SCALE: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// How many tiles a rect keeps resident, at a zoom bias, across `layers` raster
/// layers — **worst case over the whole zoom range**.
///
/// This is the sizing answer, and the only one a cache may be built on: the LRU
/// has to hold what the viewport asks for at every zoom the user can be at, not
/// only at the whole ones. Between two whole steps a tile shrinks to
/// [`MIN_TILE_SCALE`] of its side and more of them fit, so this is the larger
/// figure — 84 against 54 for a 1920x1080-point canvas at bias 0, one layer.
///
/// [`tiles_resident_at_whole_zoom`] answers the narrower question, and is only
/// ever the right one for a claim that is *about* a whole zoom.
pub fn tiles_resident_for(rect: egui::Rect, zoom_bias: u8, layers: usize) -> usize {
    tiles_resident_at_tile_scale(rect, zoom_bias, layers, MIN_TILE_SCALE)
}

/// How many tiles a rect keeps resident at a **whole** zoom, where a tile is
/// drawn exactly [`TILE_SIDE_POINTS`] across. Never larger than
/// [`tiles_resident_for`], and not a cache size.
pub fn tiles_resident_at_whole_zoom(rect: egui::Rect, zoom_bias: u8, layers: usize) -> usize {
    tiles_resident_at_tile_scale(rect, zoom_bias, layers, 1.0)
}

/// The count for a tile drawn `scale · TILE_SIDE_POINTS / 2^zoom_bias` points
/// across.
///
/// `ceil(w / side) + 1`, not `ceil` alone: the grid's phase inside the viewport
/// is free, so a window `w / side` tiles wide reaches one column further than
/// its own width. That is tight rather than generous — as the phase approaches
/// a tile boundary a window of `w / side = 7.5` covers nine columns, and
/// `ceil(7.5) + 1` is nine. `tiles/tests.rs` holds both 54 and 84 against the
/// sweep that measures `tile_span` directly rather than deriving it.
fn tiles_resident_at_tile_scale(
    rect: egui::Rect,
    zoom_bias: u8,
    layers: usize,
    scale: f32,
) -> usize {
    let side = TILE_SIDE_POINTS * scale / 2f32.powi(i32::from(zoom_bias));
    if side <= 0.0 || !rect.width().is_finite() || !rect.height().is_finite() {
        return 0;
    }
    let across = (rect.width().max(0.0) / side).ceil() as usize + 1;
    let down = (rect.height().max(0.0) / side).ceil() as usize + 1;
    across.saturating_mul(down).saturating_mul(layers)
}

/// The tile indices one viewport covers at one tile zoom, both ends inclusive.
///
/// `north` is the *smaller* row index: tile `y` grows southward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileSpan {
    pub west: u32,
    pub east: u32,
    pub north: u32,
    pub south: u32,
}

impl TileSpan {
    /// How many tiles the span names.
    pub fn tiles(self) -> usize {
        let across = (self.east.saturating_sub(self.west) as usize) + 1;
        let down = (self.south.saturating_sub(self.north) as usize) + 1;
        across.saturating_mul(down)
    }
}

/// The tiles that cover `rect` on the glass at `tile_zoom`.
///
/// **Neither end is widened, and widening one would be a bug.**
/// [`lon_to_tile_x`] and [`lat_to_tile_y`] *floor*: the index each returns is
/// the tile the coordinate falls inside, so the inclusive span already carries
/// the two part-covered edge tiles. A `+ 1` on the far end appends a column
/// wholly east of `rect` and a row wholly south of it — at 1920x1080 that is
/// 10x7 tiles where 9x6 are seen — and each of the extra 16 costs a
/// `Tiles::at` call, an HTTP request through `request_once`, an LRU probe, a
/// `TextureHandle` clone and drop (a write lock on the texture manager each),
/// and a fully clipped `Painter::image`.
///
/// Both transforms also clamp rather than wrap, so a viewport reaching past the
/// antimeridian or the Mercator limit gets the edge tile and not the far side.
pub fn tile_span(projector: &walkers::Projector, rect: egui::Rect, tile_zoom: u8) -> TileSpan {
    let nw = projector.unproject(egui::vec2(rect.left(), rect.top()));
    let se = projector.unproject(egui::vec2(rect.right(), rect.bottom()));

    // walkers Position: x = longitude, y = latitude.
    let (min_lon, max_lon) = (nw.x().min(se.x()), nw.x().max(se.x()));
    let (min_lat, max_lat) = (nw.y().min(se.y()), nw.y().max(se.y()));

    TileSpan {
        west: lon_to_tile_x(min_lon, tile_zoom),
        east: lon_to_tile_x(max_lon, tile_zoom),
        north: lat_to_tile_y(max_lat, tile_zoom),
        south: lat_to_tile_y(min_lat, tile_zoom),
    }
}

#[path = "tiles/tests.rs"]
#[cfg(test)]
mod tests;
