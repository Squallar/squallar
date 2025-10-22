#![warn(clippy::all)]
#![forbid(unsafe_code)]

use rustdar_platform_lib::run;

fn main() {
    pollster::block_on(run());
}
