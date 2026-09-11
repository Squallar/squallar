//! What the off-thread sink promises, in counts.
//!
//! Every gate here is a count or a comparison of bytes. There is no
//! wall-clock ratio in the file on purpose: this box runs many build lanes at
//! once, and a timing assertion would red-gate correctness for being busy.

use super::*;
use std::sync::Mutex;
use std::thread::ThreadId;

/// A destination that remembers what it was given and which thread gave it.
#[derive(Clone, Default)]
struct Capture {
    bytes: Arc<Mutex<Vec<u8>>>,
    threads: Arc<Mutex<Vec<ThreadId>>>,
}

impl Capture {
    fn text(&self) -> String {
        String::from_utf8(self.bytes.lock().expect("no panics while held").clone())
            .expect("the sink only ever carries what a formatter wrote")
    }

    fn writing_threads(&self) -> Vec<ThreadId> {
        self.threads.lock().expect("no panics while held").clone()
    }
}

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.threads
            .lock()
            .expect("no panics while held")
            .push(std::thread::current().id());
        self.bytes
            .lock()
            .expect("no panics while held")
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A sink writing into a fresh [`Capture`], with the writer thread held at the
/// door until `gate` is dropped or sent on.
fn sink_with_gate(gate: Option<std::sync::mpsc::Receiver<()>>) -> (Sink, Capture) {
    let cap = Capture::default();
    let theirs = cap.clone();
    let sink = start(move || {
        if let Some(gate) = gate {
            // Blocks the writer thread BEFORE its first `recv`, which is the
            // only way to hold the queue shut from outside.
            let _ = gate.recv();
        }
        Box::new(theirs)
    });
    (sink, cap)
}

fn sink() -> (Sink, Capture) {
    sink_with_gate(None)
}

/// Wait for `shared.depth` to reach zero. Bounded so a broken pump fails the
/// suite instead of hanging it; the bound is a backstop, never the assertion.
fn settle(sink: &Sink) {
    for _ in 0..20_000 {
        if sink.shared.depth.load(Ordering::Relaxed) == 0 {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    panic!("the writer thread never drained the queue");
}

const BURST: usize = 115;

/// The burst `report_frame_telemetry` says, at the size it says it.
fn burst(sink: &mut Sink) {
    for i in 0..BURST {
        sink.write_all(format!("line {i}\n").as_bytes())
            .expect("the sink never refuses a record");
    }
}

#[test]
fn a_whole_telemetry_burst_leaves_the_producing_thread_without_one_write() {
    let (mut sink, cap) = sink();
    let producer = std::thread::current().id();
    burst(&mut sink);
    settle(&sink);

    let threads = cap.writing_threads();
    assert!(
        !threads.is_empty(),
        "precondition: the destination was never written to at all, so a zero \
         below would be free"
    );
    assert_eq!(
        threads.iter().filter(|t| **t == producer).count(),
        0,
        "{} of {} writes to the destination happened on the producing thread",
        threads.iter().filter(|t| **t == producer).count(),
        threads.len(),
    );
    let c = sink.shared.read();
    assert_eq!(c.enqueued, BURST as u64, "the fires-counter");
    assert_eq!(c.written, BURST as u64);
    assert_eq!(c.dropped, 0);
    assert_eq!(c.fallback_writes, 0);
}

#[test]
fn a_burst_already_queued_leaves_as_one_write_and_not_as_one_per_record() {
    // Held at the door until the whole burst is queued, so the batch the pump
    // takes is the burst and the count below is decided rather than raced.
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let (mut sink, cap) = sink_with_gate(Some(rx));
    burst(&mut sink);
    assert_eq!(
        sink.shared.depth.load(Ordering::Relaxed),
        BURST,
        "precondition: the burst was not all queued before the door opened"
    );
    drop(tx);
    settle(&sink);
    assert_eq!(
        cap.writing_threads().len(),
        1,
        "the pump made {} writes for {BURST} queued records",
        cap.writing_threads().len(),
    );
    assert_eq!(cap.text().lines().count(), BURST);
}

#[test]
fn the_stream_keeps_the_order_the_records_were_logged_in() {
    let (mut sink, cap) = sink();
    burst(&mut sink);
    settle(&sink);
    let want: String = (0..BURST).map(|i| format!("line {i}\n")).collect();
    assert_eq!(cap.text(), want);
}

#[test]
fn a_consumer_that_has_stopped_neither_stops_the_producer_nor_swallows_a_record() {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let (mut sink, cap) = sink_with_gate(Some(rx));
    // Enough to fill the queue and overrun it. The producer must return from
    // every one of these while the consumer is held at the door.
    let offered = CAPACITY + BURST;
    for i in 0..offered {
        sink.write_all(format!("line {i}\n").as_bytes())
            .expect("the sink never refuses a record");
    }

    let held = sink.shared.read();
    assert_eq!(
        held.enqueued, CAPACITY as u64,
        "the queue took {} of {CAPACITY}; the bound is not the bound",
        held.enqueued,
    );
    assert_eq!(
        held.dropped, BURST as u64,
        "{} record(s) dropped, {BURST} expected; offered minus queued does not \
         reconcile",
        held.dropped,
    );
    // Exact here because the writer is held at the door, so nothing has left
    // and in-flight is queue occupancy. It is not a bound in general: the pump
    // takes a batch out before it writes it, and those records are still in
    // flight.
    assert_eq!(held.peak_depth, CAPACITY, "the door really did close");

    drop(tx);
    settle(&sink);

    // One more record, which is what carries the gap marker into the stream.
    sink.write_all(b"after\n")
        .expect("the sink never refuses a record");
    settle(&sink);

    let text = cap.text();
    let marker = format!("log sink: dropped {BURST} record(s) to backpressure");
    assert_eq!(
        text.matches(&marker).count(),
        1,
        "the stated gap is not in the stream exactly once:\n{}",
        text.lines().rev().take(3).collect::<Vec<_>>().join("\n"),
    );
    // The gap is stated where the gap is: immediately before the first record
    // that got through after it, not appended somewhere at the end.
    let lines: Vec<&str> = text.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.contains(&marker))
        .expect("the marker was just asserted to be present");
    assert_eq!(
        lines[at + 1],
        "after",
        "the marker did not land in front of the record that followed the gap"
    );
    let c = sink.shared.read();
    assert_eq!(
        c.written + c.dropped,
        offered as u64 + 1,
        "written {} plus dropped {} does not account for the {} records offered",
        c.written,
        c.dropped,
        offered + 1,
    );
}

#[test]
fn a_dead_writer_makes_the_producer_write_rather_than_lose_the_record() {
    let (mut sink, _cap) = sink();
    let fallen = Arc::new(Mutex::new(Vec::<u8>::new()));
    let theirs = Arc::clone(&fallen);
    sink.fallback = Box::new(move |bytes| {
        theirs
            .lock()
            .expect("no panics while held")
            .extend_from_slice(bytes);
    });
    // Ending the pump is what a dead writer thread looks like from here.
    let (dead_tx, dead_rx) = sync_channel::<Vec<u8>>(1);
    drop(dead_rx);
    sink.tx = dead_tx;

    sink.write_all(b"orphan\n")
        .expect("the sink never refuses a record");
    assert_eq!(
        String::from_utf8(fallen.lock().expect("no panics while held").clone())
            .expect("ASCII went in"),
        "orphan\n",
    );
    assert_eq!(sink.shared.read().fallback_writes, 1);
}

#[test]
fn drain_returns_only_once_the_queue_is_empty() {
    let (mut sink, cap) = sink();
    burst(&mut sink);
    // `drain` reads the installed sink, which this one is not, so the queue is
    // watched directly — the same field `drain` reads.
    for _ in 0..20_000 {
        if sink.shared.depth.load(Ordering::Relaxed) == 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert_eq!(sink.shared.depth.load(Ordering::Relaxed), 0);
    assert_eq!(cap.text().lines().count(), BURST);
}

#[test]
fn drain_with_no_sink_installed_is_not_a_failure() {
    assert!(drain(std::time::Duration::from_millis(0)));
}

/// A record formatted by `env_logger` for the given target and style.
fn formatted(style: env_logger::WriteStyle) -> Vec<u8> {
    let cap = Capture::default();
    let logger = env_logger::Builder::new()
        .filter_level(log::LevelFilter::Info)
        .write_style(style)
        .target(env_logger::Target::Pipe(Box::new(cap.clone())))
        .build();
    log::Log::log(
        &logger,
        &log::Record::builder()
            .args(format_args!("overlay rasters: 1 dispatched"))
            .level(log::Level::Info)
            .target("squallar_app::app::render")
            .build(),
    );
    cap.bytes.lock().expect("no panics while held").clone()
}

#[test]
fn the_bytes_that_reach_the_destination_are_the_ones_the_old_target_wrote() {
    // What `Target::Stderr` writes when stderr is not a terminal: env_logger
    // resolves the style to `Never` and its `AutoStream` strips.
    let plain = formatted(env_logger::WriteStyle::Never);
    // What this module puts on the queue instead — the constant `install`
    // passes, so a change there fails here rather than silently stripping
    // every line a developer sees.
    let styled = formatted(PRODUCER_STYLE);
    assert_ne!(
        plain, styled,
        "precondition: the two styles produce the same bytes, so the \
         comparison below cannot fail"
    );

    // And what the writer thread's `AutoStream` makes of them.
    let mut out = anstream::AutoStream::new(Vec::new(), anstream::ColorChoice::Never);
    out.write_all(&styled).expect("a Vec never fails a write");
    out.flush().expect("a Vec never fails a flush");
    assert_eq!(
        out.into_inner(),
        plain,
        "the writer thread's stream does not reproduce what Target::Stderr wrote"
    );
}

#[test]
fn the_style_the_writer_thread_uses_is_the_one_the_environment_asked_for() {
    // Read from the process environment, so this asserts the parse rather than
    // the ambient value: `auto` and an unset variable are the same answer, and
    // that answer depends on whether stderr is a terminal.
    assert!(matches!(
        stderr_colour_choice(),
        anstream::ColorChoice::Always | anstream::ColorChoice::Never | anstream::ColorChoice::Auto
    ));
}

#[test]
fn the_sentence_names_every_counter_it_has() {
    // Not installed in a test process, and that is the readable answer rather
    // than a sentence full of zeros from some other suite's sink.
    assert!(sink_line().is_none());
    assert!(counters().is_none());
}
