//! The deferred-drop contract, on the host.
//!
//! The queue these tests drive is the same one the browser feeds — the `cfg`
//! in [`discard`] is over routing, not over the queue — so everything here
//! except the pool test is a claim about what a wasm frame will observe. The
//! queue is thread-local and every test begins by emptying it, so a run under
//! `--test-threads=1`, where the harness reuses one thread, proves the same
//! things a parallel run does.

use super::*;
use std::sync::Arc;
// Gated with its users: every test that needs a channel is asserting *which
// thread* freed a payload, which only the native arm can be asked. See
// `DropSignal`.
#[cfg(not(target_arch = "wasm32"))]
use std::sync::mpsc;
use std::time::Duration;

/// Long enough that no test below is racing a real clock.
const AMPLE: Duration = Duration::from_secs(30);

/// Empty the queue without asserting on it, so a test starts from a known
/// state whatever ran before it on this thread.
fn empty_the_queue() {
    while drain_deferred_drops(AMPLE) > 0 {}
}

/// The drain spends its budget and stops, and successive calls empty the
/// queue — so a teardown of any size costs each frame a bounded walk rather
/// than one frame the whole thing.
#[test]
fn the_drain_is_bounded_per_call_and_empties_over_successive_calls() {
    empty_the_queue();
    for _ in 0..7 {
        defer_drop("test-discard", Box::new(vec![0u8; 16]));
    }
    // A zero budget is the tightest bound there is, and it still makes
    // progress: exactly one payload per call, which is the guarantee that
    // keeps a coarse clock or a mis-set constant from stalling the queue.
    for remaining in (0..7).rev() {
        assert_eq!(drain_deferred_drops(Duration::ZERO), 1);
        assert_eq!(has_deferred_drops(), remaining > 0);
    }
    assert_eq!(
        drain_deferred_drops(Duration::ZERO),
        0,
        "an empty queue frees nothing, minimum-one notwithstanding"
    );
}

/// An ample budget takes the whole queue in one call, which is what says the
/// bound above is the budget doing the work rather than a per-call limit of
/// one hiding in the loop.
#[test]
fn an_ample_budget_takes_the_whole_queue_at_once() {
    empty_the_queue();
    for _ in 0..7 {
        defer_drop("test-discard", Box::new(vec![0u8; 16]));
    }
    assert_eq!(drain_deferred_drops(AMPLE), 7);
    assert!(!has_deferred_drops());
}

/// The push holds the payload and the drain is where it dies. A push that
/// freed anything would be [`offload`]'s inline arm under another name — the
/// frame-thread free the queue exists to remove.
#[test]
fn a_payload_is_freed_by_the_drain_and_never_by_the_push() {
    empty_the_queue();
    let sentinel = Arc::new(());
    defer_drop("test-sentinel", Box::new(Arc::clone(&sentinel)));
    assert_eq!(
        Arc::strong_count(&sentinel),
        2,
        "the push must hold the payload, not free it"
    );
    assert!(has_deferred_drops());
    assert_eq!(drain_deferred_drops(AMPLE), 1);
    assert_eq!(
        Arc::strong_count(&sentinel),
        1,
        "the drain is where the payload dies"
    );
}

/// Oldest first, so no entry starves behind later pushes — the payload filed
/// first has waited the most frames and is the next to go.
#[test]
fn the_drain_retires_the_oldest_entry_first() {
    empty_the_queue();
    let first = Arc::new(());
    let second = Arc::new(());
    defer_drop("test-first", Box::new(Arc::clone(&first)));
    defer_drop("test-second", Box::new(Arc::clone(&second)));
    assert_eq!(drain_deferred_drops(Duration::ZERO), 1);
    assert_eq!(Arc::strong_count(&first), 1, "the oldest entry dies first");
    assert_eq!(
        Arc::strong_count(&second),
        2,
        "the newer entry waits its turn"
    );
    assert_eq!(drain_deferred_drops(Duration::ZERO), 1);
    assert_eq!(Arc::strong_count(&second), 1);
}

/// **The invariant `App::handle_redraw` reads.** A queue with anything in it
/// says so, and an empty one does not: the frame loop rests on
/// `ControlFlow::Wait` and asks for the next frame only while some term is
/// true, so a queue that answered `false` while holding payloads would stop
/// draining until the user touched the application.
#[test]
fn a_queue_holding_anything_says_so() {
    empty_the_queue();
    assert!(!has_deferred_drops(), "an emptied queue holds nothing");
    defer_drop("test-pending", Box::new(vec![0u8; 16]));
    assert!(
        has_deferred_drops(),
        "a queue holding a payload must say so, or the frame loop sleeps on it"
    );
    drain_deferred_drops(AMPLE);
    assert!(!has_deferred_drops());
}

/// **Why [`discard_each`] exists**, asserted on the queue: a collection handed
/// over whole is one payload the drain frees in a single turn, and the same
/// items handed over per item are payloads it can stop between.
///
/// Both halves are pushed with [`defer_drop`], which is the arm that queues.
/// Going through [`discard`] would prove nothing here — on this target it
/// routes to the pool — and the claim is about what the *browser* observes,
/// where a batch is the frame-long stall this module exists to remove wearing
/// the shape of the fix.
#[test]
fn a_collection_handed_over_whole_is_one_payload_and_per_item_is_many() {
    empty_the_queue();
    let whole: Vec<Arc<()>> = (0..4).map(|_| Arc::new(())).collect();
    let watched: Vec<Arc<()>> = whole.iter().map(Arc::clone).collect();
    defer_drop("test-batch", Box::new(whole));
    assert_eq!(
        drain_deferred_drops(Duration::ZERO),
        1,
        "a batch is one payload however many items it holds"
    );
    assert!(
        watched.iter().all(|item| Arc::strong_count(item) == 1),
        "one turn freed all four, which is the stall a batch hides"
    );

    // The same four, one payload apiece: now the drain stops between them.
    let per_item: Vec<Arc<()>> = (0..4).map(|_| Arc::new(())).collect();
    let watched: Vec<Arc<()>> = per_item.iter().map(Arc::clone).collect();
    for item in per_item {
        defer_drop("test-each", Box::new(item));
    }
    for freed in 1..=4 {
        assert_eq!(drain_deferred_drops(Duration::ZERO), 1);
        assert_eq!(
            watched
                .iter()
                .filter(|item| Arc::strong_count(item) == 1)
                .count(),
            freed,
            "the drain must retire one item per turn"
        );
    }
    assert!(!has_deferred_drops());
}

/// [`discard_each`] hands over **every** item, through whatever routing this
/// target has — the pool here, the queue in a browser.
///
/// The count is what matters: a `discard_each` that moved the iterator whole,
/// or stopped after the first, would be the batch the test above is about.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn discard_each_hands_over_every_item() {
    empty_the_queue();
    let (tx, rx) = mpsc::channel();
    let items: Vec<DropSignal> = (0..4).map(|_| DropSignal(tx.clone())).collect();
    drop(tx);

    discard_each("test-each", items);

    for n in 0..4 {
        rx.recv_timeout(std::time::Duration::from_secs(10))
            .unwrap_or_else(|e| panic!("only {n} of 4 items were handed over ({e})"));
    }
    assert!(!has_deferred_drops());
}

/// A guard whose `Drop` reports which thread it died on. `Sender` is `Send`,
/// so the guard satisfies [`discard`]'s bound the way every real payload does.
///
/// Gated with the two tests that use it. Both are native — a browser has one
/// thread, so "which thread freed it" is not a question that can be asked
/// there — and without the gate this is dead code in the wasm32
/// `--all-targets` build, which the release workflow runs precisely to catch
/// that.
#[cfg(not(target_arch = "wasm32"))]
struct DropSignal(mpsc::Sender<std::thread::ThreadId>);

#[cfg(not(target_arch = "wasm32"))]
impl Drop for DropSignal {
    fn drop(&mut self) {
        let _ = self.0.send(std::thread::current().id());
    }
}

/// Native [`discard`] frees on the pool's own lane — off the calling thread,
/// which in production is the frame — and files nothing in the queue.
///
/// Driven the way the funnel's own tests drive the pool: the real lanes,
/// waited on through a channel.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn a_native_discard_frees_on_the_pool_and_never_touches_the_queue() {
    empty_the_queue();
    let (tx, rx) = mpsc::channel();
    discard("test-discard", DropSignal(tx));
    let dropped_on = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the free lane must run the drop");
    assert_ne!(
        dropped_on,
        std::thread::current().id(),
        "the drop ran on the calling thread, which for a production discard is the frame"
    );
    assert!(
        !has_deferred_drops(),
        "a native discard must not file anything in this thread's queue"
    );
}

/// The free lane is **not** the opaque lane, which carries the overlay
/// rasterizations a pan is waiting on.
///
/// Asserted by thread name: the lanes are named at their spawn, so a discard
/// landing among the overlay workers is visible here rather than only as a
/// pan that lags after a site switch. A payload's own `Drop` is the one place
/// that can observe where it ran.
#[test]
#[cfg(not(target_arch = "wasm32"))]
fn a_native_discard_stays_out_of_the_lane_a_pan_is_waiting_on() {
    struct NameSignal(mpsc::Sender<String>);
    impl Drop for NameSignal {
        fn drop(&mut self) {
            let name = std::thread::current()
                .name()
                .unwrap_or_default()
                .to_string();
            let _ = self.0.send(name);
        }
    }

    let (tx, rx) = mpsc::channel();
    discard("test-lane", NameSignal(tx));
    let lane = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the free lane must run the drop");
    assert!(
        lane.starts_with("rd-free"),
        "a discard was freed on {lane:?}; the free lane exists so teardown \
         does not queue ahead of the overlay renders in rd-opaque",
    );
}
