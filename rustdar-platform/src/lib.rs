#![warn(clippy::all)]
#![forbid(unsafe_code)]

//! Desktop and Android entry points.
//!
//! The portable application lives in `rustdar-frontend`. What is left here is
//! the part that cannot be portable: the event loop bootstrap, and the concrete
//! [`platform::PlatformBridge`] implementations plus the filesystem-backed
//! config store they hand to it.

pub mod config_store;
/// Test-only: pins the Android Network Security Config to the origins rustdar
/// actually fetches from. See the module docs for why it lives in this crate.
pub mod network_security_config;
pub mod platform;
pub mod run;

pub use crate::run::run;
