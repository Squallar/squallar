use crate::tile_source::HttpsTiles;
use squallar_geo::{lat_to_tile_y, lon_to_tile_x};
use walkers::{
    TileId,
    sources::{Attribution, TileSource},
};

/// CartoDB's credit: the raster basemap's, and nothing else's.
///
/// **This is not "the" attribution any more.** The map panel draws whatever
/// [`walkers::Tiles::attribution`] answers for the source it is actually
/// fetching from, so a build reading our own archive credits OpenMapTiles
/// without this const being involved. It is the [`CartoDb`] impl's string, and
/// the panel's fallback for a frame with no source at all.
///
/// `\u{a9}` is U+00A9, registered in `ui_glyphs` as the basemap attribution
/// copyright -- never spelled `(c)`.
pub const ATTRIBUTION_TEXT: &str = "\u{a9} OpenStreetMap \u{a9} CartoDB";

/// Where the credit links. ODbL wants the notice reachable, not just shown.
pub const ATTRIBUTION_URL: &str = "https://www.openstreetmap.org/copyright";

/// The credit a build that *meant* to draw the vector archive carries once it
/// has fallen back to the rasters.
///
/// **This is the on-screen half of reporting the fault**, and it is drawn in the
/// corner the credit already occupies rather than as a new surface: the panel
/// reads [`walkers::Tiles::attribution`] off whichever source it is actually
/// drawing, so replacing the source replaces the words. Without it the fallback
/// would be indistinguishable from a build configured for CartoDB in the first
/// place, which is exactly the silent-partial-success shape this workspace
/// keeps finding.
///
/// It names what is wrong, not what the reader should do about it: the detail
/// that lets them fix it -- the URL and the transport error -- is one
/// `log::error!` away and would not fit here.
///
/// ASCII apart from the two copyright signs, deliberately: `ui_glyphs` gates
/// every non-ASCII character UI text carries against the bundled fonts, and an
/// em dash is not on that inventory.
#[cfg(feature = "basemap-vector")]
pub const FALLBACK_ATTRIBUTION_TEXT: &str =
    "\u{a9} OpenStreetMap \u{a9} CartoDB (basemap archive unavailable)";

/// What [`MapTileState::ensure_base_tiles`] credits a fallback source with.
///
/// A `cfg` selecting a value, not a branch: on a build with no vector archive
/// there is nothing to fall back *from*, the fallback arm is unreachable, and
/// the ordinary credit is the one that would be honest if it were not.
#[cfg(feature = "basemap-vector")]
const FALLBACK_CREDIT: &str = FALLBACK_ATTRIBUTION_TEXT;
/// See the arm above.
#[cfg(not(feature = "basemap-vector"))]
const FALLBACK_CREDIT: &str = ATTRIBUTION_TEXT;

/// CartoDB tile source variants.
#[derive(Clone)]
pub enum CartoDbStyle {
    LightNoLabels,
    DarkNoLabels,
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
}

impl TileSource for CartoDb {
    fn tile_url(&self, tile_id: TileId) -> String {
        let style_name = match self.style {
            CartoDbStyle::LightNoLabels => "light_nolabels",
            CartoDbStyle::DarkNoLabels => "dark_nolabels",
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
#[cfg(feature = "basemap-vector")]
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

/// Which archive [`base_source`] opens.
///
/// A per-target **selection of a value**, which is what keeps [`base_source`]
/// itself one body rather than two: the override is a developer affordance for
/// a machine with a shell, and a browser has neither an environment nor a way
/// for the user to set one. `std::env::var` does compile on
/// wasm32-unknown-unknown and would simply always answer `NotPresent`, so this
/// split buys honesty rather than compilation -- a const documented "native
/// only" that the web arm still read would be prose that is not evidence.
#[cfg(all(feature = "basemap-vector", not(target_arch = "wasm32")))]
fn archive_url() -> String {
    std::env::var(BASEMAP_ARCHIVE_URL_ENV).unwrap_or_else(|_| BASEMAP_ARCHIVE_URL.to_owned())
}

/// The wasm32 arm of [`archive_url`]: the compiled-in archive, always.
#[cfg(all(feature = "basemap-vector", target_arch = "wasm32"))]
fn archive_url() -> String {
    BASEMAP_ARCHIVE_URL.to_owned()
}

/// The credit the vector basemap carries, in the generator's own words.
///
/// Planetiler prints this pair as the required credit for what it built, so it
/// is sourced rather than composed. The panel draws it because the archive
/// source reports it, not because anything here selects between two consts.
#[cfg(feature = "basemap-vector")]
pub const ARCHIVE_ATTRIBUTION_TEXT: &str = "\u{a9} OpenStreetMap contributors \u{a9} OpenMapTiles";

/// CartoDB's pre-rendered rasters, for `is_dark`.
///
/// The base source on a build without the vector archive, and the fallback on
/// one that has it but cannot reach it — see [`MapTileState::ensure_base_tiles`].
/// `credit` is a parameter because a fallback has to say that it *is* one.
fn cartodb_source(is_dark: bool, ctx: &egui::Context, credit: &'static str) -> HttpsTiles {
    let source = if is_dark {
        CartoDb::dark()
    } else {
        CartoDb::light()
    };
    HttpsTiles::with_attribution(
        source,
        ctx.to_owned(),
        Attribution {
            text: credit,
            url: ATTRIBUTION_URL,
            logo_light: None,
            logo_dark: None,
        },
    )
}

/// The base-map tile source for `is_dark`: CartoDB's pre-rendered rasters.
#[cfg(not(feature = "basemap-vector"))]
fn base_source(is_dark: bool, ctx: &egui::Context) -> HttpsTiles {
    cartodb_source(is_dark, ctx, ATTRIBUTION_TEXT)
}

/// The base-map tile source for `is_dark`: our own archive, rendered here.
///
/// Falls back to CartoDB if the archive URL will not parse, which is the only
/// failure visible at construction. The failures that are not — the host being
/// down, a stale `omt-YYYYMMDD` generation, the body not being PMTiles — happen
/// inside the IO task, which records them on `HttpsTiles::fault`;
/// [`MapTileState::ensure_base_tiles`] is where a frame acts on that.
#[cfg(feature = "basemap-vector")]
fn base_source(is_dark: bool, ctx: &egui::Context) -> HttpsTiles {
    let url = archive_url();

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
            cartodb_source(is_dark, ctx, FALLBACK_ATTRIBUTION_TEXT)
        }
    }
}

/// Shared map tile state across all panes.
pub struct MapTileState {
    /// The tile source for the theme currently on the glass.
    ///
    /// **One slot, not one per theme and not one per surface.**
    /// [`Self::adopt_theme`] releases the source the instant its theme stops
    /// being drawn, so a second theme slot could only ever hold `None`; and
    /// labels are no longer a source of their own, because the vector basemap
    /// draws them out of the same tile it draws the ground from.
    pub tiles: Option<HttpsTiles>,
    pub current_theme_is_dark: bool,

    /// Whether the vector archive has already been found unreachable this
    /// session, so [`Self::ensure_base_tiles`] must not build another one.
    ///
    /// Without the latch a theme flip -- which drops the source and rebuilds it
    /// -- would go back to the archive, fail again, and blank the map again.
    /// Cleared by [`Self::clear`], because a suspend/resume or a graphics reset
    /// is a plausible moment for a network to have come back.
    ///
    /// Always present, never `cfg`-gated: a build with no vector archive simply
    /// never sets it, and `HttpsTiles::fault` answers `None` for a raster
    /// source on every target.
    fell_back_to_raster: bool,
}

impl Default for MapTileState {
    fn default() -> Self {
        Self {
            tiles: None,
            current_theme_is_dark: true,
            fell_back_to_raster: false,
        }
    }
}

impl MapTileState {
    /// Adopt `is_dark` as the theme, releasing the one no longer drawn.
    ///
    /// **Nothing on the glass is blanked.** The source dropped belongs to the
    /// theme that is no longer drawn; the pane is repainting from the other
    /// theme's source this frame regardless. The accepted cost is a re-download
    /// if the user flips back.
    ///
    /// Without it a single flip took residency from one live source to two and
    /// held it for the session, because [`Self::ensure_base_tiles`] only ever
    /// fills and [`Self::clear`] runs only on suspend and graphics reset. At
    /// [`crate::tile_source::WASM_TILE_CACHE_ENTRIES`] one source's worst case
    /// is 24 MiB of raster tiles, or ~59 MiB now that wasm32 draws the vector
    /// basemap too, against the 288 MiB `squallar-device-profile` allows the
    /// whole application on wasm32.
    fn adopt_theme(&mut self, is_dark: bool) {
        if is_dark == self.current_theme_is_dark {
            return;
        }
        self.current_theme_is_dark = is_dark;
        self.tiles = None;
    }

    /// Ensure the base-map tiles for the current theme are initialized, and
    /// replace a source that has reported itself unusable.
    ///
    /// **A blank map is not an acceptable resting state.** The archive is
    /// opened by the IO task, two range requests after the frame has already
    /// been handed a source, so a DNS failure, a 404, a host that is down, a
    /// stale `omt-YYYYMMDD` generation or a typo in
    /// [`BASEMAP_ARCHIVE_URL_ENV`] all surface here rather than at
    /// construction. `HttpsTiles::fault` is how they surface; this is what acts
    /// on them, once, by falling back to the raster source the build already
    /// knows how to reach.
    ///
    /// The fallback is not silent in either direction: it logs at `error!` with
    /// the transport's own words, and the credit the panel paints changes to
    /// [`FALLBACK_ATTRIBUTION_TEXT`], because the panel reads the credit off
    /// whichever source is actually being drawn.
    pub fn ensure_base_tiles(&mut self, is_dark: bool, ctx: &egui::Context) {
        self.adopt_theme(is_dark);

        if let Some(fault) = self.tiles.as_ref().and_then(HttpsTiles::fault) {
            log::error!(
                "the basemap archive is unusable, falling back to the raster \
                 basemap for this session: {fault}"
            );
            self.fell_back_to_raster = true;
            self.tiles = None;
        }

        if self.tiles.is_none() {
            self.tiles = Some(if self.fell_back_to_raster {
                cartodb_source(is_dark, ctx, FALLBACK_CREDIT)
            } else {
                base_source(is_dark, ctx)
            });
        }
    }

    /// Temporarily take the base tiles out of self for per-pane rendering.
    pub fn take_base_tiles(&mut self) -> Option<HttpsTiles> {
        self.tiles.take()
    }

    /// Restore the base tiles after per-pane rendering.
    pub fn restore_base_tiles(&mut self, tiles: Option<HttpsTiles>) {
        self.tiles = tiles;
    }

    /// Clear all tile state (called on suspend/graphics reset).
    ///
    /// The fallback latch goes with it: a resume is a plausible moment for a
    /// network that was down to be up, and retrying the archive costs one
    /// failed open if it is not.
    pub fn clear(&mut self) {
        self.fell_back_to_raster = false;
        self.tiles = None;
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
