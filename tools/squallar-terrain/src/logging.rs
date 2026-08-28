use std::sync::OnceLock;
use std::time::Instant;

static START: OnceLock<Instant> = OnceLock::new();

/// Elapsed since process start, not wall clock: turning a UNIX timestamp into
/// local time needs a tz database, and a build measured in hours is read by
/// duration anyway.
pub fn stamp() -> String {
    let s = START.get_or_init(Instant::now).elapsed().as_secs();
    format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

/// Start the clock, so the first line is not also the thing that zeroes it.
pub fn init() {
    START.get_or_init(Instant::now);
}

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        eprintln!("[{}] {}", $crate::logging::stamp(), format_args!($($arg)*))
    };
}
