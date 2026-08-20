pub mod controls;
pub mod draw;
pub mod geo;
// `pub` so the frontend's described-job dispatch tests can name the three
// polygon kinds' fetch-result types — see `handlers`'s own module comment.
pub mod handlers;
mod hatch;
pub mod jobs;
pub mod overlay_state;
pub mod rasterize;
pub mod station_model;
