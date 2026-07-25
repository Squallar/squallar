#![warn(clippy::all)]
#![forbid(unsafe_code)]

//! Desktop and Android entry points: the event loop bootstrap and the concrete
//! [`platform::PlatformBridge`] implementations. The portable application lives
//! in `rustdar-frontend`.

pub mod config_store;
/// Test-only. See the module docs for why it lives in this crate.
pub mod network_security_config;
pub mod platform;
pub mod run;

pub use crate::run::run;
