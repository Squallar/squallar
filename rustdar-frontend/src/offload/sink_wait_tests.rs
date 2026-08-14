//! The wait for a sink, on the host.
//!
//! The queue these tests drive is the same one a browser feeds — the `cfg` in
//! this module is over routing, not over whether the queue exists — so
//! everything here except the last test is a claim about what a wasm frame will
//! observe when its rasterization worker is starting or has just been lost.
//!
//! Two things shape every fixture below. [`SINK_WAIT`] and [`WORKER`] are
//! **thread-locals** and the harness reuses its threads, so each test takes a
//! [`Fixture`] guard that puts both back on the way out, including on an
//! unwind. And "runs here" is asynchronous on this target — `run_here` spawns
//! a thread for the job natively, and only runs it on the frame in a browser
//! — so a job that ran is asserted through a channel with a timeout, while a
//! job that is *held* is asserted synchronously, because nothing ran.

use super::*;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

/// Long enough that no test below is racing a real clock. Every deadline that
/// has to *lapse* is made to lapse by re-arming at [`Duration::ZERO`] rather
/// than by sleeping, so nothing here is a wall-clock assertion.
const AMPLE: Duration = Duration::from_secs(30);

/// What a `WaitPort` recorded: the job ids and the bytes it was handed, in the
/// order it was handed them. Bytes for [`tests::FakePort`]'s reason — the
/// browser's sink serialises on its way out, so recording what would have been
/// posted keeps the codec on the path these tests walk — and here the bytes are
/// also what tells one held job from another.
type Posted = Arc<std::sync::Mutex<Vec<Vec<u8>>>>;

struct WaitPort {
    posted: Posted,
    accept: bool,
}

impl JobSink for WaitPort {
    fn send(&self, _id: u64, request: JobRequest) -> Result<(), JobRequest> {
        if !self.accept {
            return Err(request);
        }
        self.posted.lock().unwrap().push(request.to_bytes());
        Ok(())
    }
}

/// Install a port and answer what it records.
fn attach(accept: bool) -> Posted {
    let posted: Posted = Arc::new(std::sync::Mutex::new(Vec::new()));
    set_worker(Box::new(WaitPort {
        posted: Arc::clone(&posted),
        accept,
    }));
    posted
}

/// This thread's funnel state, put back on the way out.
struct Fixture;

impl Drop for Fixture {
    fn drop(&mut self) {
        clear_sink_wait();
        abandon_worker("test teardown");
    }
}

/// A thread with no sink and no wait — the state a browser is in the instant
/// before its worker is started, and the one the `NoSink` arm is reached from.
fn fixture() -> Fixture {
    clear_sink_wait();
    abandon_worker("test setup");
    Fixture
}

/// A job that is cheap to run, cheap to serialise, and **tells itself apart
/// from its neighbours** — which the render fixture in [`tests`] cannot, every
/// copy of it being byte-identical.
///
/// A `Decode` of an archive header that no decoder will accept, with `n`
/// appended: `to_bytes` ends in that byte, and `execute` answers the `None` a
/// failed decode has always meant. Ordering and eviction are the subject here,
/// not rasterization, and a decode that fails at the framing is far cheaper
/// than a render — which matters at a queue of `SINK_WAIT_LIMIT`.
///
/// A **plausible** header rather than a lone byte, and the difference is not
/// cosmetic: `scan::decode_bytes` has to be reached and refuse, so that what a
/// held job costs when it finally runs is a refusal rather than whatever a
/// one-byte buffer does on its way through a parser.
fn a_numbered_job(n: u8) -> JobRequest {
    let mut archive = b"AR2V0006.001not-a-real-volume".to_vec();
    archive.push(n);
    JobRequest::Decode {
        archive: Arc::new(archive),
    }
}

/// Which numbered jobs a port was handed, in order.
fn numbers(posted: &Posted) -> Vec<u8> {
    posted
        .lock()
        .unwrap()
        .iter()
        .map(|bytes| *bytes.last().expect("a numbered job carries its byte"))
        .collect()
}

/// Dispatch [`a_numbered_job`], reporting `n` on the channel when — and only
/// when — its `deliver` runs.
fn dispatch(n: u8, delivered: &mpsc::Sender<u8>) {
    let delivered = delivered.clone();
    offload_job(
        "test-sink-wait",
        Job::Described(a_numbered_job(n)),
        move |_| {
            let _ = delivered.send(n);
        },
    );
}

// ── Arming ───────────────────────────────────────────────────────────────────

/// **The behaviour this change must not alter.** A thread that has said nothing
/// about expecting a sink runs the job where it stands, which is what every
/// `NoSink` dispatch did before the queue existed and what every target that
/// installs a sink up front still does.
#[test]
fn with_no_sink_and_nothing_expected_the_job_runs_here() {
    let _fixture = fixture();
    let (tx, rx) = mpsc::channel();
    dispatch(1, &tx);

    assert_eq!(
        rx.recv_timeout(Duration::from_secs(10)),
        Ok(1),
        "an unarmed thread must run the job rather than hold it",
    );
    assert_eq!(jobs_waiting_for_sink(), 0);
    assert!(!expecting_sink());
}

/// The same dispatch, once the thread has said a sink is on its way: held, not
/// run. This is the browser's cold start — the first volume decode arrives
/// while the worker is still instantiating — and the ~1 s of frame time it
/// costs is what waiting buys back.
#[test]
fn with_a_sink_expected_the_job_is_held_instead() {
    let _fixture = fixture();
    expect_sink(AMPLE);
    let (tx, rx) = mpsc::channel();
    dispatch(1, &tx);

    assert_eq!(jobs_waiting_for_sink(), 1, "the job must be held");
    assert!(
        rx.try_recv().is_err(),
        "a held job must not have been run, and must not have been failed",
    );
}

/// Arming is refused while a sink is installed. There is nothing to wait for,
/// and a queue in front of a live transport would delay every job by a window
/// chosen for a handshake that has already happened.
#[test]
fn a_thread_that_has_a_sink_expects_nothing() {
    let _fixture = fixture();
    let posted = attach(true);

    expect_sink(AMPLE);
    assert!(
        !expecting_sink(),
        "a thread with a sink must not arm a wait for one",
    );

    let (tx, _rx) = mpsc::channel();
    dispatch(1, &tx);
    assert_eq!(numbers(&posted), vec![1], "the job must go to the sink");
    assert_eq!(jobs_waiting_for_sink(), 0);
}

// ── The hand-over ────────────────────────────────────────────────────────────

/// The sink that arrives gets everything that was held for it, in the order it
/// was dispatched — the front of the queue first, so no job starves behind a
/// later one.
#[test]
fn the_sink_that_arrives_is_handed_what_was_held_oldest_first() {
    let _fixture = fixture();
    expect_sink(AMPLE);
    let (tx, rx) = mpsc::channel();
    for n in 1..=5 {
        dispatch(n, &tx);
    }
    assert_eq!(jobs_waiting_for_sink(), 5);

    let posted = attach(true);
    assert_eq!(
        numbers(&posted),
        vec![1, 2, 3, 4, 5],
        "the held jobs must reach the new sink, oldest first",
    );
    assert_eq!(jobs_waiting_for_sink(), 0, "the queue must be emptied");
    assert!(
        !expecting_sink(),
        "the sink arrived, so the wait must be over",
    );
    assert!(
        rx.try_recv().is_err(),
        "a job handed to a sink must be waiting on that sink's reply",
    );
}

/// The hand-over goes through the funnel, not around it: a port that refuses
/// what it is given must leave those jobs running here rather than lose them.
///
/// This is the one that says the queue cannot strand a job. A hand-over written
/// as a direct `sink.send` would drop every refusal on the floor, and each one
/// is a render slot and a pane's in-flight mark that nothing would ever
/// release.
#[test]
fn a_sink_that_refuses_what_it_is_handed_leaves_it_running_here() {
    let _fixture = fixture();
    expect_sink(AMPLE);
    let (tx, rx) = mpsc::channel();
    dispatch(1, &tx);
    dispatch(2, &tx);
    assert_eq!(jobs_waiting_for_sink(), 2);

    let posted = attach(false);
    assert!(posted.lock().unwrap().is_empty(), "the port refuses");
    let mut ran = vec![
        rx.recv_timeout(Duration::from_secs(10))
            .expect("the first refused job must still run"),
        rx.recv_timeout(Duration::from_secs(10))
            .expect("the second refused job must still run"),
    ];
    ran.sort_unstable();
    assert_eq!(ran, vec![1, 2]);
    assert_eq!(jobs_waiting_for_sink(), 0);
}

// ── The bound ────────────────────────────────────────────────────────────────

/// `SINK_WAIT_LIMIT` jobs are held; the one after that evicts the oldest, and
/// that caller is answered `None` rather than left waiting.
///
/// `None` is what `abandon_worker` already hands a job whose worker died, and
/// what a render that found no sweep has always meant: the caller's render slot
/// and its in-flight mark are released either way. What each call site then
/// *does* about it differs — a plan-view pane re-dispatches on the next frame,
/// a loop frame is retired, a decode surfaces as an error — and
/// [`wait_for_sink`] is where the three are written out. The queue is bounded
/// so that a sink which never arrives cannot become an unbounded hold on every
/// request the application has made.
#[test]
fn the_job_past_the_bound_evicts_the_oldest_and_that_caller_is_answered() {
    let _fixture = fixture();
    expect_sink(AMPLE);
    let (tx, rx) = mpsc::channel();

    for n in 0..SINK_WAIT_LIMIT {
        dispatch(n as u8, &tx);
    }
    assert_eq!(
        jobs_waiting_for_sink(),
        SINK_WAIT_LIMIT,
        "the bound must be reached without evicting anything",
    );
    assert!(
        rx.try_recv().is_err(),
        "nothing may be given up on at or below the bound",
    );

    dispatch(SINK_WAIT_LIMIT as u8, &tx);
    assert_eq!(
        rx.try_recv(),
        Ok(0),
        "the job past the bound must evict the *oldest*, and answer its caller",
    );
    assert!(
        rx.try_recv().is_err(),
        "exactly one job may be given up on per arrival past the bound",
    );
    assert_eq!(
        jobs_waiting_for_sink(),
        SINK_WAIT_LIMIT,
        "the queue must hold the bound, never the bound plus one",
    );

    // And what is held is the *newest* window, which is the half an eviction
    // that took from the back would get wrong while keeping the count right.
    let posted = attach(true);
    assert_eq!(
        numbers(&posted),
        (1..=SINK_WAIT_LIMIT as u8).collect::<Vec<_>>(),
        "the evicted job must be gone and every later one still held",
    );
}

// ── The deadline ─────────────────────────────────────────────────────────────

/// A lapsed wait runs what it held **here**, which is exactly the behaviour the
/// queue replaced. Expiring is how a browser whose worker never answers gets
/// its pictures at the old price instead of not at all.
///
/// The deadline is made to lapse by re-arming at zero rather than by sleeping:
/// what is under test is the comparison against the deadline, not the clock.
#[test]
fn a_lapsed_wait_runs_what_it_held_here() {
    let _fixture = fixture();
    expect_sink(AMPLE);
    let (tx, rx) = mpsc::channel();
    dispatch(1, &tx);
    dispatch(2, &tx);
    assert_eq!(jobs_waiting_for_sink(), 2);
    assert!(rx.try_recv().is_err(), "nothing has lapsed yet");

    // The wait, re-armed on a deadline already in the past. The jobs it is
    // holding are untouched by the re-arming itself.
    expect_sink(Duration::ZERO);
    assert_eq!(jobs_waiting_for_sink(), 2);

    flush_expired_sink_wait();
    let mut ran = vec![
        rx.recv_timeout(Duration::from_secs(10))
            .expect("the first held job must run"),
        rx.recv_timeout(Duration::from_secs(10))
            .expect("the second held job must run"),
    ];
    ran.sort_unstable();
    assert_eq!(ran, vec![1, 2], "a lapsed wait must not swallow a job");
    assert_eq!(jobs_waiting_for_sink(), 0);
    assert!(!expecting_sink(), "a lapsed wait must disarm");
}

/// A flush against a live deadline does nothing. The browser schedules one
/// timer per attempt, so a flush that ignored the deadline would empty a queue
/// armed by a *later* attempt onto the frame thread it was armed to spare.
#[test]
fn a_flush_against_a_live_deadline_holds_everything() {
    let _fixture = fixture();
    expect_sink(AMPLE);
    let (tx, rx) = mpsc::channel();
    dispatch(1, &tx);

    flush_expired_sink_wait();
    assert_eq!(
        jobs_waiting_for_sink(),
        1,
        "a wait that has not lapsed must keep what it holds",
    );
    assert!(rx.try_recv().is_err());
    assert!(expecting_sink());
}

/// The other way a lapse is noticed: the next job arrives after the deadline,
/// and takes everything held with it rather than joining a queue that has
/// stopped waiting.
#[test]
fn a_job_arriving_after_the_deadline_takes_the_held_jobs_with_it() {
    let _fixture = fixture();
    expect_sink(AMPLE);
    let (tx, rx) = mpsc::channel();
    dispatch(1, &tx);

    expect_sink(Duration::ZERO);
    dispatch(2, &tx);

    let mut ran = vec![
        rx.recv_timeout(Duration::from_secs(10))
            .expect("the held job must run"),
        rx.recv_timeout(Duration::from_secs(10))
            .expect("the arriving job must run"),
    ];
    ran.sort_unstable();
    assert_eq!(ran, vec![1, 2]);
    assert_eq!(jobs_waiting_for_sink(), 0);
    assert!(!expecting_sink());
}

// ── Losing a worker, and getting another ─────────────────────────────────────

/// **The defect, end to end, in the shape the browser meets it.**
///
/// A worker dies mid-session: the jobs it owed are failed so their panes
/// re-ask, and this is where the page used to stop — every later scan, scrub
/// and loop frame ran on the browser's one thread, for the rest of the session,
/// because nothing ever started another worker. Here the page says it expects
/// one, so the job dispatched in the gap is held; the replacement attaches, and
/// that job goes to it rather than to the frame.
///
/// The page half of this — `rustdar-web`'s `worker_port::lose` and its backoff
/// ladder — is `web_sys` and cannot be walked on a host; what a host can walk is
/// the funnel contract those two calls rely on, which is what this asserts.
#[test]
fn a_job_dispatched_between_two_workers_reaches_the_second_one() {
    let _fixture = fixture();
    let first = attach(true);
    let (tx, rx) = mpsc::channel();
    dispatch(1, &tx);
    assert_eq!(numbers(&first), vec![1], "the first worker took it");

    // `onerror`: the worker is gone, and the job it was carrying is failed
    // rather than forgotten — the pane re-asks.
    abandon_worker("the worker reported an error");
    assert_eq!(
        rx.try_recv(),
        Ok(1),
        "a job in flight when the worker was lost must be failed, not stranded",
    );
    assert!(!worker_attached());

    // The page schedules a replacement and says so. The next job waits for it
    // instead of paying for it.
    expect_sink(AMPLE);
    dispatch(2, &tx);
    assert_eq!(
        jobs_waiting_for_sink(),
        1,
        "the job after the loss must be held for the replacement",
    );
    assert!(
        rx.try_recv().is_err(),
        "the job after the loss must not have run on this thread",
    );

    let second = attach(true);
    assert_eq!(
        numbers(&second),
        vec![2],
        "the replacement must receive the job that waited for it",
    );
    assert_eq!(
        numbers(&first),
        vec![1],
        "the dead worker must not be handed anything",
    );
}

// ── The native path ──────────────────────────────────────────────────────────

/// **Native never queues.**
///
/// The queue exists on every target, because the `cfg` here is over routing;
/// what keeps a native build out of it is that `default_sink` is the job pool,
/// so a native thread has a sink from its first job and [`expect_sink`] refuses
/// to arm a thread that has one. This is asserted on a **freshly spawned**
/// thread rather than on the harness's own, which any earlier test may have
/// left detached — a thread that starts here is the state a native process's
/// threads are actually in.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn a_native_thread_cannot_be_made_to_wait_for_a_sink() {
    std::thread::spawn(|| {
        assert!(
            worker_attached(),
            "a native thread starts with the pool as its sink",
        );

        // The call the browser's adapter makes on every attempt. Here it must
        // change nothing at all.
        expect_sink(AMPLE);
        assert!(
            !expecting_sink(),
            "a native thread must not arm a wait for a sink it already has",
        );

        let (tx, rx) = mpsc::channel();
        dispatch(1, &tx);
        assert_eq!(
            jobs_waiting_for_sink(),
            0,
            "a native job must reach the pool, never the queue",
        );
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(30)),
            Ok(1),
            "the pool must run the job and deliver",
        );
    })
    .join()
    .expect("the native thread's assertions");
}
