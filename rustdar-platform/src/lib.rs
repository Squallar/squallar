#![warn(clippy::all)]
#![forbid(unsafe_code)]

//! Desktop and Android entry points.
//!
//! The portable application lives in `rustdar-frontend`. What is left here is
//! the part that cannot be portable: the event loop bootstrap, and the concrete
//! [`platform::PlatformBridge`] implementations plus the filesystem-backed
//! config store they hand to it.

pub mod config_store;
pub mod platform;
pub mod run;

pub use crate::run::run;
