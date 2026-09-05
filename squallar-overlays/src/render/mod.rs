pub mod controls;
pub mod draw;
pub mod footprint;
pub mod geo;
pub mod gridded;
// `pub` so the frontend's described-job dispatch tests can name the three
// polygon kinds' fetch-result types — see `handlers`'s own module comment.
pub mod handlers;
mod hatch;
pub mod jobs;
pub mod overlay_state;
pub mod rasterize;
mod signature_memo;
pub mod station_model;
