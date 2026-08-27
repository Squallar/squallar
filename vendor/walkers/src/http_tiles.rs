use bytes::Bytes;
use egui::Context;
use reqwest_middleware::ClientWithMiddleware;

use crate::io::Fetch;
use crate::io::http::http_client;
use crate::io::tiles_io::TilesIo;
use crate::sources::{Attribution, TileSource};
use crate::style::Style;
use crate::tiles::{EguiTileFactory, interpolate_from_lower_zoom};
use crate::{HttpOptions, TilePiece, Tiles};
use crate::{Stats, TileId};

/// Downloads the tiles via HTTP. It must persist between frames.
pub struct HttpTiles {
    attribution: Attribution,
    tiles_io: TilesIo,
    tile_size: u32,
    max_zoom: u8,
}

impl HttpTiles {
    /// Construct new [`Tiles`] with default [`HttpOptions`].
    pub fn new<S>(source: S, egui_ctx: Context) -> Self
    where
        S: TileSource + Sync + Send + 'static,
    {
        Self::with_options(source, HttpOptions::default(), egui_ctx)
    }

    /// Construct new [`Tiles`] with supplied [`HttpOptions`].
    pub fn with_options<S>(source: S, http_options: HttpOptions, egui_ctx: Context) -> Self
    where
        S: TileSource + Sync + Send + 'static,
    {
        Self::with_options_and_style(source, http_options, Style::default(), egui_ctx)
    }

    /// Construct new [`Tiles`] with supplied [`HttpOptions`] and [`Style`]. Style is relevant
    /// only for vector tile sources.
    pub fn with_options_and_style<S>(
        source: S,
        http_options: HttpOptions,
        style: Style,
        egui_ctx: Context,
    ) -> Self
    where
        S: TileSource + Sync + Send + 'static,
    {
        let attribution = source.attribution();
        let tile_size = source.tile_size();
        let max_zoom = source.max_zoom();

        Self {
            attribution,
            tiles_io: TilesIo::new(
                HttpFetch::new(source, http_options),
                EguiTileFactory::new(egui_ctx.clone(), style),
                egui_ctx,
            ),
            tile_size,
            max_zoom,
        }
    }

    pub fn stats(&self) -> Stats {
        self.tiles_io.stats()
    }

    /// Get at tile, or interpolate it from lower zoom levels. This function does not start any
    /// downloads.
    fn get_from_cache_or_interpolate(&mut self, tile_id: TileId) -> Option<TilePiece> {
        let mut zoom_candidate = tile_id.zoom;

        loop {
            let (zoomed_tile_id, uv) = interpolate_from_lower_zoom(tile_id, zoom_candidate);

            if let Some(Some(tile)) = self.tiles_io.cache.get(&zoomed_tile_id) {
                break Some(TilePiece {
                    tile: tile.clone(),
                    uv,
                });
            }

            // Keep zooming out until we find a donor or there is no more zoom levels.
            zoom_candidate = zoom_candidate.checked_sub(1)?;
        }
    }
}

impl Tiles for HttpTiles {
    /// Attribution of the source this tile cache pulls images from. Typically,
    /// this should be displayed somewhere on the top of the map widget.
    fn attribution(&self) -> Attribution {
        self.attribution.clone()
    }

    /// Return a tile if already in cache, schedule a download otherwise.
    fn at(&mut self, tile_id: TileId) -> Option<TilePiece> {
        self.tiles_io.put_single_fetched_tile_in_cache();

        if !tile_id.valid() {
            return None;
        }

        let tile_id_to_download = if tile_id.zoom > self.max_zoom {
            interpolate_from_lower_zoom(tile_id, self.max_zoom).0
        } else {
            tile_id
        };

        self.tiles_io.make_sure_is_fetched(tile_id_to_download);
        self.get_from_cache_or_interpolate(tile_id)
    }

    fn tile_size(&self) -> u32 {
        self.tile_size
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HttpFetchError {
    #[error(transparent)]
    HttpMiddleware(#[from] reqwest_middleware::Error),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
}

pub struct HttpFetch<S>
where
    S: TileSource + Send + 'static,
{
    source: S,
    max_concurrency: usize,
    client: ClientWithMiddleware,
}

impl<S> HttpFetch<S>
where
    S: TileSource + Sync + Send,
{
    pub fn new(source: S, http_options: HttpOptions) -> Self {
        Self {
            source,
            max_concurrency: http_options.max_parallel_downloads.0,
            client: http_client(&http_options),
        }
    }
}

impl<S> Fetch for HttpFetch<S>
where
    S: TileSource + Sync + Send,
{
    type Error = HttpFetchError;

    async fn fetch(&self, tile_id: TileId) -> Result<Bytes, Self::Error> {
        let url = self.source.tile_url(tile_id);
        log::trace!("Downloading '{url}'.");
        let image = self.client.get(&url).send().await?;
        log::trace!("Downloaded '{}': {:?}.", url, image.status());
        Ok(image.error_for_status()?.bytes().await?)
    }

    fn max_concurrency(&self) -> usize {
        self.max_concurrency
    }
}
