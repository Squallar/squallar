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
