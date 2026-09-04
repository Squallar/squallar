//! **The tile offloader reads the lane, and the lane is not the funnel.**
//!
//! `WorkerTileOffloader` is `cfg(target_arch = "wasm32")`, so no native test
//! executes a line of it and the wasm gate only proves it compiles — and both
//! spellings compile. The difference between them is the whole work order:
//! `jobs_in_worker()` is every job the rasterization worker owes, which on the
//! user's `huge` scene with a loop playing is never zero (18 jobs, 34.1 s of
//! worker time, 8 outstanding at leg end, 2026-09-02), so the pump's gate
//! declined every pass and 108 of 108 vector bodies were styled on the frame
//! thread. `jobs_in_lane()` is what the nested lane Worker owes, and the
//! worker's queue does not figure in it.
//!
//! So the claim is held as a source scrape, which is what this tree does when
//! the behaviour is real but unreachable from a host test — see
//! `squallar_egui::tile_source`'s basemap-ledger pin, and `pwa_assets.rs`. The
//! executing halves are elsewhere and both are green: the rule is
//! `squallar_egui::tile_source::tests` (`should_stage`), and the independence
//! of the two counts is `squallar_worker::offload::lane_tests`.

const APP_FETCH: &str = include_str!("../app_fetch.rs");

/// The body of `impl TileOffloader for WorkerTileOffloader`, to the closing
/// brace at item indentation.
fn offloader_impl() -> &'static str {
    let (_, rest) = APP_FETCH
        .split_once("impl squallar_egui::tile_source::TileOffloader for WorkerTileOffloader {")
        .expect("app_fetch.rs no longer implements the tile offloader seam");
    rest.split_once("\n}")
        .map(|(body, _)| body)
        .expect("the offloader impl has no recognisable body")
}

#[test]
fn the_tile_offloader_asks_the_lane_and_never_the_worker() {
    let body = offloader_impl();

    // Control: the scrape found the right item.
    assert!(
        body.contains("fn queued(&self)") && body.contains("fn post("),
        "the offloader impl no longer carries both seam methods, so the \
         absence checks below are reading the wrong text",
    );

    for lane in ["lane_attached()", "jobs_in_lane"] {
        assert!(
            body.contains(lane),
            "the tile offloader no longer names `{lane}`. The pump's staging \
             gate reads whatever this returns; anything but the lane's own \
             count puts a busy rasterization worker back in front of the \
             basemap, which is the 108-of-108 reading this work order exists \
             to remove.",
        );
    }

    for funnel in ["jobs_in_worker", "worker_attached()", "expecting_sink()"] {
        assert!(
            !body.contains(funnel),
            "the tile offloader names `{funnel}`. That is the funnel's whole \
             queue, not the lane's: a 3.9-5.0 s model rasterization in it \
             would make the gate decline and every vector tile would be \
             styled on the frame thread again.",
        );
    }

    // And the post goes to the lane's sink, not the funnel's. `offload_job`
    // runs a job INLINE when no sink is attached, which for a batch of
    // thirteen tessellations is the slowest arm there is; `offload_to_lane`
    // hands it back unrun so the pump styles it in slices instead.
    assert!(
        body.contains("offload_to_lane("),
        "the tile offloader no longer posts to the lane",
    );
    assert!(
        !body.contains("offload_job("),
        "the tile offloader posts to the funnel. A batch queued there waits \
         behind whatever the worker is running, which is the wait the pump \
         measured and refused.",
    );
}
