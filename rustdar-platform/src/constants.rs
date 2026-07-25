/// Default width for the application window in pixels
pub const RENDER_WIDTH: u32 = 1920;

/// Default height for the application window in pixels
pub const RENDER_HEIGHT: u32 = 1080;

/// Maximum number of concurrent background radar render threads (loop + static).
/// Android devices have much less RAM, so we cap aggressively to avoid OOM.
#[cfg(target_os = "android")]
pub const MAX_CONCURRENT_RENDERS: usize = 3;
#[cfg(not(target_os = "android"))]
pub const MAX_CONCURRENT_RENDERS: usize = 6;

/// Maximum number of loop frames to consider for rendering per dispatch cycle.
#[cfg(target_os = "android")]
pub const MAX_LOOP_RENDER_BUDGET: usize = 12;
#[cfg(not(target_os = "android"))]
pub const MAX_LOOP_RENDER_BUDGET: usize = 30;

/// Maximum number of concurrent loop scan downloads per pane.
#[cfg(target_os = "android")]
pub const MAX_CONCURRENT_LOOP_DOWNLOADS: usize = 4;
#[cfg(not(target_os = "android"))]
pub const MAX_CONCURRENT_LOOP_DOWNLOADS: usize = 8;

/// Maximum total number of loop frames kept per pane.
/// Limits combined memory from textures and scan data.
#[cfg(target_os = "android")]
pub const MAX_LOOP_FRAMES: usize = 20;
#[cfg(not(target_os = "android"))]
pub const MAX_LOOP_FRAMES: usize = 60;

/// Maximum number of entries kept in `RenderDispatcher::render_cache`.
///
/// The cache exists so panes showing the same site/product/elevation share one
/// render; it is not a history. Each entry holds an RGBA image and a matching
/// `f32` value grid — `IMAGE_SIZE² × 8` bytes, 32 MiB at 2048² — and until this
/// bound existed the only thing that ever removed one was `reset_panes*`, so a
/// user cycling products accumulated them without limit.
///
/// Sized to comfortably exceed the pane count (`MAX_PANES_DESKTOP` is 6,
/// `MAX_PANES_MOBILE` is 4) so the panes on screen can never evict each other,
/// with a little headroom for switching back and forth.
#[cfg(target_os = "android")]
pub const MAX_RENDER_CACHE_ENTRIES: usize = 6;
#[cfg(not(target_os = "android"))]
pub const MAX_RENDER_CACHE_ENTRIES: usize = 8;
