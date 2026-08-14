//! What the pool has to be true about, as opposed to what it happens to do.
//!
//! Three claims, and all three are things the thread-per-job arm this replaced
//! could not make: the transport **moves** a request rather than copying it, a
//! lane runs no more than its thread count at a time, and a job that panics
//! answers instead of taking a worker with it.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

/// **The claim the whole convergence rests on: this arm copies nothing.**
///
/// The browser's transport serialises because a `postMessage` transfer list is
/// the only handover a browser has, and that shows up as
/// `JobRequest::to_bytes`. A copying result transport was measured at 58–75 ms
/// per raster at the 7362 px desktop ceiling — 31–45% on top of the render —
/// which is why "one path that can move-or-transfer" has to be a fact about
/// this implementation and not a claim in a comment.
///
/// Asserted by **allocation identity**, not by a timing: the `Arc` that came
/// out the far end of the channel points at the same heap block the caller put
/// in. A copy of any kind — an eager `to_bytes`, a `Clone` on the way through,
/// a structured clone — would fail this, and no amount of it being fast would
/// let it pass.
#[test]
fn the_native_transport_moves_a_request_rather_than_copying_it() {
    // Big enough that a copy would be a real cost, not merely a different
    // pointer: this is the shape of a Level II archive on its way to a decode.
    let archive = std::sync::Arc::new(vec![7u8; 4 << 20]);
    let sent_to = std::sync::Arc::as_ptr(&archive);
    let bytes_at = archive.as_ptr();

    let (tx, rx) = mpsc::channel();
    let (interactive, _keep_alive) = mpsc::channel();
    let handle = Handle {
        described: tx,
        interactive,
    };
    handle
        .send(
            17,
            JobRequest::Decode {
                archive: std::sync::Arc::clone(&archive),
            },
        )
        .expect("the receiver is alive");

    let (id, arrived) = rx.recv().expect("the request crossed");
    assert_eq!(id, 17, "the id travels with the request, not beside it");
    let JobRequest::Decode { archive: arrived } = arrived else {
        panic!("the transport changed the request into something else");
    };
    assert!(
        std::ptr::eq(std::sync::Arc::as_ptr(&arrived), sent_to),
        "the request was copied across the transport, not moved",
    );
    assert!(
        std::ptr::eq(arrived.as_ptr(), bytes_at),
        "the payload was reallocated, so something copied 4 MiB of it",
    );
}

/// The refusal contract, which is what lets the funnel drop its own copy of the
/// request: a send that cannot land hands the job back untouched.
///
/// Not a hypothetical — it is the only `Err` this transport can produce, and
/// the funnel's answer to it is to run the job on the calling thread. A
/// refusal that swallowed the request would leave a pane holding a render slot
/// nothing will ever release.
#[test]
fn a_refusal_hands_the_request_back_whole() {
    let archive = std::sync::Arc::new(vec![3u8; 64]);
    let sent_to = std::sync::Arc::as_ptr(&archive);

    let (tx, rx) = mpsc::channel::<(u64, JobRequest)>();
    drop(rx);
    let (interactive, _keep_alive) = mpsc::channel();
    let handle = Handle {
        described: tx,
        interactive,
    };

    let refused = handle.send(
        1,
        JobRequest::Decode {
            archive: std::sync::Arc::clone(&archive),
        },
    );
    let Err(JobRequest::Decode { archive: back }) = refused else {
        panic!("a lane with no receiver must refuse, and must give the job back");
    };
    assert!(
        std::ptr::eq(std::sync::Arc::as_ptr(&back), sent_to),
        "the refused request came back as a copy",
    );
}

/// **Bounded concurrency**, which is the thing native did not have: `offload`
/// spawned a fresh OS thread per job with no ceiling of its own, so the only
/// bound was `render_dispatch`'s pre-admission check and anything that did not
/// go through it was unbounded.
///
/// Twenty tasks over two threads. The peak recorded inside the work is the
/// measurement; twenty is enough that an unbounded lane would have to be
/// unlucky twenty times over to look like a bounded one.
#[test]
fn a_lane_runs_no_more_than_its_thread_count_at_once() {
    static RUNNING: AtomicUsize = AtomicUsize::new(0);
    static PEAK: AtomicUsize = AtomicUsize::new(0);
    static DONE: AtomicUsize = AtomicUsize::new(0);

    fn work(_: ()) {
        let now = RUNNING.fetch_add(1, Ordering::SeqCst) + 1;
        PEAK.fetch_max(now, Ordering::SeqCst);
        // Long enough that two threads genuinely overlap, so a lane that ran
        // everything serially would still be caught by the `>= 2` below rather
        // than passing the ceiling vacuously.
        std::thread::sleep(std::time::Duration::from_millis(5));
        RUNNING.fetch_sub(1, Ordering::SeqCst);
        DONE.fetch_add(1, Ordering::SeqCst);
    }

    let (tx, rx) = mpsc::channel();
    lane("rd-test-bound", 2, rx, work);
    for _ in 0..20 {
        tx.send(()).expect("the lane is alive");
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while DONE.load(Ordering::SeqCst) < 20 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(DONE.load(Ordering::SeqCst), 20, "every task must run");
    let peak = PEAK.load(Ordering::SeqCst);
    assert!(peak <= 2, "a two-thread lane ran {peak} jobs at once");
    assert!(
        peak >= 2,
        "the lane never used its second thread, so the ceiling above proves nothing"
    );
}

/// **The failure path native did not have.**
///
/// A rasterizer that panicked used to take its whole thread down with the
/// job's `deliver` un-run, which left the pane's in-flight mark set and its
/// render slot taken for the rest of the session — a pane that goes blank and
/// never recovers. `None` is the same "nothing to draw" every other failure
/// already produces, and the caller's slot is released on the way through.
#[test]
fn a_job_that_panics_answers_with_nothing() {
    let quiet = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let panicked: Option<u8> = guarded("test", || panic!("the rasterizer gave up"));
    std::panic::set_hook(quiet);

    assert_eq!(panicked, None, "a panicking job must still answer");
    assert_eq!(
        guarded("test", || 5),
        Some(5),
        "a job that did not panic must answer with its value"
    );
}

/// And the worker survives it. A lane that lost a thread per panic would narrow
/// silently until nothing ran at all, which is the same wedge one job wide.
#[test]
fn a_panicking_task_does_not_narrow_the_lane() {
    static SEEN: AtomicUsize = AtomicUsize::new(0);

    fn work(explode: bool) {
        SEEN.fetch_add(1, Ordering::SeqCst);
        assert!(!explode, "on purpose");
    }

    let quiet = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));

    let (tx, rx) = mpsc::channel();
    // One thread, so a panic that killed its worker would strand everything
    // after it and the count below could never reach four.
    lane("rd-test-panic", 1, rx, work);
    for explode in [true, true, false, false] {
        tx.send(explode).expect("the lane is alive");
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while SEEN.load(Ordering::SeqCst) < 4 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    std::panic::set_hook(quiet);
    assert_eq!(
        SEEN.load(Ordering::SeqCst),
        4,
        "the lane's one worker did not survive a panicking task",
    );
}
