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
//!   `cfg!` in its body, so every configuration is reachable from a host test.
//!
//! Data and policy only, denominated in `squallar-radar`'s size vocabulary.
//! Nothing here renders, allocates, or touches a device.

pub mod budget;
pub mod constants;
/// The rule behind the `mobile` cfg. Compiled only for tests — the production
/// copy is `include!`d by `build.rs`.
#[cfg(test)]
mod mobile_cfg;
pub mod quality;
