//! The crate's parallel-iteration prelude: rayon, on every target.
//!
//! The web arm used to be a `mod seq` of sequential stand-ins, because
//! wasm32-unknown-unknown had no threads to give rayon. It has them as of WS3b:
//! the browser bundle builds under `-Ctarget-feature=+atomics,+bulk-memory,
//! +mutable-globals` with `-Zbuild-std`, and `rustdar-web` fills the pool with
//! Web Workers over one shared linear memory (`wasm-bindgen-rayon`). Both arms
//! now compile from identical source.
//!
//! The stand-ins are gone rather than kept as a degradation path, because the
//! degradation now lives where it belongs — in the *pool*, not in the iterator.
//! A browser that cannot give rayon its threads (no `SharedArrayBuffer`, no
//! nested Workers, or a deployment served without COOP/COEP) still reaches this
//! same `par_iter`: `rustdar_web::rayon_pool::install_serial_pool` hands it a
//! global pool of one thread — the caller's — so the work runs inline at
//! exactly the speed `seq` ran it. Nothing here reads a `cfg`.
//!
//! **Every caller of this module owes rayon a built global pool.** rayon panics
//! rather than falling back when `build_global` never ran and cannot run, and
//! on wasm32-unknown-unknown it cannot: `std::thread::spawn` is `Unsupported`
//! there. Native gets its pool from rayon's own lazy default; the two browser
//! threads that reach this — the page and the rasterization worker — get theirs
//! from `rustdar_web::rayon_pool`.

pub(crate) use rayon::prelude::*;
