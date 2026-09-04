//! The tile lane: a second sink the worker's queue cannot occupy, on the host.
//! The registry these tests drive is the one a browser's lane files into.

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

/// What a `RecordingPort` was handed: job ids and the bytes, in order.
type Posted = Arc<Mutex<Vec<(u64, Vec<u8>)>>>;

/// A port that records what it was handed instead of posting anywhere, or
/// refuses everything.
struct RecordingPort {
    posted: Posted,
    accept: bool,
}

impl JobSink for RecordingPort {
    fn send(&self, id: u64, request: JobRequest) -> Result<(), JobRequest> {
        if !self.accept {
            return Err(request);
        }
        self.posted.lock().unwrap().push((id, request.to_bytes()));
        Ok(())
    }
}

fn recording(accept: bool) -> (Box<dyn JobSink>, Posted) {
    let posted: Posted = Arc::new(Mutex::new(Vec::new()));
    let port = RecordingPort {
        posted: Arc::clone(&posted),
        accept,
    };
    (Box::new(port), posted)
}

/// This thread's two sinks, both retired on the way out — and on the way in,
/// since the harness reuses threads.
struct Fixture;

impl Drop for Fixture {
    fn drop(&mut self) {
        abandon_lane("test teardown");
        abandon_worker("test teardown");
    }
}

fn fixture() -> Fixture {
    abandon_lane("test setup");
    abandon_worker("test setup");
    Fixture
}

/// A job that is cheap to describe and tells itself apart from its
/// neighbours by its last byte.
fn a_numbered_job(n: u8) -> JobRequest {
    let mut archive = b"AR2V0006.001not-a-real-volume".to_vec();
    archive.push(n);
    JobRequest::describe(
        squallar_radar::jobs::DecodeJob {
            archive: Arc::new(archive),
        },
        ceiling_only_geometry(0),
    )
}

/// Post to the lane, reporting on `delivered` if and when the answer comes.
fn post_to_lane(n: u8, delivered: &mpsc::Sender<(u8, bool)>) -> Option<u64> {
    let delivered = delivered.clone();
    offload_to_lane("test-lane", a_numbered_job(n), move |result| {
        let _ = delivered.send((n, result.is_some()));
    })
}

/// Dispatch to the worker, reporting on `delivered` when answered.
fn dispatch_to_worker(n: u8, delivered: &mpsc::Sender<(u8, bool)>) {
    let delivered = delivered.clone();
    offload_job(
        "test-lane-worker",
        Job::Described(a_numbered_job(n)),
        move |result| {
            let _ = delivered.send((n, result.is_some()));
        },
    );
}

/// **The rule the tile pump's gate now reads.** The worker's queue can hold
/// as much as it likes; the lane's count is the lane's alone. Before the
/// lane, the gate read `jobs_in_worker` and a busy scene staged nothing.
#[test]
fn the_lanes_count_ignores_what_the_worker_owes() {
    let _fixture = fixture();
    let (worker, _) = recording(true);
    set_worker(worker);
    let (lane, _) = recording(true);
    set_lane(lane);
    let (tx, _rx) = mpsc::channel();

    for n in 1..=3 {
        dispatch_to_worker(n, &tx);
    }
    assert_eq!(jobs_in_worker(), 3, "the worker owes the three it took");
    assert_eq!(
        jobs_in_lane(),
        0,
        "a busy worker must read as an idle lane; that is the whole seam"
    );

    let id = post_to_lane(9, &tx).expect("an attached lane takes the batch");
    assert_eq!(jobs_in_lane(), 1);
    assert_eq!(jobs_in_worker(), 3, "the lane's batch is not the worker's");

    deliver_job_reply(id, None);
    assert_eq!(jobs_in_lane(), 0, "the reply retires the lane's entry");
    assert_eq!(jobs_in_worker(), 3, "and touches nothing of the worker's");
}

/// No lane, no post — and, unlike the worker's fallthrough, no inline run:
/// `deliver` never runs, so the caller knows the batch is still its own.
#[test]
fn with_no_lane_nothing_is_posted_and_nothing_is_delivered() {
    let _fixture = fixture();
    assert!(!lane_attached());
    let ran = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&ran);
    let id = offload_to_lane("test-lane", a_numbered_job(1), move |_| {
        flag.store(true, Ordering::SeqCst);
    });
    assert_eq!(id, None, "there is nowhere to post");
    assert!(
        !ran.load(Ordering::SeqCst),
        "a lane post with no lane must not run the job here: the caller has a \
         cheaper inline path and this would be the slow arm chosen for it"
    );
    assert_eq!(jobs_in_lane(), 0);
}

/// A lane that refuses hands the batch back undelivered and leaves no entry
/// behind — the registry must not hold a slot for a job that never left.
#[test]
fn a_lane_that_refuses_hands_the_batch_back_undelivered() {
    let _fixture = fixture();
    let (lane, posted) = recording(false);
    set_lane(lane);
    let (tx, rx) = mpsc::channel();
    assert_eq!(post_to_lane(2, &tx), None);
    assert!(posted.lock().unwrap().is_empty());
    assert!(rx.try_recv().is_err(), "the deliver must not have run");
    assert_eq!(jobs_in_lane(), 0, "no entry may survive a refused send");
}

/// Retiring one installation fails its jobs and only its jobs, in both
/// directions.
#[test]
fn abandoning_the_lane_fails_its_jobs_and_none_of_the_workers() {
    let _fixture = fixture();
    let (worker, _) = recording(true);
    set_worker(worker);
    let (lane, _) = recording(true);
    set_lane(lane);
    let (tx, rx) = mpsc::channel();

    dispatch_to_worker(1, &tx);
    post_to_lane(2, &tx).expect("posted");
    post_to_lane(3, &tx).expect("posted");

    abandon_lane("test");
    assert!(!lane_attached());
    let mut failed: Vec<(u8, bool)> = rx.try_iter().collect();
    failed.sort_unstable();
    assert_eq!(
        failed,
        vec![(2, false), (3, false)],
        "the lane's two batches are failed with nothing, and the worker's job is untouched"
    );
    assert_eq!(jobs_in_worker(), 1);
    assert_eq!(jobs_in_lane(), 0);

    abandon_worker("test");
    assert_eq!(rx.try_iter().collect::<Vec<_>>(), vec![(1, false)]);
}

/// Replacing the lane retires the old one's jobs first, as `set_worker` does:
/// a job whose sink is swapped under it would otherwise sit in the registry
/// forever.
#[test]
fn a_replaced_lane_fails_what_it_still_owed() {
    let _fixture = fixture();
    let (lane, _) = recording(true);
    set_lane(lane);
    let (tx, rx) = mpsc::channel();
    post_to_lane(4, &tx).expect("posted");

    let (next, _) = recording(true);
    set_lane(next);
    assert_eq!(rx.try_recv(), Ok((4, false)));
    assert_eq!(jobs_in_lane(), 0);
    assert!(lane_attached(), "the replacement is installed");
}

/// **Parity.** The lane speaks the funnel's wire byte for byte: the same
/// request posted to either sink is the same `to_bytes`, so the lane's
/// nested Worker runs `execute_encoded` on exactly what the worker would have,
/// and every codec digest the worker suite pins covers the lane too.
#[test]
fn the_lane_speaks_the_funnels_wire_byte_for_byte() {
    let _fixture = fixture();
    let (worker, worker_posted) = recording(true);
    set_worker(worker);
    let (lane, lane_posted) = recording(true);
    set_lane(lane);
    let (tx, _rx) = mpsc::channel();

    dispatch_to_worker(7, &tx);
    post_to_lane(7, &tx).expect("posted");

    let via_worker = worker_posted.lock().unwrap()[0].1.clone();
    let via_lane = lane_posted.lock().unwrap()[0].1.clone();
    assert_eq!(via_worker, via_lane, "one wire, two sinks");
    assert_ne!(
        worker_posted.lock().unwrap()[0].0,
        lane_posted.lock().unwrap()[0].0,
        "and two ids, from the one id space"
    );
}

/// The one row that actually rides the lane, on both sinks.
///
/// The test above is over an arbitrary job; this is over the job the browser
/// posts. It pins the two things a reply depends on: the request encodes
/// identically whichever sink carries it, and both dispatches resolve the
/// SAME codec row — which is what `deliver_encoded_reply` decodes an answer
/// through, taken from `Pending::row` at dispatch and never from the reply's
/// own tag.
#[test]
fn a_basemap_tiles_batch_is_the_same_row_and_the_same_bytes_on_either_sink() {
    let _fixture = fixture();
    let (worker, worker_posted) = recording(true);
    set_worker(worker);
    let (lane, lane_posted) = recording(true);
    set_lane(lane);

    let request = || {
        JobRequest::describe(
            squallar_basemap::jobs::BasemapTilesJob {
                style: squallar_basemap::jobs::StyleKey {
                    is_dark: true,
                    disabled: ["housenumber".to_string()].into_iter().collect(),
                },
                tiles: vec![squallar_basemap::jobs::TileBody {
                    z: 12,
                    x: 954,
                    y: 1600,
                    mvt: Arc::new(vec![0x1a, 0x2b, 0x3c, 0x4d]),
                }],
            },
            ceiling_only_geometry(0),
        )
    };

    assert_eq!(
        row_for(&request().job).label,
        "basemap/tiles",
        "the lane's only row is not the one this test posts"
    );

    offload_job("worker-basemap", Job::Described(request()), |_| {});
    offload_to_lane("lane-basemap", request(), |_| {}).expect("the lane took the batch");

    assert_eq!(
        worker_posted.lock().unwrap()[0].1,
        lane_posted.lock().unwrap()[0].1,
        "a batch encodes differently depending on which sink carries it; the \
         lane's nested Worker would then run `execute_encoded` on bytes the \
         worker never sees, and no digest suite covers those",
    );
}
