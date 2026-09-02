use crate::tile_source::HttpsTiles;
use squallar_geo::{lat_to_tile_y, lon_to_tile_x};
use walkers::sources::Attribution;

/// Where the basemap credit links. ODbL wants the notice reachable, not just
/// shown.
pub const ATTRIBUTION_URL: &str = "https://www.openstreetmap.org/copyright";

/// The credit the panel paints for a session whose archive has been found
/// unreachable — the on-screen half of reporting the fault, drawn in the
/// corner the credit already occupies rather than as a new surface.
///
/// There is no raster provider to fall back to any more (CartoDB was deleted
/// with the flip to the archive basemap), so an unreachable archive means the
/// ground layer draws **nothing**, and this line is what keeps that state from
/// being a silent blank: [`MapTileState::base_archive_is_unreachable`] is how
/// the panel knows to paint it. It names what is wrong, not what the reader
/// should do about it; the detail that lets them fix it -- the URL and the
/// transport error -- is one `log::error!` away and would not fit here.
///
/// ASCII, deliberately: `ui_glyphs` gates every non-ASCII character UI text
/// carries against the bundled fonts, and this string owes no copyright sign
/// because no provider's pixels are on the glass while it shows.
pub const UNREACHABLE_ATTRIBUTION_TEXT: &str = "basemap archive unreachable";

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
pub const BASEMAP_ARCHIVE_URL: &str = "https://tiles.squallar.app/basemap/omt-20260828.pmtiles";

/// An archive URL that replaces [`BASEMAP_ARCHIVE_URL`] when it is set.
///
/// Native only, and read once per source construction. It exists because the
/// published archive is the *only* other way to exercise this path, and a
/// change to the draw seam that can only be checked against 83 GB over a
/// network is a change that will stop being checked.
///
/// **It must name an `https://` URL.** `file://` is not a scheme
/// [`crate::basemap_archive::HttpRangeSource`] serves, and a plain-HTTP local
/// server does not work either: the client every archive read goes through is
/// `squallar_source::tls`, which sets `https_only`, pinned by
/// `the_archive_client_refuses_cleartext`. A cleartext override is rejected on
/// the scheme before a byte is fetched. Serve the local archive over TLS.
#[cfg(not(target_arch = "wasm32"))]
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
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn archive_url() -> String {
    std::env::var(BASEMAP_ARCHIVE_URL_ENV).unwrap_or_else(|_| BASEMAP_ARCHIVE_URL.to_owned())
}

/// The wasm32 arm of [`archive_url`]: the compiled-in archive, always.
#[cfg(target_arch = "wasm32")]
pub(crate) fn archive_url() -> String {
    BASEMAP_ARCHIVE_URL.to_owned()
}

/// The self-hosted terrain hillshade PMTiles archive.
///
/// Published as parts (`.part000`..): `HttpRangeSource` probes `<url>.part000`
/// at open and selects parts mode on its own, so this names the logical
/// archive and nothing here knows parts exist. The generation
/// (`7c94bc6966ab-20260829`) is compiled in, like the basemap's.
pub const TERRAIN_ARCHIVE_URL: &str =
    "https://tiles.squallar.app/terrain/7c94bc6966ab-20260829/squallar-terrain-hillshade.pmtiles";

/// An archive URL that replaces [`TERRAIN_ARCHIVE_URL`] when it is set.
/// Native only, for the reason [`BASEMAP_ARCHIVE_URL_ENV`] is: the draw seam
/// must stay checkable against a local archive. **Over TLS** — see that const
/// for why cleartext is refused before a byte is fetched.
#[cfg(not(target_arch = "wasm32"))]
pub const TERRAIN_ARCHIVE_URL_ENV: &str = "SQUALLAR_TERRAIN_ARCHIVE";

/// Which archive the terrain source opens — [`archive_url`]'s split, for
/// [`archive_url`]'s reason.
#[cfg(not(target_arch = "wasm32"))]
fn terrain_archive_url() -> String {
    std::env::var(TERRAIN_ARCHIVE_URL_ENV).unwrap_or_else(|_| TERRAIN_ARCHIVE_URL.to_owned())
}

/// The wasm32 arm of [`terrain_archive_url`]: the compiled-in archive, always.
#[cfg(target_arch = "wasm32")]
fn terrain_archive_url() -> String {
    TERRAIN_ARCHIVE_URL.to_owned()
}

// ---------------------------------------------------------------------------
// The height archives: terrain-RGB, read as data rather than drawn as pixels
// ---------------------------------------------------------------------------

/// The generation segment both height archive URLs carry **while no height
/// archive has been published**.
///
/// The archives these URLs name do not exist: building them is a separate
/// work unit that needs ~1.75 TB of scratch and hours of S3 streaming, and it
/// has not run. A plausible-looking `<12-hex>-<YYYYMMDD>` here would compile,
/// pass every pin in the tree and 404 in the field, so the placeholder says so
/// in the URL itself instead.
///
/// [`crate::tiles::tests`]'s `the_height_archives_are_still_unpublished` holds
/// both URLs against this string, so the day a real generation is configured
/// that test goes red and its message names every other site that has to move
/// in the same commit. Removing the marker without moving them is the failure
/// mode it exists to make impossible.
pub const HEIGHT_ARCHIVE_GENERATION_PLACEHOLDER: &str = "UNPUBLISHED-GENERATION";

/// The self-hosted global terrain-RGB PMTiles archive: packed elevation,
/// z0-z11.
///
/// Published like the hillshade, in parts, so [`crate::basemap_archive::HttpRangeSource`]
/// probes `<url>.part000` at open and a bare `GET` of this path 404s by
/// design.
///
/// **The bodies are PNG and there is no picture in them.** Each pixel is a
/// base-256 elevation triple; anything that hands one to an image compositor
/// is a bug, which is what `tile_source::ArchiveTileKind::TerrainRgb`
/// exists to catch.
///
/// The generation is [`HEIGHT_ARCHIVE_GENERATION_PLACEHOLDER`] until the
/// archive exists.
pub const HEIGHT_ARCHIVE_URL: &str = "https://tiles.squallar.app/terrain-rgb/UNPUBLISHED-GENERATION/squallar-terrain-terrain-rgb.pmtiles";

/// The CONUS terrain-RGB archive: the same encoding at z11-z12 over
/// `-125,24,-66,50`, which is where the radar sites that need a metre-scale
/// ground are.
///
/// A second archive rather than a deeper first one because the two are built
/// by separate runs with separate scopes and are independently openable. Its
/// generation is [`HEIGHT_ARCHIVE_GENERATION_PLACEHOLDER`] for the same reason
/// [`HEIGHT_ARCHIVE_URL`]'s is.
///
/// It is in [`live_archive_urls`] from the day it is declared, even though
/// nothing opens it yet: a generation the block cache does not know about is
/// deleted at the first cache open of every launch, so the list has to lead
/// the reader rather than follow it.
pub const CONUS_HEIGHT_ARCHIVE_URL: &str = "https://tiles.squallar.app/terrain-rgb-conus/UNPUBLISHED-GENERATION/squallar-terrain-terrain-rgb.pmtiles";

/// An archive URL that replaces [`HEIGHT_ARCHIVE_URL`] when it is set.
/// Native only, for the reason [`BASEMAP_ARCHIVE_URL_ENV`] is.
///
/// **It cannot name a cleartext URL through [`height_range_source`]**, for the
/// reason [`BASEMAP_ARCHIVE_URL_ENV`] cannot: `archive_client` sets
/// `https_only`. A local archive is therefore served over TLS, or read through
/// a source built with a client of the caller's own -- which is what
/// `tiles::height_tests` does.
#[cfg(not(target_arch = "wasm32"))]
pub const HEIGHT_ARCHIVE_URL_ENV: &str = "SQUALLAR_HEIGHT_ARCHIVE";

/// Which archive the height reader opens -- [`archive_url`]'s split, for
/// [`archive_url`]'s reason.
#[cfg(not(target_arch = "wasm32"))]
pub fn height_archive_url() -> String {
    std::env::var(HEIGHT_ARCHIVE_URL_ENV).unwrap_or_else(|_| HEIGHT_ARCHIVE_URL.to_owned())
}

/// The wasm32 arm of [`height_archive_url`]: the compiled-in archive, always.
#[cfg(target_arch = "wasm32")]
pub fn height_archive_url() -> String {
    HEIGHT_ARCHIVE_URL.to_owned()
}

/// Where the persistent archive block cache lives, installed once by the
/// application at construction from `PlatformBridge::basemap_cache_dir` —
/// the same platform fact `zone_cache_dir` is, reaching its consumer the way
/// that one does: decided by the platform, handed over once, never poked
/// through a UI setter.
///
/// A process-wide `OnceLock` rather than a `Gui` field because it is process
/// configuration, not UI state: [`base_source`] and the terrain source are
/// free functions, called again after a [`MapTileState::clear`] and once per
/// slot per session otherwise, and every build must see the same answer.
/// (Neither a theme flip nor a layer toggle is a build any more — the first
/// re-styles in place, the second parks.) Ungated and target-shared: a
/// build without the archive reader, or a platform with no filesystem,
/// simply never reads it.
static BASEMAP_CACHE_DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// Install the archive block cache directory. First installation wins;
/// installing nothing leaves every archive read uncached, which is the
/// degraded mode the cache is documented to fall back to.
pub fn install_basemap_cache_dir(dir: std::path::PathBuf) {
    let _ = BASEMAP_CACHE_DIR.set(dir);
}

/// Every archive URL this build reads, in the one place they are enumerated.
///
/// **The whole of the block cache's invalidation is derived from this list**,
/// and a URL missing from it is not a degraded cache but a deleted one:
/// `gc_stale_generations` `remove_dir_all`s every directory under the shared
/// root whose name is not a live generation, from `ensure_open` inside a
/// `get_or_init`, so the omission costs its archive the whole of its cache at
/// the first cache open of **every launch** — a slow map, never an error.
///
/// It is therefore every archive, not the one being opened: the sources are
/// built lazily and in no fixed order, and a live set that only knew the
/// opening source would cost the others theirs.
///
/// `the_live_generation_set_covers_every_archive_url_the_build_reads` in
/// `tiles::tests` is the ratchet: a fifth archive that is declared and not
/// listed here reddens rather than silently wiping a fourth.
#[cfg(not(target_arch = "wasm32"))]
fn live_archive_urls() -> Vec<String> {
    vec![
        archive_url(),
        terrain_archive_url(),
        height_archive_url(),
        CONUS_HEIGHT_ARCHIVE_URL.to_owned(),
    ]
}

/// The block cache configuration for the archive at `url`, or `None` when no
/// cache directory was installed.
///
/// The live-generation set is [`live_archive_urls`] mapped through
/// `generation_for_url`, no matter which source is being built — see that
/// function for why the set is every archive rather than this one.
#[cfg(not(target_arch = "wasm32"))]
fn archive_block_cache(url: &str) -> Option<crate::basemap_archive::block_cache::BlockCacheConfig> {
    use crate::basemap_archive::block_cache;

    let root = BASEMAP_CACHE_DIR.get()?.clone();
    Some(block_cache::BlockCacheConfig {
        root,
        generation: block_cache::generation_for_url(url),
        live_generations: live_archive_urls()
            .iter()
            .map(|url| block_cache::generation_for_url(url))
            .collect(),
        cap_bytes: block_cache::BLOCK_CACHE_BYTES,
    })
}

/// The wasm32 arm of [`archive_block_cache`]: no filesystem, no cache — the
/// same selection-of-a-body split as [`archive_url`].
#[cfg(target_arch = "wasm32")]
fn archive_block_cache(
    _url: &str,
) -> Option<crate::basemap_archive::block_cache::BlockCacheConfig> {
    None
}

/// The store the downloaded areas on this device are read back through, or
/// `None` when this platform has nowhere to keep them.
///
/// The same selection-of-a-body split as [`archive_block_cache`] and
/// [`archive_url`]: native reads plain files out of the basemap directory the
/// bridge chose; the web store is the service worker's route, and until that
/// route exists there is nothing to read back. Neither arm branches inside a
/// body — each *is* the body its target selects.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn offline_store(
    basemap_dir: Option<&std::path::Path>,
) -> Option<crate::basemap_download::PlatformSegmentStore> {
    Some(crate::basemap_download::FsSegmentStore::new(
        basemap_dir?.to_path_buf(),
    ))
}

/// The wasm32 arm of [`offline_store`]. The service-worker routes the store
/// would read through are not built yet, and a store pointed at a route that
/// does not answer would spend a request per launch to be told so.
#[cfg(target_arch = "wasm32")]
pub(crate) fn offline_store(
    _basemap_dir: Option<&std::path::Path>,
) -> Option<crate::basemap_download::PlatformSegmentStore> {
    None
}

/// A range source over the base map archive, for the two readers that are not
/// the render path: the size measurement and the download engine.
///
/// **The same URL and the same client the map itself reads through**, so a
/// figure is quoted against the archive the download will actually fetch from.
/// Deliberately *not* `HttpsTiles`: its request channel is bounded at 6 and
/// its LRU is the live map's working set, so measuring or downloading through
/// it would evict what the user is looking at.
///
/// # Errors
///
/// [`crate::basemap_archive::RangeError`] if the archive URL will not parse —
/// the same and only construction-time failure `from_archive_url` has.
pub(crate) fn archive_range_source()
-> Result<crate::basemap_archive::HttpRangeSource, crate::basemap_archive::RangeError> {
    crate::basemap_archive::HttpRangeSource::new(
        crate::basemap_archive::archive_client(),
        &archive_url(),
    )
}

/// [`archive_range_source`] over the terrain hillshade archive, for the same
/// two readers.
///
/// The terrain archive is published in parts and a bare `GET` of the logical
/// name 404s **by design**; `HttpRangeSource` probes `<url>.part000` at open
/// and selects parts mode itself, so nothing here or above knows parts exist.
///
/// # Errors
///
/// As [`archive_range_source`].
pub(crate) fn terrain_range_source()
-> Result<crate::basemap_archive::HttpRangeSource, crate::basemap_archive::RangeError> {
    crate::basemap_archive::HttpRangeSource::new(
        crate::basemap_archive::archive_client(),
        &terrain_archive_url(),
    )
}

/// The generation the terrain archive this build reads carries — the one
/// derivation, so a record and the block cache cannot disagree about which
/// hillshade a byte came from.
pub(crate) fn terrain_generation() -> String {
    crate::basemap_archive::block_cache::generation_for_url(&terrain_archive_url())
}

/// A range source over the global height archive.
///
/// **Deliberately not a third [`HttpsTiles`], and not by analogy with
/// [`terrain_source`].** Heights are data, not pixels: an `HttpsTiles` would
/// decode every body into a texture, run the hillshade remap over packed
/// elevation, and spend the render path's own tile cache on grids nothing
/// draws. The reader above this is the layer `terrain_range_source` already
/// shows is reusable —
///
/// ```text
/// height_range_source()                       // parts probe, free
///   -> block_cache::BlockCachedSource::new(_, archive_block_cache(&url))
///   -> BasemapArchives::open(_)
///   -> .tile(z, x, y) -> TileBytes             // undecoded, on BOTH targets
/// ```
///
/// — and the bytes stay undecoded all the way to whoever unpacks them, which
/// is not this crate.
///
/// The archive is published in parts and a bare `GET` of the logical name
/// 404s **by design**; `HttpRangeSource` probes `<url>.part000` at open and
/// selects parts mode itself.
///
/// # Errors
///
/// [`crate::basemap_archive::RangeError`] if the archive URL will not parse —
/// the same and only construction-time failure `terrain_range_source` has.
pub fn height_range_source()
-> Result<crate::basemap_archive::HttpRangeSource, crate::basemap_archive::RangeError> {
    crate::basemap_archive::HttpRangeSource::new(
        crate::basemap_archive::archive_client(),
        &height_archive_url(),
    )
}

/// The generation the height archive this build reads carries — the one
/// derivation, exactly as [`terrain_generation`] is for the hillshade.
pub fn height_generation() -> String {
    crate::basemap_archive::block_cache::generation_for_url(&height_archive_url())
}

/// The credit the hillshade's elevation data requires.
///
/// The DEM is `COP-DEM_GLO-30 Public, 2021 release` — see
/// `tools/squallar-terrain/README.md` for the pinned provenance. Carried on
/// the source like every other credit, and read off it by the panel's one
/// notice: while the terrain slot holds a source, `ui_map` appends this line
/// to the base credit — one notice per panel still — and drops it the frame
/// the slot is released, because an idle credit is clutter that dilutes the
/// required ones.
pub const TERRAIN_ATTRIBUTION_TEXT: &str = "\u{a9} Copernicus DEM 2021";

/// The terrain hillshade tile source, or `None` when the archive URL will not
/// parse — the only failure visible at construction; the rest surface on
/// `HttpsTiles::fault` inside the IO task.
///
/// `basemap_dir` is the same store the base source reads its downloaded areas
/// out of: an area's two halves live side by side in it, and each source takes
/// its own. A downloaded area's hillshade therefore draws with no network for
/// the same reason its base map does, through the same composition.
fn terrain_source(
    ctx: &egui::Context,
    basemap_dir: Option<&std::path::Path>,
) -> Option<HttpsTiles> {
    let url = terrain_archive_url();
    let attribution = Attribution {
        text: TERRAIN_ATTRIBUTION_TEXT,
        url: "https://spacedata.copernicus.eu/collections/copernicus-digital-elevation-model",
        logo_light: None,
        logo_dark: None,
    };

    match HttpsTiles::from_terrain_archive_url(
        &url,
        attribution,
        ctx.to_owned(),
        archive_block_cache(&url),
        offline_store(basemap_dir),
    ) {
        Ok(tiles) => Some(tiles),
        Err(error) => {
            log::error!("{url} is not a usable terrain archive URL: {error}");
            None
        }
    }
}

/// The credit the vector basemap carries, in the generator's own words.
///
/// Planetiler prints this pair as the required credit for what it built, so it
/// is sourced rather than composed. The panel draws it because the archive
/// source reports it, not because anything here selects between two consts.
pub const ARCHIVE_ATTRIBUTION_TEXT: &str = "\u{a9} OpenStreetMap contributors \u{a9} OpenMapTiles";

/// The base-map tile source for `is_dark`: our own archive, rendered here, or
/// `None` if the archive URL will not parse — the only failure visible at
/// construction. The failures that are not — the host being down, a stale
/// `omt-YYYYMMDD` generation, the body not being PMTiles — happen inside the
/// IO task, which records them on `HttpsTiles::fault`;
/// [`MapTileState::ensure_base_tiles`] is where a frame acts on that. There is
/// no raster provider to fall back to.
fn base_source(
    is_dark: bool,
    disabled_source_layers: &std::collections::BTreeSet<String>,
    ctx: &egui::Context,
    basemap_dir: Option<&std::path::Path>,
) -> Option<HttpsTiles> {
    let url = archive_url();

    let attribution = Attribution {
        text: ARCHIVE_ATTRIBUTION_TEXT,
        url: ATTRIBUTION_URL,
        logo_light: None,
        logo_dark: None,
    };

    match HttpsTiles::from_archive_url(
        &url,
        crate::basemap_style::committed_filtered(is_dark, disabled_source_layers),
        attribution,
        ctx.to_owned(),
        archive_block_cache(&url),
        offline_store(basemap_dir),
    ) {
        Ok(tiles) => Some(tiles),
        Err(error) => {
            log::error!("{url} is not a usable basemap archive URL: {error}");
            None
        }
    }
}

/// Shared map tile state across all panes.
pub struct MapTileState {
    /// The tile source, styled for the theme currently on the glass.
    ///
    /// **One slot, not one per theme and not one per surface.** A theme flip
    /// does not replace the source: [`Self::ensure_base_tiles`] re-styles it
    /// in place ([`HttpsTiles::set_style`]), so its parsed-geometry cache
    /// survives the flip and the new theme is derived from it without a
    /// fetch. A second theme slot would hold nothing but a duplicate of this
    /// one's caches; and labels are no longer a source of their own, because
    /// the vector basemap draws them out of the same tile it draws the ground
    /// from.
    pub tiles: Option<HttpsTiles>,
    pub current_theme_is_dark: bool,

    /// The terrain hillshade source — the second slot, beside the basemap's.
    ///
    /// Built lazily by [`Self::ensure_terrain_tiles`], **only while some
    /// visible pane has the Terrain layer on**: a disabled layer must cost
    /// zero network, and a source that exists is a source whose IO task will
    /// be asked for tiles. Parked by [`Self::release_terrain_tiles`] when the
    /// last pane switches the layer off — still zero network, because a source
    /// nothing draws is a source nothing asks, and no longer a re-download if
    /// it comes back; see [`Self::parked_terrain`] for why it is parked rather
    /// than dropped. Actually let go by [`Self::clear`], with everything else.
    ///
    /// **A theme flip does NOT touch it.** The base slot is re-styled on a
    /// flip because the committed style its tiles are styled with is the
    /// theme; the hillshade remap (black shadows, white highlights, alpha
    /// from relief — `terrain::remap_hillshade`) is theme-independent by
    /// design, so its pixels are right under both themes and there is nothing
    /// for a flip to re-style or re-download.
    pub terrain: Option<HttpsTiles>,

    /// Whether the basemap archive has already been found unreachable this
    /// session, so [`Self::ensure_base_tiles`] must not build another source.
    ///
    /// Without the latch every frame after a failed open would build the
    /// source again, fail again, and keep a dead retry loop warm. There is no
    /// raster provider to fall back to since the CartoDB path was deleted:
    /// while latched the ground layer draws **nothing**, said once at
    /// `error!`, and the panel paints [`UNREACHABLE_ATTRIBUTION_TEXT`] in the
    /// credit corner (read through [`Self::base_archive_is_unreachable`]) so
    /// the degraded state is on the glass rather than only in a log. Cleared
    /// by [`Self::clear`], because a suspend/resume or a graphics reset is a
    /// plausible moment for a network to have come back.
    base_unreachable: bool,

    /// What the live base source last said about its own tile reads, sampled
    /// once per frame by [`Self::ensure_base_tiles`] — see
    /// [`HttpsTiles::reads_are_failing`].
    ///
    /// **Sampled rather than read through, and the reason is ownership**: the
    /// panel hands the source out of the slot for the whole pane loop
    /// (`ui_map`'s `take_base_tiles`), and the credit is composed inside it, so
    /// [`Self::tiles`] is empty at exactly the moment the question is asked.
    ///
    /// Distinct from [`Self::base_unreachable`] in the direction that matters:
    /// that one is latched for the session because the archive will never
    /// open, this one goes back down on its own when the reads answer again.
    base_reads_failing: bool,

    /// Never build a tile source: the test harnesses' switch, set through
    /// [`Self::go_offline_for_tests`]. Distinct from
    /// [`Self::base_unreachable`], which is a *found* degraded state and
    /// changes the painted credit; offline is the normal state's geometry
    /// with the network left out.
    offline: bool,

    /// Which source-layers were disabled in the style [`Self::tiles`] is
    /// currently styled with — half of the comparison
    /// [`Self::ensure_base_tiles`] makes to notice a toggle flip (the other
    /// half is [`Self::current_theme_is_dark`]). Meaningful only while the
    /// slot holds the vector source; the raster fallback has no style to
    /// filter.
    base_disabled_source_layers: std::collections::BTreeSet<String>,

    /// How many times a base source has been constructed — the probe the
    /// restyle-not-rebuild pin reads, because two `HttpsTiles` cannot be
    /// compared for identity once moved.
    #[cfg(test)]
    pub(crate) base_builds: usize,

    /// How many times the live base source has been re-styled in place — the
    /// other half of that pin: a theme flip or a toggle flip must move this,
    /// not [`Self::base_builds`].
    #[cfg(test)]
    pub(crate) base_restyles: usize,

    /// [`Self::base_unreachable`]'s twin for the terrain slot, latched for
    /// the same reason. A failed terrain archive means the layer draws
    /// nothing, said once at `error!`. Cleared by [`Self::clear`], because a
    /// resume is a plausible moment for the network to be back.
    terrain_failed: bool,

    /// The base source the BasemapTiles layer was switched off with, kept
    /// alive so switching it back on is a move rather than a rebuild.
    ///
    /// **Parked, not leaked.** At most one source lives here, it is the one the
    /// previous frame was already drawing, and it does no work: a parked source
    /// is never drawn, so nothing calls `request_once` on it and it issues no
    /// requests. What it holds is its LRU and its parsed-geometry cache, which
    /// is exactly what makes coming back free.
    ///
    /// The reason it is parked rather than dropped is that **dropping it is not
    /// free and not asynchronous**. `HttpsTiles` owns a `runtime::Runtime`
    /// whose `Drop` joins the IO thread, and that thread's tokio runtime waits
    /// for `spawn_blocking` tile tessellations that have already started. That
    /// join runs on whoever drops the source, and the dropper here is
    /// `Gui::ui` - the frame thread. Measured release on 2026-08-31 against the
    /// committed Monaco fixture: **up to 13.1 ms of frame-thread block**,
    /// against 0.034 ms for a source with no IO thread. `tile_source`'s own
    /// note called this out as unverified and named "a layer release" as one of
    /// the moments that could still hit it; it does, and this is that moment
    /// removed.
    parked_base: Option<HttpsTiles>,

    /// [`Self::parked_base`] for the terrain slot, for the same reason. The
    /// hillshade source owns the same kind of `Runtime` and its release ran the
    /// same join on the same thread.
    parked_terrain: Option<HttpsTiles>,

    /// Where this device keeps downloaded offline areas, or `None` when the
    /// platform has nowhere for them.
    ///
    /// Handed over once, at `Gui` construction (`Gui::with_basemap_dir`),
    /// never through a per-frame argument: the path is constant for the
    /// process, and `ensure_base_tiles` is called from a frame. A source
    /// built while this is `None` reads everything from the network, which is
    /// the state every test that does not opt in is in.
    basemap_dir: Option<std::path::PathBuf>,
}

impl Default for MapTileState {
    fn default() -> Self {
        Self {
            tiles: None,
            terrain: None,
            current_theme_is_dark: true,
            base_unreachable: false,
            base_reads_failing: false,
            offline: false,
            base_disabled_source_layers: std::collections::BTreeSet::new(),
            #[cfg(test)]
            base_builds: 0,
            #[cfg(test)]
            base_restyles: 0,
            terrain_failed: false,
            parked_base: None,
            parked_terrain: None,
            basemap_dir: None,
        }
    }
}

impl MapTileState {
    /// Point the base slot at this device's downloaded basemap areas.
    ///
    /// Called once, from `Gui::with_basemap_dir`, before any frame. Not a
    /// per-frame argument and not a re-buildable setting: a source already
    /// built keeps the store it was built with, and the next rebuild — a
    /// theme flip is not one — picks this up.
    pub(crate) fn set_basemap_dir(&mut self, dir: Option<std::path::PathBuf>) {
        self.basemap_dir = dir;
    }

    /// Ensure the base-map tiles for the current theme and detail set are
    /// initialized, and retire a source that has reported itself unusable.
    ///
    /// The archive is opened by the IO task, two range requests after the
    /// frame has already been handed a source, so a DNS failure, a 404, a host
    /// that is down, a stale `omt-YYYYMMDD` generation or a typo in
    /// [`BASEMAP_ARCHIVE_URL_ENV`] all surface here rather than at
    /// construction. `HttpsTiles::fault` is how they surface; this is what
    /// acts on them, once, by latching [`Self::base_unreachable`]. There is no
    /// raster provider left to fall back to, so the honest degraded state is a
    /// ground layer that draws nothing -- not silent in either direction: it
    /// logs at `error!` with the transport's own words, and the panel paints
    /// [`UNREACHABLE_ATTRIBUTION_TEXT`] in the credit corner for as long as
    /// the latch holds.
    ///
    /// **A style change — a theme flip, or a flip of the BasemapTiles layer's
    /// per-source-layer detail set — re-styles the live source in place**
    /// ([`HttpsTiles::set_style`]): the source's parsed-geometry cache serves
    /// the new style with zero fetches and zero re-parses, the tiles on the
    /// glass keep drawing in the outgoing style until each restyled one
    /// arrives, and nothing blanks. This replaces the v1 rebuild, which
    /// dropped the source and re-downloaded every visible tile — the cost the
    /// old note here recorded as accepted with the parsed cache as future
    /// work. The terrain slot is untouched by any of it: the hillshade remap
    /// is theme-independent by design (see the `terrain` field's docs).
    pub fn ensure_base_tiles(
        &mut self,
        is_dark: bool,
        disabled_source_layers: &std::collections::BTreeSet<String>,
        ctx: &egui::Context,
    ) {
        // Unpark first, before anything reads the slot. A theme or detail flip
        // that happened while the layer was off has to land on the source that
        // comes back, and the restyle arm below can only see a source that is
        // already in the slot; unparking after it would bring back a source
        // styled for the theme the user has since left.
        if self.tiles.is_none() {
            self.tiles = self.parked_base.take();
        }

        if let Some(fault) = self.tiles.as_ref().and_then(HttpsTiles::fault) {
            log::error!(
                "the basemap archive is unusable, so the basemap draws nothing \
                 this session: {fault}"
            );
            self.base_unreachable = true;
            self.tiles = None;
            // An unusable archive is unusable parked, too.
            self.parked_base = None;
        }

        let style_changed = self.current_theme_is_dark != is_dark
            || self.base_disabled_source_layers != *disabled_source_layers;
        if style_changed {
            self.current_theme_is_dark = is_dark;
            self.base_disabled_source_layers = disabled_source_layers.clone();
            if let Some(tiles) = self.tiles.as_mut() {
                tiles.set_style(crate::basemap_style::committed_filtered(
                    is_dark,
                    disabled_source_layers,
                ));
                #[cfg(test)]
                {
                    self.base_restyles += 1;
                }
            }
        }

        if self.tiles.is_none() && !self.base_unreachable {
            self.current_theme_is_dark = is_dark;
            self.base_disabled_source_layers = disabled_source_layers.clone();
            #[cfg(test)]
            {
                self.base_builds += 1;
            }
            self.tiles = if self.offline {
                Some(HttpsTiles::inert(
                    Attribution {
                        text: ARCHIVE_ATTRIBUTION_TEXT,
                        url: ATTRIBUTION_URL,
                        logo_light: None,
                        logo_dark: None,
                    },
                    ctx.to_owned(),
                ))
            } else {
                base_source(
                    is_dark,
                    disabled_source_layers,
                    ctx,
                    self.basemap_dir.as_deref(),
                )
            };
            // A URL that will not parse yields no source and never will this
            // session: latch, exactly as the terrain slot does, so the
            // per-frame cost of an unbuildable source is one bool read.
            self.base_unreachable = self.tiles.is_none();
        }

        // **After the build arm, so a source made this frame is told before
        // it is asked for a tile** — that first install is the one
        // `HttpsTiles::set_feathering` does not bump the generation for.
        // Unconditional and per frame: the comparison inside is one `f32`,
        // and the event this exists to catch — the window crossing to a
        // different-DPI display — arrives as a changed `pixels_per_point` on
        // an ordinary frame with no other seam behind it. The terrain slot is
        // untouched: a hillshade tile is raster and flattens to nothing.
        if let Some(tiles) = self.tiles.as_mut() {
            tiles.set_feathering(crate::tile_mesh::feathering_of(ctx));
        }

        // The other half of the health question, and the half the archive can
        // only answer after it has opened: a source that opened and then
        // answered nothing. Sampled here, where the slot is still full — the
        // panel empties it for the pane loop before it composes the credit.
        self.base_reads_failing = self
            .tiles
            .as_ref()
            .is_some_and(HttpsTiles::reads_are_failing);
    }

    /// Whether the basemap archive is not putting tiles on the glass, so the
    /// panel must paint [`UNREACHABLE_ATTRIBUTION_TEXT`] instead of a provider
    /// credit: the degraded state has to be on the glass, not only in the log.
    ///
    /// Two states, because there are two ways to draw nothing and the credit
    /// lies in both. The archive that will not open is latched for the session
    /// and leaves the slot empty. A source that opened and then failed every
    /// tile read keeps its slot, keeps its IO task, and recovers on its own —
    /// so this can go back down, and the caller must ask every frame rather
    /// than remembering the answer.
    pub fn base_archive_is_unreachable(&self) -> bool {
        self.base_unreachable || self.base_reads_failing
    }

    /// Put the base slot in the state a dead archive leaves it in: latched
    /// unreachable, source dropped. For the credit-composition tests, which
    /// need the latch without a real transport failure to raise it; what the
    /// tests using it prove is therefore the *composition* given the latch,
    /// not the latch's own raising.
    #[cfg(test)]
    pub(crate) fn latch_base_unreachable_for_test(&mut self) {
        self.base_unreachable = true;
        self.tiles = None;
    }

    /// Put a slot in the state a source that opened and then answered nothing
    /// leaves it in: the source still there, still its own IO task, reporting
    /// its reads as failing. Answers whether there was a source to say it of,
    /// so a caller can refuse to assert on an empty slot.
    ///
    /// The distinction from [`Self::latch_base_unreachable_for_test`] is the
    /// whole point: that one empties the slot, so a credit composed from it
    /// never exercises the arm where a live source's own attribution is the
    /// thing that must be overruled.
    #[cfg(test)]
    pub(crate) fn fail_reads_for_test(&mut self, base: bool) -> bool {
        let slot = if base {
            self.tiles.as_ref()
        } else {
            self.terrain.as_ref()
        };
        let Some(tiles) = slot else { return false };
        tiles.fail_reads_for_test();
        if base {
            self.base_reads_failing = true;
        }
        true
    }

    /// Whether this instance is building only inert sources.
    ///
    /// Read by the frame path's *other* archive readers — the offline-area
    /// size measurement and the download engine — so that one switch covers
    /// every route to the production archive rather than only the tile slots.
    /// Always false outside a test harness.
    pub(crate) fn is_offline(&self) -> bool {
        self.offline
    }

    /// Build only inert tile sources for the rest of this instance's life.
    ///
    /// **For test harnesses that drive `Gui::ui`, and nothing else.** A live
    /// source is an IO thread making range requests against the production
    /// archive, and what it delivers races the frames of whatever built it,
    /// with two arms: tiles that arrive paint label `TextShape`s at
    /// arbitrary map positions (measured 2026-08-29: a harness frame held
    /// open for 400 ms painted 80+ live city labels, several straddling
    /// pill-row rects), and a transport fault latches
    /// [`UNREACHABLE_ATTRIBUTION_TEXT`] over the provider credit. Either way
    /// a unit test's glass changed with how much wall-clock time the test
    /// took — the under-load flakes this switch removes.
    ///
    /// The slots still fill, with [`HttpsTiles::inert`]: every ensure/release
    /// path and the credit composition run exactly as shipped, against a
    /// source whose header never arrives and whose fault can never be set —
    /// the fast-test glass, on every run. Unreachable is NOT latched; a test
    /// that wants the degraded credit raises the latch itself through
    /// [`Self::latch_base_unreachable_for_test`].
    pub fn go_offline_for_tests(&mut self) {
        self.offline = true;
        self.tiles = None;
        self.terrain = None;
        // The park slots too: a source built before the switch is a live one,
        // and unparking it later would put the thing this switch exists to
        // prevent straight back into the slot.
        self.parked_base = None;
        self.parked_terrain = None;
    }

    /// Park the base source: the last visible pane switched the BasemapTiles
    /// layer off. Symmetric with [`Self::release_terrain_tiles`] — a disabled
    /// layer still costs zero network, because a source nothing draws is a
    /// source nothing asks for tiles. The unreachable latch survives, exactly
    /// as the terrain failure latch does: a release is not a network recovery.
    ///
    /// **This used to drop the source, and dropping it blocked the frame
    /// thread** for as long as an already-started tile tessellation took to
    /// finish — see [`Self::parked_base`] for the mechanism and the 13.1 ms
    /// measurement. It also threw away the parsed-geometry cache, so switching
    /// the layer back on re-downloaded and re-tessellated every visible tile.
    /// Parking removes both, and is the same trade
    /// [`HttpsTiles::set_style`] already makes for a theme flip.
    ///
    /// The *read-failure* sample does not survive, because it is not a latch:
    /// it describes what the user can see, and they can no longer see it. Left
    /// standing it would have a layer the user switched off reported as an
    /// unreachable archive — the same lie in the other direction.
    pub fn release_base_tiles(&mut self) {
        // `take`, and only when it yields: two releases in a row must not
        // overwrite the parked source with the empty slot the first one left.
        if let Some(tiles) = self.tiles.take() {
            self.parked_base = Some(tiles);
        }
        self.base_reads_failing = false;
    }

    /// Temporarily take the base tiles out of self for per-pane rendering.
    pub fn take_base_tiles(&mut self) -> Option<HttpsTiles> {
        self.tiles.take()
    }

    /// Restore the base tiles after per-pane rendering.
    pub fn restore_base_tiles(&mut self, tiles: Option<HttpsTiles>) {
        self.tiles = tiles;
    }

    /// Ensure the terrain hillshade source exists, and replace one that has
    /// reported itself unusable. Call **only while some visible pane draws
    /// the Terrain layer** — the lazy half of the slot's zero-cost-when-off
    /// contract; [`Self::release_terrain_tiles`] is the other half.
    ///
    /// The fault handling is [`Self::ensure_base_tiles`]'s without the
    /// fallback arm: there is nothing to fall back to, so a dead archive is
    /// logged once and the layer draws nothing until [`Self::clear`] resets
    /// the latch.
    pub fn ensure_terrain_tiles(&mut self, ctx: &egui::Context) {
        // As in `ensure_base_tiles`: the slot is refilled from the park before
        // anything else looks at it.
        if self.terrain.is_none() {
            self.terrain = self.parked_terrain.take();
        }

        if let Some(fault) = self.terrain.as_ref().and_then(HttpsTiles::fault) {
            log::error!(
                "the terrain archive is unusable, so the Terrain layer draws                  nothing this session: {fault}"
            );
            self.terrain_failed = true;
            self.terrain = None;
            self.parked_terrain = None;
        }

        if self.terrain.is_none() && !self.terrain_failed {
            self.terrain = if self.offline {
                Some(HttpsTiles::inert(
                    Attribution {
                        text: TERRAIN_ATTRIBUTION_TEXT,
                        url: "https://spacedata.copernicus.eu/collections/copernicus-digital-elevation-model",
                        logo_light: None,
                        logo_dark: None,
                    },
                    ctx.to_owned(),
                ))
            } else {
                terrain_source(ctx, self.basemap_dir.as_deref())
            };
            // A build with no archive reader, or a URL that will not parse,
            // yields no source and never will this session: latch, so the
            // per-frame cost of an unbuildable source is one bool read.
            self.terrain_failed = self.terrain.is_none();
        }
    }

    /// Park the terrain source: the last pane showing the layer switched it
    /// off. Symmetric with [`Self::release_base_tiles`], including why it is a
    /// park and not a drop — see [`Self::parked_terrain`].
    pub fn release_terrain_tiles(&mut self) {
        if let Some(terrain) = self.terrain.take() {
            self.parked_terrain = Some(terrain);
        }
    }

    /// Temporarily take the terrain tiles out of self for per-pane rendering.
    pub fn take_terrain_tiles(&mut self) -> Option<HttpsTiles> {
        self.terrain.take()
    }

    /// Restore the terrain tiles after per-pane rendering.
    pub fn restore_terrain_tiles(&mut self, tiles: Option<HttpsTiles>) {
        self.terrain = tiles;
    }

    /// Clear all tile state (called on suspend/graphics reset).
    ///
    /// Both unreachable latches go with it: a resume is a plausible moment for
    /// a network that was down to be up, and retrying the archive costs one
    /// failed open if it is not.
    pub fn clear(&mut self) {
        self.base_unreachable = false;
        self.base_reads_failing = false;
        self.tiles = None;
        self.terrain = None;
        self.terrain_failed = false;
        // A suspend or a graphics reset really does mean let go: the park
        // exists to survive a *toggle*, and a source parked across a suspend
        // would hold its caches and its IO thread for as long as the app sat
        // in the background.
        self.parked_base = None;
        self.parked_terrain = None;
        // `current_theme_is_dark` and the disabled set stay: they describe the
        // style last asked for, and the next `ensure_base_tiles` builds fresh
        // on the empty slot whatever they say.
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

/// How far out of the drawn level the always-resident ancestor net sits, in
/// whole zoom steps.
///
/// **This is what stands between a zoom-out and a black screen.**
/// `HttpsTiles::cached_or_interpolated` answers a missing tile with a cached
/// *ancestor*, walking towards zoom 0 and never towards the leaves, so the
/// tiles a zoom-out was just looking at — which are its descendants — can never
/// answer for it. Nothing else requested a shallower level either:
/// `HttpsTiles::ground_at` asks only for the level being drawn. So a session
/// that had only ever been deep held nothing at all for a shallow viewport, and
/// drew nothing, until the network answered.
///
/// One tile this many steps out covers `2^steps` of the drawn level on a side,
/// so the net is a whole viewport's ancestry for a handful of entries: at 4
/// steps a 1920x1200-point canvas keeps **4** of them beside the 96 it draws.
///
/// Four rather than one because one is unaffordable and four is free. The
/// immediate parent level would be the sharpest net, but its span is a quarter
/// of the drawn span plus its boundary — 35 more entries at 1920x1200, taking
/// the wasm working set to 131, whose vector worst case is 322 MiB against
/// `squallar-device-profile`'s 288 MiB whole-application texture budget. Four
/// steps costs 8 entries at the bound below and overruns nothing. What it gives
/// up is sharpness in the covering ancestor, which is a stretched tile either
/// way and is replaced the moment the real one lands.
pub const WARM_ANCESTOR_STEPS: u8 = 4;

/// How many tiles a rect keeps resident once the [`WARM_ANCESTOR_STEPS`] net is
/// kept warm beside the level being drawn — **the sizing answer for any cache
/// behind `ui_map_overlays::draw_tile_layer`**, which requests both.
///
/// The net's span is the drawn span shifted right by [`WARM_ANCESTOR_STEPS`],
/// and a shift can only ever land a range of `n` indices on `n / 2^steps + 1`
/// of them; the `+ 1` beyond that is the grid phase, exactly as in
/// [`tiles_resident_at_tile_scale`]. So this is a bound rather than a
/// measurement. `ui_map_overlays`'
/// `tests::a_drawn_layer_asks_for_the_ancestor_net_and_no_more_than_its_bound`
/// holds one measured `tile_span` under it — a 1920x1080 canvas at zoom 6 —
/// against the net a real `draw_tile_layer` pass asked for. That is a single
/// point, not a sweep: no test walks the zoom range against this bound.
pub fn tiles_resident_with_warm_net(rect: egui::Rect, zoom_bias: u8, layers: usize) -> usize {
    let (across, down) = tiles_resident_grid(rect, zoom_bias, MIN_TILE_SCALE);
    let net = |n: usize| n.saturating_sub(1) / (1 << WARM_ANCESTOR_STEPS) + 2;
    across
        .saturating_mul(down)
        .saturating_add(net(across).saturating_mul(net(down)))
        .saturating_mul(layers)
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
    let (across, down) = tiles_resident_grid(rect, zoom_bias, scale);
    across.saturating_mul(down).saturating_mul(layers)
}

/// The resident grid's two dimensions, before they are multiplied out. Shared
/// so [`tiles_resident_with_warm_net`] shifts the same figures the drawn count
/// is built from and the two can never drift.
fn tiles_resident_grid(rect: egui::Rect, zoom_bias: u8, scale: f32) -> (usize, usize) {
    let side = TILE_SIDE_POINTS * scale / 2f32.powi(i32::from(zoom_bias));
    if side <= 0.0 || !rect.width().is_finite() || !rect.height().is_finite() {
        return (0, 0);
    }
    let across = (rect.width().max(0.0) / side).ceil() as usize + 1;
    let down = (rect.height().max(0.0) / side).ceil() as usize + 1;
    (across, down)
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

/// The height reader's own suite: a fixture archive over a loopback server,
/// read through the override. Beside [`tests`] rather than inside it because
/// it needs the archive module's loopback harness and a child process, and
/// neither is a thing the tile-geometry suite has any business carrying.
#[path = "tiles/height_tests.rs"]
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod height_tests;

/// Archive bytes to real Colorado heights, end to end over a served fixture
/// archive holding the committed real Terrain-RGB tile. Separate from
/// [`height_tests`] because it is about the bytes rather than about the URL
/// override, and so needs no child process.
#[path = "tiles/real_terrain_tests.rs"]
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod real_terrain_tests;
