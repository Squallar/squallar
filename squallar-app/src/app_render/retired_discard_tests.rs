//! **Where the retired overlay payloads are freed.**
//!
//! The whole point of the seam is that the free does not happen on the frame
//! thread, so the assertion is the thread the `Drop` ran on — the only place
//! that can observe it — and not that the call was made.

use std::sync::mpsc;

/// A payload whose `Drop` reports which thread it died on, in the shape the
/// drain carries: a `Box<dyn Any + Send>`, exactly as a layer hands one back.
struct DropSignal(mpsc::Sender<std::thread::ThreadId>);

impl Drop for DropSignal {
    fn drop(&mut self) {
        let _ = self.0.send(std::thread::current().id());
    }
}

/// The frame hands the batch over and the frees land somewhere else.
#[test]
fn the_retired_batch_is_freed_off_the_frame_thread() {
    let (tx, rx) = mpsc::channel();
    let batch: Vec<Box<dyn std::any::Any + Send>> = vec![
        Box::new(DropSignal(tx.clone())),
        Box::new(DropSignal(tx.clone())),
    ];
    drop(tx);

    let moved = crate::app::App::discard_retired_overlay_data(batch);
    assert_eq!(
        moved, 2,
        "the seam must report what it moved, not that it ran"
    );

    let frame_thread = std::thread::current().id();
    for _ in 0..2 {
        let dropped_on = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("every retired payload must be freed by the pool");
        assert_ne!(
            dropped_on, frame_thread,
            "a retired generation was freed on the frame thread, which is the \
             cost this seam exists to move",
        );
    }
}

/// An empty batch is the ordinary case — most frames retire nothing — and it
/// must cost nothing and say zero.
#[test]
fn an_empty_batch_moves_nothing() {
    assert_eq!(crate::app::App::discard_retired_overlay_data(Vec::new()), 0);
}
