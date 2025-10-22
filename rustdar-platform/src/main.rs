#![warn(clippy::all)]
#![forbid(unsafe_code)]

use rustdar_platform_lib::run;

fn main() {
    if let Err(e) = pollster::block_on(run()) {
        eprintln!("Application error: {}", e);
        std::process::exit(1);
    }
}
