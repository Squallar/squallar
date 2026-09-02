#![warn(clippy::all)]
#![forbid(unsafe_code)]

//! The device-class policy floor: what class of machine this is and what it
//! may spend. Two kinds of statement live here:
//!
//! * the `cfg` cascades in [`constants`] and [`quality`] — per-target figures
//!   selected at compile time, answering *which APIs exist* rather than *what
//!   the machine is*;
//! * the runtime resolver in [`budget`] — a pure function from a
//!   [`budget::DeviceProfile`] to one immutable [`budget::Budgets`], with no
//!   `cfg!` in its body, so every configuration is reachable from a host test;
//! * the need / capacity arithmetic in [`scene`] and [`fit`] — what the scene
//!   on screen costs at a budget, and the largest budget whose cost fits the
//!   device's capacity, shedding down the resolver's own ladder. Pure, and
//!   priced only through cost functions the tree already has.
//! * the fixed-shape latency histogram in [`hist`] — pure integer arithmetic
//!   over a compile-time bin layout. It sits here, under both the UI and the
//!   wgpu boundary, because both sides read one: the recorder fills on the
//!   frame thread and the diagnostics panel diffs snapshots of it, and this is
//!   the one crate both already stand on without a cycle.
//!
//! Data and policy only, denominated in `squallar-radar`'s size vocabulary.
//! Nothing here renders, allocates, or touches a device.

pub mod budget;
pub mod constants;
pub mod fit;
pub mod hist;
pub mod linear_memory;
/// The rule behind the `mobile` cfg. Compiled only for tests — the production
/// copy is `include!`d by `build.rs`.
#[cfg(test)]
mod mobile_cfg;
pub mod quality;
pub mod scene;
