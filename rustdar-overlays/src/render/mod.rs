pub mod controls;
pub mod draw;
pub mod geo;
// `pub` so the frontend's described-job dispatch tests can name the three
// polygon kinds' fetch-result types — see `handlers`'s own module comment.
pub mod handlers;
mod hatch;
// The seven overlay codec rows of the job boundary, beside the rasterizers
// they run (WO-M6.2). Until WO-M6.3 flips the frontend onto `jobs::JOB_CODECS`,
// nothing routes through it.
pub mod jobs;
pub mod overlay_state;
pub mod rasterize;
pub mod station_model;
