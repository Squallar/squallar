//! Raster tiles of one size share one texture, so a viewport of them is one
//! draw instead of one draw each.
//!
//! # The figure this exists for
//!
//! A `ClippedPrimitive` opens on a change of clip rect, a change of texture,
//! or a callback (`epaint::tessellator`), and the length of the primitive
//! list is the length of the frame tail — on the web GL backend
//! `queue.submit` replays every recorded command as a real GL call on the
//! frame thread, and that is 93% of the tail.
//!
//! A raster tile layer draws its viewport one `Painter::image` per cell under
//! **one** clip rect, so nothing but the texture separates them. Measured on
//! the native rig's scene D (1920x1080, one pane, terrain shading on,
//! 2026-09-05): the terrain hillshade put **47 tiles on the glass as 47
//! primitives, 47 draws, 47 bind groups and 94 buffer binds** — 47 of the 73
//! primitives of a frame with no basemap pass in it, and 47 of the 120 draws
//! of one with. Every one of those draws was six indices: a single quad.
//!
//! The tiles are the same size and the same format and they do not overlap,
//! so the only thing making them 47 of anything is 47 texture ids. Put them
//! in one texture and epaint coalesces the run into one mesh by itself.
//!
//! # What the page costs
//!
//! A page is allocated in full and uploaded once — 2048x2048 RGBA is 16 MB of
//! zeros — where the per-tile textures it replaces are uploaded as they
//! arrive. That is a one-off against a per-frame saving, and the steady state
//! is within 4% of what it replaces: a tile's own upload grows by its gutter
//! (a 256x256 tile is written as 258x258).
//!
//! # The gutter, and why the page is not a round number
//!
//! Slots sit on a pitch of the tile's side plus two, and a tile is written
//! into the middle of its slot with its edge row and column **duplicated**
//! into the border. Without it a tile magnified even slightly samples past
//! its own edge and picks up the neighbouring slot — a seam of somebody
//! else's terrain along every tile boundary — because a shared texture has no
//! per-tile clamp. With it, sampling anywhere inside the tile's window reads
//! either the tile or a copy of its own edge, which is exactly what
//! `ClampToEdge` gives a tile that owns its texture.
//!
//! The page side is therefore a whole number of pitches rather than of tiles,
//! and it is capped at [`MAX_PAGE_SIDE`] — 2048, the largest 2D texture
//! WebGL2 is required to support, and this workspace ships to WebGL2. A
//! 256x256 tile pitches at 258, seven of which fit, so a page is 1806x1806
//! and holds 49 tiles: one more than the 47 the measurement above put on the
//! glass, and a viewport needing more spills into a second page and draws two
//! primitives rather than one.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use egui::{ColorImage, Context, Rect, TextureHandle};
use walkers::RasterTile;

/// The largest 2D texture side this may ask for.
///
/// `wgpu::Limits::downlevel_webgl2_defaults()` sets `max_texture_dimension_2d`
/// to 2048 and this workspace ships a WebGL2 target, so a page wider than this
/// is a page that fails to allocate on the browser arm and nowhere else.
pub(crate) const MAX_PAGE_SIDE: usize = 2048;

/// Texels of duplicated edge around each tile in a page. See the module docs.
pub(crate) const GUTTER: usize = 1;

/// How many slots of `side` texels fit along a page edge, or `None` when a
/// tile that size cannot usefully share.
///
/// Two per edge is the floor, so a page that exists holds at least four
/// tiles. One per edge is a page holding one tile: the zero-fill of a whole
/// page to save nothing.
fn slots_along(side: usize) -> Option<usize> {
    let pitch = side.checked_add(2 * GUTTER)?;
    let slots = MAX_PAGE_SIDE / pitch;
    (slots >= 2).then_some(slots)
}

/// One shared texture and which of its slots are free.
struct Page {
    texture: TextureHandle,
    /// Free slot indices, in row-major order over the page.
    free: Vec<u32>,
    /// Slots handed out and not yet returned. The page is dropped at zero.
    live: usize,
    /// Slots along one edge, for turning an index into an origin.
    cols: usize,
    /// The page's size in texels, for turning an origin into a window.
    page_size: [usize; 2],
}

/// Every page, by the tile size it holds.
#[derive(Default)]
struct Atlas {
    classes: HashMap<[usize; 2], Vec<Option<Page>>>,
    /// Names pages uniquely, so two pages of one class are two names in the
    /// texture manager rather than one name twice.
    minted: u64,
}

/// The atlas, as the context holds it.
#[derive(Clone)]
struct Shared(Arc<Mutex<Atlas>>);

/// A slot reserved for as long as this value lives.
///
/// **The `Drop` is the whole contract.** [`RasterTile`] holds one of these
/// behind an `Arc` and hands it to every clone, so the slot returns to its
/// page when the last copy of the tile — the cache's, and any the frame took
/// out of it — is gone, and not before.
struct Lease {
    atlas: Arc<Mutex<Atlas>>,
    class: [usize; 2],
    page: usize,
    slot: u32,
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mut atlas = self.atlas.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(pages) = atlas.classes.get_mut(&self.class) else {
            return;
        };
        let Some(slot) = pages.get_mut(self.page) else {
            return;
        };
        let Some(page) = slot.as_mut() else {
            return;
        };
        page.free.push(self.slot);
        page.live = page.live.saturating_sub(1);
        // An empty page is released rather than kept warm. Keeping it would
        // hold its whole area for a layer that has been switched off, and the
        // page after it costs one zero-fill to build again — the same trade
        // the per-tile textures this replaces made, at page granularity.
        if page.live == 0 {
            *slot = None;
        }
        if pages.iter().all(Option::is_none) {
            atlas.classes.remove(&self.class);
        }
    }
}

/// The context's atlas, made on first use.
fn shared(ctx: &Context) -> Arc<Mutex<Atlas>> {
    ctx.data_mut(|data| {
        data.get_temp_mut_or_insert_with(egui::Id::new("squallar.raster_atlas"), || {
            Shared(Arc::new(Mutex::new(Atlas::default())))
        })
        .0
        .clone()
    })
}

/// Put one decoded raster tile on the GPU, sharing a texture with the other
/// tiles its size where that is possible.
///
/// `name` names the tile's own texture on the fallback path and contributes
/// nothing on the shared one, where the page carries its own name: a page is
/// one texture holding many tiles and cannot be named after one of them.
///
/// **Falls back to a texture of the tile's own** for any size that cannot
/// usefully share — see [`slots_along`]. The caller cannot tell the two apart
/// and does not have to: [`RasterTile::window_of`] is the identity on the
/// fallback.
pub(crate) fn place(ctx: &Context, name: &str, image: ColorImage) -> RasterTile {
    let size = image.size;
    match reserve(ctx, &image) {
        Some((texture, window, lease)) => RasterTile::shared(texture, window, size, lease),
        None => RasterTile::own(ctx.load_texture(name, image, Default::default())),
    }
}

/// Reserve a slot for `image`, write it there, and hand back what draws it:
/// the page's texture, the window the tile occupies, and the lease that holds
/// the slot. `None` for a tile that cannot share.
fn reserve(
    ctx: &Context,
    image: &ColorImage,
) -> Option<(TextureHandle, Rect, Arc<dyn std::any::Any + Send + Sync>)> {
    let size = image.size;
    let cols = slots_along(size[0])?;
    let rows = slots_along(size[1])?;
    let pitch = [size[0] + 2 * GUTTER, size[1] + 2 * GUTTER];
    let page_size = [cols * pitch[0], rows * pitch[1]];

    let atlas = shared(ctx);
    let mut guard = atlas.lock().unwrap_or_else(PoisonError::into_inner);

    let pages = guard.classes.entry(size).or_default();
    let index = match pages
        .iter()
        .position(|page| page.as_ref().is_some_and(|page| !page.free.is_empty()))
    {
        Some(index) => index,
        None => {
            let minted = guard.minted;
            guard.minted += 1;
            // Built and uploaded holding the atlas lock but **not** the
            // context's memory lock — `shared` released that before returning,
            // and `Context::load_texture` takes the texture manager's write
            // lock, which nothing on this path holds.
            let texture = ctx.load_texture(
                format!("raster_atlas_{}x{}_{minted}", size[0], size[1]),
                ColorImage::filled(page_size, egui::Color32::TRANSPARENT),
                Default::default(),
            );
            let page = Page {
                texture,
                // Popped from the back, so slots are handed out in row-major
                // order and a viewport's tiles land next to each other.
                free: (0..(cols * rows) as u32).rev().collect(),
                live: 0,
                cols,
                page_size,
            };
            let pages = guard.classes.entry(size).or_default();
            match pages.iter().position(Option::is_none) {
                Some(index) => {
                    pages[index] = Some(page);
                    index
                }
                None => {
                    pages.push(Some(page));
                    pages.len() - 1
                }
            }
        }
    };

    let page = guard
        .classes
        .get_mut(&size)
        .and_then(|pages| pages.get_mut(index))
        .and_then(Option::as_mut)?;
    let slot = page.free.pop()?;
    page.live += 1;
    let origin = [
        (slot as usize % page.cols) * pitch[0],
        (slot as usize / page.cols) * pitch[1],
    ];
    page.texture
        .set_partial(origin, gutter(image), Default::default());
    let texture = page.texture.clone();
    let page_size = page.page_size;
    drop(guard);

    let min = egui::pos2(
        (origin[0] + GUTTER) as f32 / page_size[0] as f32,
        (origin[1] + GUTTER) as f32 / page_size[1] as f32,
    );
    let window = Rect::from_min_max(
        min,
        egui::pos2(
            min.x + size[0] as f32 / page_size[0] as f32,
            min.y + size[1] as f32 / page_size[1] as f32,
        ),
    );
    Some((
        texture,
        window,
        Arc::new(Lease {
            atlas,
            class: size,
            page: index,
            slot,
        }),
    ))
}

/// `image` with its edge row and column duplicated [`GUTTER`] texels outwards.
///
/// This is what a shared texture has instead of `ClampToEdge`: a sampler
/// reaching past the tile's window finds a copy of the tile's own edge rather
/// than the neighbouring slot. See the module docs.
fn gutter(image: &ColorImage) -> ColorImage {
    let [w, h] = image.size;
    let padded = [w + 2 * GUTTER, h + 2 * GUTTER];
    let mut pixels = Vec::with_capacity(padded[0] * padded[1]);
    // `clamp` maps a padded coordinate onto the source's nearest real texel,
    // which is the duplication stated as arithmetic: the gutter rows are
    // copies of the first and last source rows, the gutter columns of the
    // first and last source columns, corners included.
    let clamp = |v: usize, len: usize| v.saturating_sub(GUTTER).min(len - 1);
    for y in 0..padded[1] {
        let row = clamp(y, h) * w;
        for x in 0..padded[0] {
            pixels.push(image.pixels[row + clamp(x, w)]);
        }
    }
    ColorImage::new(padded, pixels)
}

#[cfg(test)]
#[path = "raster_atlas/tests.rs"]
mod tests;
