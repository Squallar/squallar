//! Where a long-running, CPU-bound job runs.
//!
//! Four places in this crate hand a closure somewhere it will not stall the
//! frame that created it: the static radar render, the loop-frame render, the
//! overlay rasterization and the radar-sites rasterization. All four have the
//! same shape — a `FnOnce` that ends by sending its result on an
//! `mpsc::Sender` and calling `notify_redraw` — and all four had the same
//! `std::thread::Builder` call written out inline.
//!
//! They are funnelled through one function here so the wasm arm exists once.

/// Run `job` away from the frame that requested it.
///
/// Native spawns a named OS thread and returns immediately.
///
/// wasm32-unknown-unknown has no threads: `std::thread::Builder::spawn` there
/// returns `Err(Unsupported)` at *runtime* rather than failing to compile, so a
/// bare spawn site does not break the web build — it compiles clean and then
/// panics the first time the user asks for a radar frame. That is the failure
/// this function exists to remove. The web arm runs `job` inline.
///
/// Running inline blocks the frame, and for radar rasterization that is a
/// visible stall; the worker in `rustdar-web` is the answer for that one path.
/// This is what keeps every *other* offloaded job correct rather than absent,
/// and what the rasterizer falls back to when no worker is available.
///
/// Inline execution preserves the contract the callers actually depend on. Each
/// `job` delivers through a channel that is drained on a later frame, so a send
/// that happens before the caller returns is indistinguishable from one that
/// happens after it — the receiver cannot tell, and neither can the render
/// budget, whose `RenderGuard` simply drops sooner.
///
/// The `Send` bound is kept on both arms deliberately. It costs the web arm
/// nothing (every existing caller already satisfies it, since they were written
/// for threads) and dropping it would silently license a `!Send` job that then
/// fails to compile on desktop.
pub fn offload(name: &'static str, job: impl FnOnce() + Send + 'static) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::thread::Builder::new()
            .name(name.into())
            .spawn(job)
            .unwrap_or_else(|e| panic!("failed to spawn {name} thread: {e}"));
    }
    #[cfg(target_arch = "wasm32")]
    {
        // Timed because this is the one arm where the cost lands on the frame.
        // The number is what decides whether a worker is needed and how many, so
        // it is logged rather than estimated.
        let started = web_time::Instant::now();
        job();
        log::info!("{name} took {} ms on the main thread", started.elapsed().as_millis());
    }
}
