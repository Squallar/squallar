use clap::Parser;

/// Command-line configuration for the application.
/// Currently empty but can be extended with CLI arguments in the future.
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
pub struct Config {}
