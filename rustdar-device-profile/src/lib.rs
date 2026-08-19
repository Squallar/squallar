#![warn(clippy::all)]
#![forbid(unsafe_code)]

//! The device-class policy floor: what class of machine this is and what it
//! may spend.
//!
//! Two kinds of statement live here, and keeping them distinct is the crate's
//! whole design (see [`budget`]'s module doc, where it is argued at length):
//!
//! * **the `cfg` cascades** in [`constants`] and [`quality`] — per-target
//!   figures selected at compile time. A `cfg` answers with the only fact
//!   available at compile time, which is **which APIs exist**, not **what the
//!   machine is**: a native Android build and a native desktop build differ in
//!   what the bracket may safely be, and one wasm binary serves a phone
//!   browser and a workstation browser alike.
//! * **the runtime resolver** in [`budget`] — a pure function from a
//!   [`budget::DeviceProfile`] (what the machine said about itself) to one
//!   immutable [`budget::Budgets`], with no `cfg!` in its body, so every
//!   shipped configuration and every synthetic one is reachable from a single
//!   host test run.
//!
//! Everything here is data and policy, denominated in `rustdar-radar`'s size
//! vocabulary (`VoxelShape`, the wire/raster ceilings). Nothing here renders,
//! allocates, or touches a device: the crates above — the GPU stack, the
//! worker, the app shell — read these figures and spend them.

pub mod budget;
pub mod constants;
/// The rule behind the `mobile` cfg. Compiled only for tests — the production
/// copy is `include!`d by `build.rs`, which runs before this crate exists.
#[cfg(test)]
mod mobile_cfg;
pub mod quality;
