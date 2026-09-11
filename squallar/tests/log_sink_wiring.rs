//! The installed sink really takes the records the logger is given.
//!
//! **A fires-counter, not a unit test.** `log_sink`'s own suite drives a
//! `Sink` it built itself, which proves the machinery and proves nothing about
//! whether anything is wired to it. This installs the sink the way
//! `run::run` installs it, logs through the global `log` macros, and reads the
//! counter: a cut whose precondition never holds delivers exactly nothing, and
//! a zero here is what says so.
//!
//! Its own test binary because `log::set_logger` succeeds once per process.

use std::time::Duration;

const LINES: usize = 115;

#[test]
fn the_installed_sink_takes_every_record_the_global_logger_is_given() {
    let mut builder = env_logger::Builder::new();
    builder.filter_level(log::LevelFilter::Info);
    squallar_native::log_sink::install(&mut builder)
        .try_init()
        .expect("no logger is installed in this test binary");

    assert!(
        squallar_native::log_sink::counters().is_some(),
        "install did not record a sink to read"
    );
    let before = squallar_native::log_sink::counters()
        .expect("just asserted")
        .enqueued;

    for i in 0..LINES {
        log::info!("log sink wiring probe {i}");
    }
    assert!(
        squallar_native::log_sink::drain(Duration::from_secs(10)),
        "the writer thread did not empty the queue"
    );

    let c = squallar_native::log_sink::counters().expect("just asserted");
    assert_eq!(
        c.enqueued - before,
        LINES as u64,
        "the global logger handed the sink {} of {LINES} records",
        c.enqueued - before,
    );
    assert_eq!(
        c.written, c.enqueued,
        "a record was queued and never written"
    );
    assert_eq!(c.dropped, 0, "records were dropped on an idle queue");
    assert_eq!(
        c.fallback_writes, 0,
        "the producer wrote {} record(s) itself, which is a write(2) back on \
         the caller's thread",
        c.fallback_writes,
    );
    assert!(c.peak_depth >= 1, "nothing was ever queued at all");

    let line = squallar_native::log_sink::sink_line().expect("install ran");
    assert!(line.starts_with("log sink: "), "{line}");
}
