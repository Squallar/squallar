#![warn(clippy::all)]
#![forbid(unsafe_code)]

//! The basemap row: the committed vector style, and the wire that carries a
//! styled tile off the thread that styled it.
//!
//! On native a vector tile is parsed and tessellated on the IO runtime's
//! blocking pool and the frame side only moves a finished tile into a cache.
//! On wasm32 there is no second thread in `squallar-egui` to pay it on, so the
//! frame pump paid it -- `take:vector` p99 9.5-22.6 ms against a 4 ms bar.
//! This crate is what lets the browser pay it where the radar rasterizer is
//! already paid: in the worker.
//!
//! Three modules, and the split is the boundary itself:
//!
//! * [`style`] is the committed style, moved here from `squallar-egui` so that
//!   the ~222 KB of compiled-in JSON exists ONCE. The worker runs the same wasm
//!   module the page does, so it already has these bytes; nothing about a style
//!   crosses the wire but `(is_dark, disabled)`.
//! * [`wire`] is the codec for `Vec<walkers::ShapeOrText>` -- four tags, and it
//!   REFUSES anything else rather than dropping it.
//! * [`jobs`] is the registry row over the two.

pub mod jobs;
pub mod style;
pub mod wire;
