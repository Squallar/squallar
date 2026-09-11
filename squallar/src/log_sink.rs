//! The log write, moved off whatever thread produced the record.
//!
//! **Why this exists, measured.** `App::report_frame_telemetry` is gated on a
//! 2 s tick and then says its whole family in one burst: **115 records, 34,514
//! B, in one frame**, counted with `strace -f -c -e trace=write` over 200
//! ticks (23,000 writes, one per record, exactly). With the shipped
//! `Target::Stderr` every one of those is a blocking `write(2)` on the frame
//! thread, so the cost of that frame is set by *whatever is reading the other
//! end* — and that reader is not ours. Release build, two-pane headless
//! fixture, one `report_frame_telemetry` call as the denominator:
//!
//!   destination                     cost of the tick frame
//!   pipe, drained immediately          252 us
//!   regular file                       292 us
//!   pty                                417 us
//!   pipe drained at ~2 MB/s         17,193 us
//!
//! The last row is the reason for this module. 34,514 B at 2 MB/s is 17.2 ms
//! and the frame thread pays all of it: the burst is far bigger than a pipe
//! buffer, so the producer is paced by the consumer with no bound anywhere. A
//! terminal emulator, an `ssh` session or a CI log collector is exactly that
//! consumer, and that is the configuration every measurement leg of this
//! campaign runs in. It is not a slow path that shows up in a mean; it is an
//! unbounded stall on one frame in ~120 whose size nothing in this process
//! decides.
//!
//! **What moves.** The producer formats the record — `env_logger`'s own
//! formatter, unchanged — and hands the bytes to a queue. The writer thread
//! does the ANSI resolution and the `write(2)`. On the same fixture the
//! on-thread half of the emission falls from ~138 us to ~52 us, and, the
//! point, stops depending on the reader at all.
//!
//! **Colour is preserved rather than lost.** `env_logger` resolves
//! `WriteStyle::Auto` by looking at its target, and a `Target::Pipe` is not a
//! terminal, so the naive move silently strips colour from every line a
//! developer sees. Instead the builder is told `Always` and the writer thread
//! wraps the real `stderr` in the same `anstream::AutoStream` that
//! `Target::Stderr` would have used, with the same choice — `RUST_LOG_STYLE`,
//! else `AutoStream::choice(&stderr)`. What reaches fd 2 is what reached it
//! before.
//!
//! **Nothing is reordered.** `env_logger` holds one mutex across the whole of
//! its `print`, so records enter the queue in the order they were logged, and
//! one consumer takes them out in that order. Several rig probes parse these
//! lines by family.
//!
//! **What happens under backpressure, stated.** The queue is bounded at
//! [`CAPACITY`] records and the producer **never** waits on it: a bounded
//! channel whose send blocks has moved the syscall, not removed it. A record
//! offered to a full queue is therefore **dropped**, and the drop is counted
//! and then said in the stream, in position, as
//! `log sink: dropped N record(s) to backpressure`. A consumer that parses a
//! family then reads a stated gap instead of an absence, which is the
//! difference between a missing measurement and a wrong one.

use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};

/// How many records may be queued before a record is dropped rather than
/// waited on.
///
/// The bound is in records because the drop rule is per record. At the 300 B
/// mean of a telemetry line — 34,514 B over 115 records, measured — 4,096
/// records is ~1.2 MB of worst-case backlog, and one telemetry tick is 115 of
/// them: a consumer has to stop dead for **35 ticks, over a minute**, before
/// the first record is dropped. Sized against that and not against taste. The
/// queue exists to absorb one 34 KB burst against a reader that drains between
/// ticks; 35 whole bursts of headroom is already far past the shape it is for,
/// and 1.2 MB is not free on a process whose resident budget is argued in
/// single MB.
pub const CAPACITY: usize = 4096;

/// What one sink has done. Shared by the producer half and its writer thread,
/// and **not process-global**: a static would make one suite's figures the sum
/// of its own and whatever a neighbouring test logged while it ran.
#[derive(Debug, Default)]
struct Shared {
    /// Records handed to the queue. **The fires-counter.** A cut whose
    /// precondition never holds delivers nothing; a zero here says this sink
    /// was built and never used.
    enqueued: AtomicU64,
    /// Records the writer thread has written.
    written: AtomicU64,
    /// Records dropped to a full queue, cumulative, including the ones a gap
    /// marker has already reported.
    dropped: AtomicU64,
    /// Drops not yet said in the stream.
    unsaid_drops: AtomicU64,
    /// Records handed over and not yet written out.
    ///
    /// **Records in flight, which is wider than records in the channel**, and
    /// deliberately: the pump takes a whole batch out of the channel before it
    /// writes any of it, and a record that is in the pump's hands is not yet
    /// anywhere a reader can see. [`drain`] has to wait for those too, so this
    /// falls only once the bytes are out — which also means it can read above
    /// [`CAPACITY`] while a batch is being written.
    depth: AtomicUsize,
    /// The high-water mark of `depth`. **A zero here is not evidence the door
    /// cannot close** — it is evidence about one run, which is why the suite
    /// closes the door by hand rather than reading this and concluding.
    peak_depth: AtomicUsize,
    /// Records the producer wrote itself because the writer thread was gone.
    /// The only path here that can perform a `write(2)` on a caller's thread;
    /// it exists so a dead writer loses nothing, and it should read 0.
    fallback_writes: AtomicU64,
}

/// What the sink has done, for a caller that wants to say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counters {
    /// Records handed to the queue.
    pub enqueued: u64,
    /// Records written by the writer thread.
    pub written: u64,
    /// Records dropped to a full queue.
    pub dropped: u64,
    /// Records handed over and not yet written out.
    pub depth: usize,
    /// The most that were ever in flight at once.
    pub peak_depth: usize,
    /// Records the producer had to write itself.
    pub fallback_writes: u64,
}

impl Shared {
    fn read(&self) -> Counters {
        Counters {
            enqueued: self.enqueued.load(Ordering::Relaxed),
            written: self.written.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
            depth: self.depth.load(Ordering::Relaxed),
            peak_depth: self.peak_depth.load(Ordering::Relaxed),
            fallback_writes: self.fallback_writes.load(Ordering::Relaxed),
        }
    }
}

/// The sink [`install`] handed to `env_logger`, so the process can read its
/// counters and drain it on the way out. `None` until then.
static INSTALLED: std::sync::OnceLock<Arc<Shared>> = std::sync::OnceLock::new();

/// What the installed sink has done, or `None` where [`install`] never ran.
pub fn counters() -> Option<Counters> {
    INSTALLED.get().map(|s| s.read())
}

/// The one sentence this module says about itself, or `None` where
/// [`install`] never ran.
pub fn sink_line() -> Option<String> {
    let c = counters()?;
    Some(format!(
        "log sink: {} enqueued, {} written, {} dropped of a {CAPACITY}-record queue; in flight {}, peak {}; {} written by the producer",
        c.enqueued, c.written, c.dropped, c.depth, c.peak_depth, c.fallback_writes,
    ))
}

/// The producer half: an [`io::Write`] to hand to `env_logger` as its target.
///
/// Its `write` is one `Vec` of the record's bytes and one non-blocking offer
/// to the queue. Its `flush` is a no-op **on purpose**: `env_logger` flushes
/// its target after every single record, so a flush that waited on the writer
/// thread would put the wait back on the caller once per line. Draining is
/// [`drain`], which the process calls when it is leaving.
/// Where a record goes when the writer thread is gone.
type Fallback = Box<dyn Fn(&[u8]) + Send>;

pub struct Sink {
    tx: SyncSender<Vec<u8>>,
    shared: Arc<Shared>,
    /// Owned rather than reached through `io::stderr()` so a suite can point
    /// it somewhere it can read back.
    fallback: Fallback,
}

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // Read before it is taken: on the path this is on 4,096 times out of
        // 4,096 there is nothing to say, and a load is not a read-modify-write.
        let unsaid = match self.shared.unsaid_drops.load(Ordering::Relaxed) {
            0 => 0,
            _ => self.shared.unsaid_drops.swap(0, Ordering::Relaxed),
        };
        let msg = if unsaid == 0 {
            buf.to_vec()
        } else {
            // Prepended to the record rather than offered as a message of its
            // own: one offer cannot half-succeed, so the gap is stated at the
            // position the gap is in even while the queue is still nearly full.
            let mut v =
                format!("log sink: dropped {unsaid} record(s) to backpressure\n").into_bytes();
            v.extend_from_slice(buf);
            v
        };
        // **Counted up before the offer, not after it.** The writer thread can
        // take a record and count it out before a producer that counted it in
        // afterwards has run, and `depth` is unsigned: the order here is what
        // stops the two crossing. An offer that fails puts it straight back.
        let before = self.shared.depth.fetch_add(1, Ordering::Relaxed);
        match self.tx.try_send(msg) {
            Ok(()) => {
                self.shared.enqueued.fetch_add(1, Ordering::Relaxed);
                self.shared
                    .peak_depth
                    .fetch_max(before + 1, Ordering::Relaxed);
            }
            Err(TrySendError::Full(_)) => {
                self.shared.depth.fetch_sub(1, Ordering::Relaxed);
                // This record, and the ones whose marker went down with it.
                self.shared.dropped.fetch_add(1, Ordering::Relaxed);
                self.shared
                    .unsaid_drops
                    .fetch_add(unsaid + 1, Ordering::Relaxed);
            }
            Err(TrySendError::Disconnected(msg)) => {
                self.shared.depth.fetch_sub(1, Ordering::Relaxed);
                // The writer thread is gone. Correctness before latency: write
                // it here rather than lose it, and count that it happened.
                self.shared.fallback_writes.fetch_add(1, Ordering::Relaxed);
                (self.fallback)(&msg);
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Drive `rx` into `out` until the last [`Sink`] is dropped.
///
/// Takes everything already queued before writing, so a 115-record burst
/// leaves as a handful of `write(2)`s rather than 115 of them. `depth` falls
/// only once the bytes are out, which is what makes [`drain`] mean what it
/// says.
fn pump(rx: &Receiver<Vec<u8>>, out: &mut dyn Write, shared: &Shared) {
    let mut batch: Vec<Vec<u8>> = Vec::new();
    let mut bytes: Vec<u8> = Vec::new();
    while let Ok(first) = rx.recv() {
        batch.push(first);
        batch.extend(rx.try_iter());
        bytes.clear();
        for msg in &batch {
            bytes.extend_from_slice(msg);
        }
        let _ = out.write_all(&bytes);
        let _ = out.flush();
        shared
            .written
            .fetch_add(batch.len() as u64, Ordering::Relaxed);
        shared.depth.fetch_sub(batch.len(), Ordering::Relaxed);
        batch.clear();
    }
}

/// Wait for the installed sink's queue to empty, or give up after `timeout`.
///
/// Polled rather than signalled: this is called when the process is leaving,
/// once, so a condvar would be machinery for one call. `true` where the queue
/// emptied — including where there is no sink to drain.
pub fn drain(timeout: std::time::Duration) -> bool {
    let Some(shared) = INSTALLED.get() else {
        return true;
    };
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if shared.depth.load(Ordering::Relaxed) == 0 {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// What `env_logger` would have resolved `WriteStyle::Auto` to for
/// `Target::Stderr`, taken here because a `Target::Pipe` cannot resolve it.
///
/// `RUST_LOG_STYLE` outranks the terminal check and is read the way
/// `env_logger::Env` reads it: `always`, `never`, and anything else — `auto`
/// and an unset variable included — is the automatic decision.
fn stderr_colour_choice() -> anstream::ColorChoice {
    match std::env::var("RUST_LOG_STYLE").as_deref() {
        Ok("always") => anstream::ColorChoice::Always,
        Ok("never") => anstream::ColorChoice::Never,
        _ => anstream::AutoStream::choice(&io::stderr()),
    }
}

/// The style the producer must format in.
///
/// **`Always`, and that is what makes the move invisible.** `env_logger`
/// resolves `Auto` against its target, and a `Target::Pipe` is never a
/// terminal, so `Auto` here would resolve to `Never` and strip the colour out
/// of every line before the writer thread could decide. Formatting styled and
/// resolving on the writer thread is what leaves fd 2 seeing what
/// `Target::Stderr` showed it.
const PRODUCER_STYLE: env_logger::WriteStyle = env_logger::WriteStyle::Always;

/// Point `builder` at a writer thread, and start it.
///
/// The builder is told `WriteStyle::Always` so the ANSI resolution — the half
/// of `Target::Stderr`'s cost that is not the syscall — happens on the writer
/// thread's `AutoStream` rather than on the caller's. That stream is built
/// with [`stderr_colour_choice`], which is the choice `Target::Stderr` would
/// have made.
pub fn install(builder: &mut env_logger::Builder) -> &mut env_logger::Builder {
    let choice = stderr_colour_choice();
    let sink = start(move || Box::new(anstream::AutoStream::new(io::stderr(), choice)));
    let _ = INSTALLED.set(Arc::clone(&sink.shared));
    builder
        .write_style(PRODUCER_STYLE)
        .target(env_logger::Target::Pipe(Box::new(sink)))
}

/// Spawn the writer thread and hand back the producer half.
///
/// `out` is called once, on the writer thread, to build the destination.
fn start<F>(out: F) -> Sink
where
    F: FnOnce() -> Box<dyn Write + Send> + Send + 'static,
{
    let (tx, rx) = sync_channel::<Vec<u8>>(CAPACITY);
    let shared = Arc::new(Shared::default());
    let theirs = Arc::clone(&shared);
    std::thread::Builder::new()
        .name("squallar-log".into())
        .spawn(move || {
            let mut out = out();
            pump(&rx, out.as_mut(), &theirs);
        })
        .expect("a log writer thread");
    Sink {
        tx,
        shared,
        fallback: Box::new(|bytes| {
            let _ = io::stderr().write_all(bytes);
        }),
    }
}

#[cfg(test)]
#[path = "log_sink_tests.rs"]
mod log_sink_tests;
