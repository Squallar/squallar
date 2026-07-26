#![warn(clippy::all)]
// See the note in lib.rs. Nothing in this file is unsafe; it matches lib.rs so
// the two halves of the crate carry the same rule.
#![deny(unsafe_code)]

use rustdar_platform_lib::run;

fn main() {
    if let Err(e) = pollster::block_on(run()) {
        eprintln!("Application error: {}", e);
        std::process::exit(1);
    }
}
